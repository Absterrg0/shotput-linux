#!/bin/bash
# Runs a command in the Sway test session, e.g. sway-cmd.sh swaymsg seat seat0 cursor set 400 300
podman exec shotput-sway bash -c 'export DBUS_SESSION_BUS_ADDRESS=$(cat /tmp/bus) XDG_RUNTIME_DIR=/tmp/rt XDG_CURRENT_DESKTOP=sway XDG_SESSION_TYPE=wayland SHOTPUT_TEST=1 $(cat /tmp/env); "$@"' -- "$@"
