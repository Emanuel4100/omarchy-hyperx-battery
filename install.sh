#!/usr/bin/env bash
# Builds and installs the hyperx-mouse-battery daemon + systemd user service
# that this bar widget reads from, and puts the widget on the bar.
# Run from the plugin folder after `omarchy plugin add`.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PLUGIN_ID="emanuel.hyperx-battery"

if [ "$(id -u)" -eq 0 ]; then
  echo "install.sh: do not run as root or sudo." >&2
  echo "Run: ./install.sh   (as your normal login user; cargo must be on your PATH.)" >&2
  exit 1
fi

# Build outside the plugin folder so no target/ tree ends up next to the QML.
export CARGO_TARGET_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/hyperx-mouse-battery"
cargo build --release --manifest-path "$ROOT/daemon/Cargo.toml"

install -d "$HOME/.local/bin"
install -m0755 "$CARGO_TARGET_DIR/release/hyperx-mouse-battery-daemon" "$HOME/.local/bin/hyperx-mouse-battery-daemon"
ln -sf "$ROOT/bin/hyperx-show-battery-state" "$HOME/.local/bin/hyperx-show-battery-state"

UNIT_DIR="$HOME/.config/systemd/user"
install -d "$UNIT_DIR"
sed "s|@BIN@|$HOME/.local/bin/hyperx-mouse-battery-daemon|g" \
  "$ROOT/packaging/systemd/hyperx-mouse-battery.service.in" \
  >"$UNIT_DIR/hyperx-mouse-battery.service"
systemctl --user daemon-reload
systemctl --user enable --now hyperx-mouse-battery.service
# Pick up a rebuilt binary if the service was already running.
systemctl --user restart hyperx-mouse-battery.service

if command -v omarchy >/dev/null 2>&1; then
  omarchy bar put "$PLUGIN_ID" --before omarchy.power 2>/dev/null || true
fi

echo "Installed the daemon service and added $PLUGIN_ID to the bar."
echo ""
echo "If it isn't showing:"
echo "  omarchy bar put $PLUGIN_ID --before omarchy.power"
echo "  omarchy-shell shell rescanPlugins"
echo ""
echo "If the daemon reports 'permission denied' on /dev/hidrawN (check: systemctl --user status hyperx-mouse-battery.service),"
echo "install the udev rule so your user can read the mouse's hidraw device without root:"
echo "  sudo cp $ROOT/packaging/udev/72-hyperx-pulsefire2-hidraw.rules /etc/udev/rules.d/"
echo "  sudo udevadm control --reload-rules"
echo "  sudo udevadm trigger --action=add --subsystem-match=hidraw"
echo "(The 72- prefix matters: it must sort before systemd's 73-seat-late.rules, which is what"
echo " actually applies the ACL for TAG+=\"uaccess\" — a later-numbered rule sets the tag too late.)"
