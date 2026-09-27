use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use nix::poll::{poll, PollFd, PollFlags, PollTimeout};
use serde::Serialize;
use tempfile::NamedTempFile;

const HID_ID_SUB: &str = "000003F0:00000F98";
const HID_PHYS_SUB: &str = "input2";
const READ_CAP: usize = 128;
/// Drop stale hidraw frames plus any passive (plug/unplug) reports that arrived while we slept.
const MAX_DRAIN_BYTES: usize = 1024;
const REQUEST: [u8; 64] = {
    let mut b = [0u8; 64];
    b[0] = 0x50;
    b[1] = 0x02;
    b
};

const DEFAULT_POLL_SECS: u64 = 15;
const READ_TIMEOUT: Duration = Duration::from_secs(3);
const STATE_FILENAME: &str = "hyperx-mouse-battery.json";
/// After this many seconds without a successful read, expose `manager_error` for the
/// preferences UI (not the top bar). Override with `HYPERX_MANAGER_ERROR_AFTER_SECS`.
const DEFAULT_MANAGER_ERROR_AFTER_SECS: u64 = 6 * 3600;

#[derive(Serialize)]
struct State {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    percent: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    charging: Option<bool>,
    #[serde(skip_serializing_if = "is_false")]
    stale: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    manager_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_read_ok_at: Option<i64>,
    updated: i64,
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn runtime_state_path() -> PathBuf {
    let base = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| {
        let uid = nix::unistd::Uid::current().as_raw();
        format!("/run/user/{uid}")
    });
    PathBuf::from(base).join(STATE_FILENAME)
}

fn find_hidraw_path() -> Option<PathBuf> {
    let dir = fs::read_dir("/sys/class/hidraw").ok()?;
    let mut entries: Vec<_> = dir.flatten().collect();
    entries.sort_by_key(|e| e.file_name().to_string_lossy().into_owned());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("hidraw") {
            continue;
        }
        let uevent = entry.path().join("device/uevent");
        let content = fs::read_to_string(&uevent).ok()?;
        let mut ok_id = false;
        let mut ok_phys = false;
        for line in content.lines() {
            if line.starts_with("HID_ID=") && line.contains(HID_ID_SUB) {
                ok_id = true;
            }
            if line.starts_with("HID_PHYS=") && line.contains(HID_PHYS_SUB) {
                ok_phys = true;
            }
        }
        if ok_id && ok_phys {
            let n = name.trim_start_matches("hidraw");
            return Some(PathBuf::from(format!("/dev/hidraw{n}")));
        }
    }
    None
}

/// Pull everything currently readable (non-blocking) into `buf` — clears queued reports so the
/// next `50 02` exchange sees a fresh feature reply; also preserves passive events for parsing.
fn drain_hidraw_pending(file: &mut File, buf: &mut Vec<u8>) -> io::Result<()> {
    loop {
        if buf.len() >= MAX_DRAIN_BYTES {
            break;
        }
        let mut fds = [PollFd::new(file.as_fd(), PollFlags::POLLIN)];
        let ready = poll(&mut fds, PollTimeout::ZERO).map_err(|e| io::Error::other(e))?;
        if ready == 0 {
            break;
        }
        let mut chunk = [0u8; 64];
        match file.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Append one `50 02` request and up to [`READ_CAP`] **new** bytes of response.
fn append_exchange_response(file: &mut File, buf: &mut Vec<u8>) -> io::Result<()> {
    file.write_all(&REQUEST)?;
    let start_len = buf.len();
    let deadline = Instant::now() + READ_TIMEOUT;
    while buf.len().saturating_sub(start_len) < READ_CAP && Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let slice_ms = remaining.as_millis().min(1000) as u16;
        let timeout = PollTimeout::from(slice_ms.max(1));
        let mut fds = [PollFd::new(file.as_fd(), PollFlags::POLLIN)];
        match poll(&mut fds, timeout) {
            Ok(0) => break,
            Ok(_) => {
                let mut chunk = [0u8; 64];
                match file.read(&mut chunk) {
                    Ok(0) => std::thread::sleep(Duration::from_millis(10)),
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e),
                }
            }
            Err(e) => return Err(io::Error::other(e)),
        }
    }
    Ok(())
}

fn poll_rounds() -> u32 {
    std::env::var("HYPERX_POLL_ROUNDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&n| (1..=5).contains(&n))
        .unwrap_or(2)
}

/// Open hidraw fresh each cycle (same effect as restarting the service for device state),
/// drain queued input, then run several `50 02` exchanges so later responses refresh power flags.
fn read_hid_from_path(path: &Path) -> io::Result<Vec<u8>> {
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    let mut buf = Vec::new();
    drain_hidraw_pending(&mut file, &mut buf)?;
    let rounds = poll_rounds();
    for r in 0..rounds {
        if r > 0 {
            drain_hidraw_pending(&mut file, &mut buf)?;
        }
        append_exchange_response(&mut file, &mut buf)?;
    }
    Ok(buf)
}

/// Active poll: `0x51 0x02`, battery at +2, status bytes at +3 / +4.
/// +4: `0x00` / `0xFF` = wired / wireless when present. If +4 is something else (often
/// "ignore, varies"), +3 may be `0x01` (plugged), `0x02` (charging phase), `0x00` / `0xFE`
/// (discharging) — same semantics as the 11-byte event format.
fn charging_from_active_poll(b3: u8, b4: u8) -> bool {
    match b4 {
        0x00 => true,
        0xFF => false,
        _ => matches!(b3, 0x01 | 0x02),
    }
}

/// Legacy layout: `51 02` at byte 64 (battery at 66). The buffer may hold **multiple** `51 02`
/// frames after several hid reads; byte +3/+4 at offset 64 can stay stale while a **later** frame
/// has the real charging flag. Use the **last valid frame** for both battery and charging.
/// Passive 11-byte packets can still override charging in the caller when present.
fn parse_battery_packet(reply: &[u8]) -> Option<(u8, bool)> {
    const LEGACY_HDR: usize = 64;
    let end = reply.len().checked_sub(5)?;

    let mut last: Option<(u8, bool)> = None;

    for i in 0..=end {
        if reply[i] == 0x51 && reply[i + 1] == 0x02 {
            let bat = reply[i + 2];
            if bat > 100 {
                continue;
            }
            let charging = charging_from_active_poll(reply[i + 3], reply[i + 4]);
            last = Some((bat, charging));
        }
    }

    if last.is_some() {
        return last;
    }

    if reply.len() > LEGACY_HDR + 4
        && reply[LEGACY_HDR] == 0x51
        && reply[LEGACY_HDR + 1] == 0x02
    {
        let bat = reply[LEGACY_HDR + 2];
        if bat <= 100 {
            let charging =
                charging_from_active_poll(reply[LEGACY_HDR + 3], reply[LEGACY_HDR + 4]);
            return Some((bat, charging));
        }
    }
    None
}

/// Passive 11-byte broadcast (USB plug/unplug): byte 6 = `0x03`, battery at +1, power at +4.
fn parse_passive_charging(reply: &[u8]) -> Option<bool> {
    let end = reply.len().checked_sub(11)?;
    for i in 0..=end {
        if reply[i + 6] != 0x03 {
            continue;
        }
        let bat = reply[i + 1];
        if bat > 100 {
            continue;
        }
        let phase = reply[i + 3];
        match reply[i + 4] {
            0x00 => return Some(true),
            0xFF => return Some(false),
            _ => {
                if phase == 0x02 {
                    return Some(true);
                }
                if phase == 0xFE {
                    return Some(false);
                }
                continue;
            }
        }
    }
    None
}

fn write_state_atomic(path: &Path, state: &State) -> io::Result<()> {
    let json = serde_json::to_string(state).map_err(|e| io::Error::other(e))?;
    let dir = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "state path has no parent")
    })?;
    fs::create_dir_all(dir)?;
    let tmp = NamedTempFile::new_in(dir)?;
    tmp.as_file().write_all(json.as_bytes())?;
    tmp.as_file().sync_all()?;
    tmp.persist(path)?;
    Ok(())
}

fn poll_interval() -> Duration {
    let secs = std::env::var("HYPERX_POLL_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_POLL_SECS);
    Duration::from_secs(secs)
}

fn manager_error_after_secs() -> u64 {
    std::env::var("HYPERX_MANAGER_ERROR_AFTER_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|&n| n >= 60)
        .unwrap_or(DEFAULT_MANAGER_ERROR_AFTER_SECS)
}

fn build_manager_error(
    now: i64,
    daemon_started: i64,
    last_read_ok_at: Option<i64>,
    cached_percent: Option<u8>,
    after_secs: u64,
) -> Option<String> {
    let overdue = |t: i64| now.saturating_sub(t) >= after_secs as i64;

    match last_read_ok_at {
        Some(t) if overdue(t) => {
            let hrs = after_secs / 3600;
            let pct = cached_percent
                .map(|p| format!(" Last known: {p}%."))
                .unwrap_or_default();
            Some(format!(
                "No fresh battery reading for over {} hour(s).{pct} Wake the mouse or check the USB receiver.",
                hrs.max(1)
            ))
        }
        None if cached_percent.is_none() && overdue(daemon_started) => Some(
            "Still no battery data from the mouse. Check hidraw permissions (udev), \
             that the wireless dongle is connected, and that the user service is running."
                .into(),
        ),
        _ => None,
    }
}

fn main() {
    let state_path = runtime_state_path();
    let mut cached_path: Option<PathBuf> = None;
    let mut cached_percent: Option<u8> = None;
    let mut cached_charging: Option<bool> = None;
    let mut last_read_ok_at: Option<i64> = None;
    let daemon_started = libc_time_secs();
    let after_secs = manager_error_after_secs();

    loop {
        let path = cached_path
            .clone()
            .or_else(find_hidraw_path)
            .filter(|p| p.exists());

        let now = libc_time_secs();
        let mut stale = false;

        let read_outcome = match path.as_ref() {
            None => Err(io::Error::new(
                io::ErrorKind::NotFound,
                "HyperX hidraw device not found",
            )),
            Some(p) => match read_hid_from_path(p) {
                Ok(reply) => {
                    let passive = parse_passive_charging(&reply);
                    match parse_battery_packet(&reply) {
                        Some((bat, c_from_poll)) => {
                            let charging = match passive {
                                Some(true) => true,
                                Some(false) => false,
                                None => c_from_poll,
                            };
                            cached_percent = Some(bat);
                            cached_charging = Some(charging);
                            last_read_ok_at = Some(now);
                            cached_path = Some(p.clone());
                            Ok(())
                        }
                        None => {
                            if let Some(c) = passive {
                                cached_charging = Some(c);
                            }
                            cached_path = Some(p.clone());
                            Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "no 0x51 0x02 battery packet in HID reply",
                            ))
                        }
                    }
                }
                Err(e) => {
                    if e.kind() == io::ErrorKind::PermissionDenied {
                        eprintln!(
                            "hyperx-mouse-battery: permission denied on {p:?}; add a udev rule."
                        );
                    }
                    Err(e)
                }
            },
        };

        if read_outcome.is_err() {
            if cached_percent.is_some() {
                stale = true;
            } else if path.is_none() {
                cached_path = None;
            }
        }

        let manager_error = build_manager_error(
            now,
            daemon_started,
            last_read_ok_at,
            cached_percent,
            after_secs,
        );

        let (ok, percent, charging) = match cached_percent {
            Some(p) => (
                true,
                Some(p),
                Some(cached_charging.unwrap_or(false)),
            ),
            None => (false, None, None),
        };

        let state = State {
            ok,
            percent,
            charging,
            stale,
            manager_error,
            last_read_ok_at,
            updated: now,
        };

        if let Err(e) = write_state_atomic(&state_path, &state) {
            eprintln!("hyperx-mouse-battery: failed to write state: {e}");
        }

        let _ = read_outcome;

        std::thread::sleep(poll_interval());
    }
}

fn libc_time_secs() -> i64 {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    t.as_secs() as i64
}
