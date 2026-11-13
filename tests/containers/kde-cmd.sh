#!/bin/bash
# Runs a command in the KDE test session: tests/containers/kde-cmd.sh shotput area
podman exec shotput-kde bash -c 'export DBUS_SESSION_BUS_ADDRESS=$(cat /tmp/bus) XDG_RUNTIME_DIR=/tmp/rt WAYLAND_DISPLAY=wl-kde XDG_CURRENT_DESKTOP=KDE XDG_SESSION_TYPE=wayland SHOTPUT_TEST=1 QT_QPA_PLATFORM=wayland; "$@"' -- "$@"
