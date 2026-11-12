#!/bin/bash
# Restarts the nested shell so it reloads the extension's modules.
T=${SHOTPUT_TEST_DIR:-/tmp/claude-1000/shotput-test}
[ -f "$T/pid" ] && kill -- -$(ps -o pgid= $(cat "$T/pid") | tr -d ' ') 2>/dev/null
sleep 1
"$(dirname "$0")/nested-shell.sh"
for i in $(seq 1 20); do "$(dirname "$0")/sh-eval.sh" "$(cat "$(dirname "$0")/drive.js")" 2>/dev/null | grep -q true && break; sleep 0.5; done
"$(dirname "$0")/sh-eval.sh" 'Main.overview.hide(); "ready"'
