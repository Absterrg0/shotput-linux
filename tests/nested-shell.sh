#!/bin/bash
# Starts a headless gnome-shell in its own D-Bus session and data folders, with Shotput and the
# test kit enabled. Prints nothing; the bus address lands in $T/bus. Stop it with `kill $(cat $T/pid)`.
set -e
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(dirname "$HERE")
T=${SHOTPUT_TEST_DIR:-/tmp/claude-1000/shotput-test}
rm -rf "$T"
mkdir -p "$T"/{config,data/gnome-shell/extensions,data/dbus-1/services,cache,runtime,home}
chmod 700 "$T/runtime"
ln -s "$ROOT/extension/shotput@shotput.github.io" "$T/data/gnome-shell/extensions/"
cp -r "$HERE/testkit@shotput.local" "$T/data/gnome-shell/extensions/"
cat > "$T/data/dbus-1/services/io.github.shotput.Shotput.service" <<SVC
[D-BUS Service]
Name=io.github.shotput.Shotput
Exec=$ROOT/editor/target/release/shotput --gapplication-service
SVC
export XDG_CONFIG_HOME="$T/config" XDG_DATA_HOME="$T/data" XDG_CACHE_HOME="$T/cache" XDG_RUNTIME_DIR="$T/runtime"
unset DISPLAY WAYLAND_DISPLAY
setsid dbus-run-session -- bash -c '
  echo "$DBUS_SESSION_BUS_ADDRESS" > "$0/bus"
  gsettings set org.gnome.shell enabled-extensions "${SHOTPUT_TEST_EXTENSIONS:-[\"shotput@shotput.github.io\", \"testkit@shotput.local\"]}"
  gsettings set org.gnome.shell disable-user-extensions false
  gsettings set org.gnome.shell welcome-dialog-last-shown-version "999"
  gsettings set org.gnome.desktop.interface color-scheme prefer-dark
  dbus-update-activation-environment WAYLAND_DISPLAY=shotput-test XDG_CONFIG_HOME="$XDG_CONFIG_HOME" XDG_DATA_HOME="$XDG_DATA_HOME" XDG_CACHE_HOME="$XDG_CACHE_HOME" XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" GDK_BACKEND=wayland
  monitors=""
  for m in ${SHOTPUT_TEST_MONITORS:-1280x800}; do monitors="$monitors --virtual-monitor $m"; done
  exec gnome-shell --headless $monitors --wayland-display shotput-test --no-x11
' "$T" > "$T/shell.log" 2>&1 &
echo $! > "$T/pid"
for i in $(seq 1 60); do
  [ -s "$T/bus" ] && DBUS_SESSION_BUS_ADDRESS=$(cat "$T/bus") gdbus call --session --dest org.gnome.Shell --object-path /org/gnome/Shell --method org.gnome.Shell.Eval '1' >/dev/null 2>&1 && break
  sleep 0.5
done
