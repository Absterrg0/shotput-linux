#!/bin/bash
# Starts the Sway test container with a Shotput binary: tests/containers/sway-run.sh PATH/TO/shotput
set -e
BIN=${1:?usage: sway-run.sh PATH/TO/shotput [WxH]}
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
SHOTS=/tmp/claude-1000/shotput-shots/ct
mkdir -p "$SHOTS" && chmod 777 "$SHOTS"
podman rm -f shotput-sway >/dev/null 2>&1 || true
podman run -d --name shotput-sway --cap-add SYS_NICE ${SHOTPUT_NO_LAYER:+-e SHOTPUT_NO_LAYER=1} -v "$ROOT:/src:ro,z" -v "$SHOTS:$SHOTS:z" -v "$BIN:/usr/local/bin/shotput:ro,z" \
  shotput-test:sway /src/tests/containers/sway-inside.sh "${2:-1280x800}" >/dev/null
for i in $(seq 1 60); do podman exec shotput-sway test -f /tmp/env 2>/dev/null && break; sleep 1; done
sleep 3
echo ready
