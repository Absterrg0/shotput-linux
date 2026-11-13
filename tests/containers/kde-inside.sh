#!/bin/bash
# Runs inside the KDE test container: KWin headless, Shotput installed the way the installer
# does it (app entry with KWin's screenshot permission, schemas, D-Bus activation), started in
# the background as at login.
set -e
SIZE=${1:-1280x800}
if [ "${2:-}" != "--session" ]; then
  export XDG_RUNTIME_DIR=/tmp/rt
  mkdir -p -m 700 "$XDG_RUNTIME_DIR"
  exec dbus-run-session -- "$0" "$SIZE" --session
fi
echo "$DBUS_SESSION_BUS_ADDRESS" > /tmp/bus
export DBUS_SYSTEM_BUS_ADDRESS="$DBUS_SESSION_BUS_ADDRESS"
export XDG_CURRENT_DESKTOP=KDE XDG_SESSION_TYPE=wayland SHOTPUT_TEST=1 SHOTPUT_DEBUG=1
# GTK hands KWin shared-memory buffers; GPU buffers do not cross the container boundary.
export GSK_RENDERER=cairo GDK_DISABLE=dmabuf

data=~/.local/share
mkdir -p $data/applications $data/dbus-1/services $data/gnome-shell/extensions
cp -r /src/extension/shotput@shotput.github.io $data/gnome-shell/extensions/
glib-compile-schemas $data/gnome-shell/extensions/shotput@shotput.github.io/schemas
sed "s#@BIN@#/usr/local/bin/shotput#g" /src/data/io.github.shotput.Shotput.desktop > $data/applications/io.github.shotput.Shotput.desktop
printf '[D-BUS Service]\nName=io.github.shotput.Shotput\nExec=/usr/local/bin/shotput --gapplication-service\n' > $data/dbus-1/services/io.github.shotput.Shotput.service
update-desktop-database -q $data/applications || true
dbus-update-activation-environment XDG_CURRENT_DESKTOP XDG_SESSION_TYPE SHOTPUT_TEST XDG_RUNTIME_DIR DBUS_SYSTEM_BUS_ADDRESS

w=${SIZE%x*}; h=${SIZE#*x}
QT_FORCE_STDERR_LOGGING=1 QT_LOGGING_RULES="${KWIN_LOG:-}" ${KWIN_WRAP:-} kwin_wayland --virtual --width "$w" --height "$h" --socket wl-kde --no-lockscreen > /tmp/kwin.log 2>&1 &
for i in $(seq 1 60); do [ -S "$XDG_RUNTIME_DIR/wl-kde" ] && break; sleep 0.5; done
export WAYLAND_DISPLAY=wl-kde QT_QPA_PLATFORM=wayland
dbus-update-activation-environment WAYLAND_DISPLAY QT_QPA_PLATFORM
sleep 2
[ -z "${SHOTPUT_NO_START:-}" ] && /usr/local/bin/shotput --background > /tmp/shotput.log 2>&1 &
wait
