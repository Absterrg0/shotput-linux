#!/bin/bash
# Runs JavaScript in the containerised GNOME Shell started by run.sh.
podman exec "shotput-$1" bash -c 'DBUS_SESSION_BUS_ADDRESS=$(cat /tmp/bus) gdbus call --session --dest org.gnome.Shell --object-path /org/gnome/Shell --method org.gnome.Shell.Eval "$1"' -- "$2"
