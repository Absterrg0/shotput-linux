#!/bin/bash
# Runs JavaScript in the nested shell and prints the result.
T=${SHOTPUT_TEST_DIR:-/tmp/claude-1000/shotput-test}
DBUS_SESSION_BUS_ADDRESS=$(cat "$T/bus") gdbus call --session --dest org.gnome.Shell --object-path /org/gnome/Shell --method org.gnome.Shell.Eval "$1"
