import QtQuick
import Quickshell
import Quickshell.Io
import qs.Commons
import qs.Ui

// HyperX Pulsefire Haste 2 Wireless mouse battery indicator.
//
// Reads the JSON state file written by hyperx-mouse-battery-daemon (see the
// daemon/ directory in this plugin's source project) rather than polling the
// mouse itself: the daemon already owns the HID request/response protocol,
// last-known-value caching, and staleness detection, so the widget's only
// job is to render whatever it last wrote.
BarWidget {
  id: root
  moduleName: "emanuel.hyperx-battery"

  readonly property string statePath: (Quickshell.env("XDG_RUNTIME_DIR") || "") + "/hyperx-mouse-battery.json"
  readonly property int lowBatteryThreshold: Math.max(0, Math.round(Number(root.setting("lowBatteryThreshold", 20)) || 0))

  property bool ok: false
  property int percent: -1
  property bool charging: false
  property bool stale: false
  property string managerError: ""
  property int updatedAt: 0
  property bool lowNotified: false
  // The daemon's own "stale" flag only covers a failed poll with a cached
  // value on hand. If the daemon itself stops writing (crashed, service
  // disabled), the JSON goes silent instead — catch that by age since
  // nothing in the file can flag its own absence.
  property bool daemonSilent: false

  // Same battery glyph ramp the built-in power panel uses (Model.js), so a
  // low mouse battery reads the same as a low laptop battery at a glance.
  readonly property var dischargeIcons: ["󰁺", "󰁻", "󰁼", "󰁽", "󰁾", "󰁿", "󰂀", "󰂁", "󰂂", "󰁹"]
  readonly property var chargeIcons: ["󰢜", "󰂆", "󰂇", "󰂈", "󰢝", "󰂉", "󰢞", "󰂊", "󰂋", "󰂅"]

  function batteryGlyph() {
    var idx = Math.max(0, Math.min(9, Math.floor(root.percent / 10)))
    if (root.percent >= 100) idx = 9
    return root.charging ? root.chargeIcons[idx] : root.dischargeIcons[idx]
  }

  // Nerd Font mdi-mouse (U+F037D) — a monochrome vector glyph from the same
  // font as the rest of the bar, so it inherits widgetColor() below. The
  // previous 🖱 emoji came from Noto Color Emoji, a fixed-color glyph that
  // ignores Text.color entirely, which is why it always rendered plain white
  // regardless of theme or low-battery state.
  readonly property string mouseGlyph: "󰍽"

  function widgetText() {
    if (!root.ok || root.percent < 0) return root.mouseGlyph + " …"
    return root.mouseGlyph + " " + root.batteryGlyph() + " " + root.percent + "%"
  }

  function widgetColor() {
    if (!root.ok || root.daemonSilent || root.stale) return Color.muted
    if (!root.charging && root.lowBatteryThreshold > 0 && root.percent >= 0 && root.percent <= root.lowBatteryThreshold)
      return Color.urgent
    return root.bar ? root.bar.barForeground : Color.foreground
  }

  function tooltip() {
    if (root.daemonSilent)
      return "HyperX mouse — daemon not responding (systemctl --user status hyperx-mouse-battery.service)"
    if (root.managerError.length > 0)
      return root.managerError
    if (!root.ok)
      return "HyperX mouse — waiting for the battery daemon…"
    var parts = [root.percent + "%"]
    if (root.charging) parts.push("charging")
    else if (root.stale) parts.push("last known reading")
    return "HyperX mouse — " + parts.join(", ")
  }

  function resetState() {
    root.ok = false
    root.percent = -1
    root.charging = false
    root.stale = false
    root.managerError = ""
    root.lowNotified = false
  }

  function recomputeDaemonSilent() {
    root.daemonSilent = root.updatedAt > 0 && (Date.now() / 1000 - root.updatedAt) > 90
  }

  function parseState(content) {
    try {
      var d = JSON.parse(String(content || ""))
      if (!d || typeof d !== "object") {
        root.resetState()
        return
      }
      root.ok = d.ok === true
      root.percent = (typeof d.percent === "number") ? Math.max(0, Math.min(100, Math.round(d.percent))) : -1
      root.charging = d.charging === true
      root.stale = d.stale === true
      root.managerError = typeof d.manager_error === "string" ? d.manager_error : ""
      root.updatedAt = typeof d.updated === "number" ? d.updated : root.updatedAt
      root.recomputeDaemonSilent()
      root.maybeNotifyLowBattery()
    } catch (e) {
      root.resetState()
    }
  }

  function maybeNotifyLowBattery() {
    if (root.lowBatteryThreshold <= 0 || !root.ok || root.stale || root.charging || root.percent < 0) {
      root.lowNotified = false
      return
    }
    if (root.percent > root.lowBatteryThreshold) {
      root.lowNotified = false
      return
    }
    if (root.lowNotified) return
    root.lowNotified = true
    Quickshell.execDetached([
      "notify-send", "-u", "critical", "-a", "HyperX Mouse Battery",
      "Mouse battery low", root.percent + "% remaining. Charge it or switch to USB."
    ])
  }

  FileView {
    id: stateFile
    path: root.statePath
    watchChanges: true
    printErrors: false
    onFileChanged: reload()
    onLoaded: root.parseState(text())
    onLoadFailed: root.resetState()
  }

  // Belt-and-suspenders against a missed inotify event: the daemon replaces
  // the file with an atomic rename on every write, which can occasionally
  // outrun a file watch. Re-read on the daemon's own poll cadence so the bar
  // can never get stuck on a stale read indefinitely.
  Timer {
    interval: 15000
    running: true
    repeat: true
    onTriggered: stateFile.reload()
  }

  // Recomputes daemonSilent even when no new JSON has arrived at all.
  Timer {
    interval: 5000
    running: true
    repeat: true
    triggeredOnStart: true
    onTriggered: root.recomputeDaemonSilent()
  }

  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  WidgetButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    text: root.widgetText()
    fontSize: Style.bar.iconFont
    foreground: root.widgetColor()
    pressable: false
    tooltipText: root.tooltip()
  }
}
