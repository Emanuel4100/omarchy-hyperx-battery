# HyperX Pulsefire Haste 2 — Battery Indicator

Battery level, charging status, and a low-battery alert for the **HyperX Pulsefire Haste 2 Wireless** mouse, displayed in the desktop bar.

An Omarchy 4 bar-widget plugin (`BarWidget.qml`) backed by a small Rust daemon (`daemon/`) that reads the mouse over HID.

## How It Works

A small **Rust daemon** polls the mouse over HID (`/dev/hidrawN`) every **15 seconds** (configurable) and writes the result to a JSON file. The bar widget just reads that file: a bar icon showing the same battery glyph ramp as Omarchy's own power panel (🔋/⚡ style, 10 steps) plus the percentage, colored `Color.urgent` at/below the configured low-battery threshold, `Color.muted` when the reading is stale or the daemon has gone silent. Tooltip shows charging state and, if the daemon can't read the mouse for a long time, why.

### HID Protocol

The daemon sends a 64-byte request (`0x50 0x02`) and reads the **active poll** reply: header `0x51 0x02`, **battery** at **byte +2** (0–100). **Charging** uses **byte +3** (`0x01` / `0x00`) and, when present, **byte +4** as the passive-style power source (`0x00` = wired/charging, `0xFF` = wireless) — the daemon prefers +4 when those values appear.

On USB plug/unplug the mouse may send a **passive 11-byte** broadcast: **byte +6** = `0x03`, **battery** at **+1**, **power** at **+4** (same `0x00` / `0xFF` meaning). The daemon scans for that layout without requiring a specific byte +0.

1. **Active poll (request/response)** — 64-byte packet after `50 02`:

   | Offset from `51 02` | Meaning |
   |---------------------|---------|
   | +2 | Battery % |
   | +3 | `0x01` charging / `0x00` discharging (used if +4 is not `00`/`FF`) |
   | +4 | `0x00` wired power / `0xFF` wireless (preferred when set) |

2. **Passive broadcast** — 11 bytes on cable change:

   | Byte | Meaning |
   |------|---------|
   | 1 | Battery % |
   | 4 | `0x00` wired (charging) / `0xFF` wireless |
   | 6 | `0x03` signature |

The daemon keeps **last known** percent/charging when the mouse sleeps or hidraw is briefly unavailable. After a long period without fresh reads, a **manager_error** string is written; the widget surfaces it in its tooltip (the bar itself just stays on the last known % or shows "…").

### JSON State File

Written atomically to `$XDG_RUNTIME_DIR/hyperx-mouse-battery.json`:

```json
{"ok":true,"percent":97,"charging":false,"stale":false,"last_read_ok_at":1712856000,"updated":1712856060}
```

| Field | Type | Description |
|-------|------|-------------|
| `ok` | bool | `true` when a battery % is available to show (including cached) |
| `percent` | int \| null | Battery level 0–100 |
| `charging` | bool \| null | `true` when wired/charging |
| `stale` | bool | last poll failed but cached values are shown |
| `manager_error` | string \| null | Long-outage hint; shown in the widget's tooltip |
| `last_read_ok_at` | int \| null | Unix time of last successful HID read |
| `updated` | int | Unix timestamp of last daemon write |

A **low battery alarm** threshold (0 = off) is the widget's `lowBatteryThreshold` setting: `omarchy bar set emanuel.hyperx-battery lowBatteryThreshold <n>`.

## Requirements

- **Omarchy 4** (Hyprland + the Quickshell-based `omarchy-shell`)
- **Rust toolchain** (`cargo`) to build the daemon
- **HyperX Pulsefire Haste 2 Wireless** (USB HID ID `03F0:0F98`)

## Installation

```bash
omarchy plugin add https://github.com/Emanuel4100/omarchy-hyperx-battery.git
~/.config/omarchy/plugins/emanuel.hyperx-battery/install.sh
```

`install.sh`:

1. Builds the daemon (`daemon/`) in release mode (into `~/.cache/hyperx-mouse-battery`, not the plugin folder) and installs it to `~/.local/bin/`
2. Installs and starts the `hyperx-mouse-battery` systemd user service
3. Links `bin/hyperx-show-battery-state` into `~/.local/bin/` (prints the daemon's current state file)
4. Adds the widget to the bar's right section via `omarchy bar put`

> **Do not run with `sudo`.** Everything it touches (`~/.local`, `~/.config`, the user systemd instance) is per-user.

If the widget doesn't appear on the bar, force a plugin rescan: `omarchy-shell shell rescanPlugins`.

### udev Permissions

If the daemon reports "permission denied" on `/dev/hidrawN`, install the udev rule:

```bash
sudo cp packaging/udev/72-hyperx-pulsefire2-hidraw.rules /etc/udev/rules.d/
sudo udevadm control --reload-rules
sudo udevadm trigger --action=add --subsystem-match=hidraw
```

This grants access to the HyperX hidraw device for logged-in users without requiring root. The filename's `72-` prefix matters: systemd's `73-seat-late.rules` is what actually applies the ACL for a `TAG+="uaccess"` rule, and it only sees tags set by rule files that sort before it — a `99-...rules` file (an earlier name of this rule) sets the tag one pass too late, so `udevadm trigger` silently does nothing for an already-plugged-in device (a fresh unplug/replug would still work by luck, since that's a genuinely new `add` event through the full rule chain).

## Configuration

| Environment Variable | Default | Description |
|---------------------|---------|-------------|
| `HYPERX_POLL_SECS` | `15` | Seconds between battery polls |
| `HYPERX_POLL_ROUNDS` | `2` | `50 02` exchanges per poll (`1`–`5`); extra rounds refresh charging on some firmware |
| `HYPERX_MANAGER_ERROR_AFTER_SECS` | `21600` (6h) | After this many seconds without a successful read, `manager_error` is set (minimum override `60`) |

Edit the systemd unit to change:

```bash
systemctl --user edit hyperx-mouse-battery.service
```

```ini
[Service]
Environment=HYPERX_POLL_SECS=30
Environment=HYPERX_MANAGER_ERROR_AFTER_SECS=7200
```

## Files

```
.
├── manifest.json                  # Plugin id, bar-widget metadata, settings schema
├── BarWidget.qml                  # Reads the daemon's JSON state file, renders the bar icon
├── install.sh                     # Build + install daemon and systemd unit, add the widget to the bar
├── bin/
│   └── hyperx-show-battery-state  # Pretty-print the daemon's state file
├── daemon/
│   ├── Cargo.toml
│   └── src/main.rs                # HID polling daemon (Rust)
└── packaging/
    ├── systemd/
    │   └── hyperx-mouse-battery.service.in
    └── udev/
        └── 72-hyperx-pulsefire2-hidraw.rules
```

## Uninstall

```bash
systemctl --user disable --now hyperx-mouse-battery.service
rm ~/.config/systemd/user/hyperx-mouse-battery.service
rm ~/.local/bin/hyperx-mouse-battery-daemon ~/.local/bin/hyperx-show-battery-state
rm -rf ~/.cache/hyperx-mouse-battery

omarchy plugin remove emanuel.hyperx-battery
# `omarchy bar` has no remove verb — delete the {"id": "emanuel.hyperx-battery"} entry
# from bar.layout.right in ~/.config/omarchy/shell.json (it hot-reloads on save)

```

## License

GPL-2.0-or-later. See `LICENSE`.
