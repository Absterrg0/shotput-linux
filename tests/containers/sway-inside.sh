#!/bin/bash
# Runs inside the Sway test container: headless Sway with software rendering, and Shotput
# installed and started in the background, as at login.
set -e
SIZE=${1:-1280x800}
if [ "${2:-}" != "--session" ]; then
  export XDG_RUNTIME_DIR=/tmp/rt
  mkdir -p -m 700 "$XDG_RUNTIME_DIR"
  exec dbus-run-session -- "$0" "$SIZE" --session
fi
echo "$DBUS_SESSION_BUS_ADDRESS" > /tmp/bus
export DBUS_SYSTEM_BUS_ADDRESS="$DBUS_SESSION_BUS_ADDRESS"
export XDG_CURRENT_DESKTOP=sway XDG_SESSION_TYPE=wayland SHOTPUT_TEST=1 SHOTPUT_DEBUG=1
data=~/.local/share
mkdir -p $data/applications $data/dbus-1/services $data/gnome-shell/extensions
cp -r /src/extension/shotput@shotput.github.io $data/gnome-shell/extensions/
glib-compile-schemas $data/gnome-shell/extensions/shotput@shotput.github.io/schemas
sed "s#@BIN@#/usr/local/bin/shotput#g" /src/data/io.github.shotput.Shotput.desktop > $data/applications/io.github.shotput.Shotput.desktop
printf '[D-BUS Service]\nName=io.github.shotput.Shotput\nExec=/usr/local/bin/shotput --gapplication-service\n' > $data/dbus-1/services/io.github.shotput.Shotput.service

cat > /tmp/sway.conf <<CONF
output HEADLESS-1 resolution ${SIZE} position 0 0
default_border pixel 1
focus_follows_mouse no
CONF
WLR_BACKENDS=headless WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 WLR_HEADLESS_OUTPUTS=1 \
  sway -c /tmp/sway.conf > /tmp/sway.log 2>&1 &
for i in $(seq 1 60); do ls $XDG_RUNTIME_DIR/sway-ipc.*.sock >/dev/null 2>&1 && break; sleep 0.5; done
export SWAYSOCK=$(ls $XDG_RUNTIME_DIR/sway-ipc.*.sock | head -1)
export WAYLAND_DISPLAY=$(cd $XDG_RUNTIME_DIR && ls wayland-* | grep -v lock | head -1)
echo "SWAYSOCK=$SWAYSOCK WAYLAND_DISPLAY=$WAYLAND_DISPLAY" > /tmp/env
dbus-update-activation-environment XDG_CURRENT_DESKTOP XDG_SESSION_TYPE SHOTPUT_TEST SHOTPUT_DEBUG XDG_RUNTIME_DIR \
  DBUS_SYSTEM_BUS_ADDRESS SWAYSOCK WAYLAND_DISPLAY
${SHOTPUT_NO_LAYER:+env SHOTPUT_LAYER_SHELL=1} /usr/local/bin/shotput --background > /tmp/shotput.log 2>&1 &
wait
