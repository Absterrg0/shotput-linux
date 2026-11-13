#!/bin/bash
# Runs inside the test container: installs the extension and starts GNOME Shell headless.
set -e
SIZE=${1:-1280x800}
ext=~/.local/share/gnome-shell/extensions
mkdir -p "$ext" ~/.local/share/dbus-1/services
cp -r /src/extension/shotput@shotput.github.io /src/tests/testkit@shotput.local "$ext/"
glib-compile-schemas "$ext/shotput@shotput.github.io/schemas"
if [ -x /usr/local/bin/shotput ]; then
  printf '[D-BUS Service]\nName=io.github.shotput.Shotput\nExec=/usr/local/bin/shotput --gapplication-service\n' \
    > ~/.local/share/dbus-1/services/io.github.shotput.Shotput.service
fi
export XDG_RUNTIME_DIR=/tmp/rt
mkdir -p -m 700 "$XDG_RUNTIME_DIR"
if [ "${2:-}" != "--session" ]; then
  exec dbus-run-session -- "$0" "$SIZE" --session
fi
echo "$DBUS_SESSION_BUS_ADDRESS" > /tmp/bus
# No system bus in a container; GNOME Shell only needs something to connect to.
export DBUS_SYSTEM_BUS_ADDRESS="$DBUS_SESSION_BUS_ADDRESS"
gsettings set org.gnome.shell enabled-extensions "['shotput@shotput.github.io', 'testkit@shotput.local']"
gsettings set org.gnome.shell disable-user-extensions false
gsettings set org.gnome.shell welcome-dialog-last-shown-version 999
dbus-update-activation-environment WAYLAND_DISPLAY=wl-test XDG_RUNTIME_DIR=/tmp/rt GDK_BACKEND=wayland
exec gnome-shell --headless --virtual-monitor "$SIZE" --wayland-display wl-test --no-x11 > /tmp/shell.log 2>&1
