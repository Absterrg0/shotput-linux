#!/bin/bash
# Starts the KDE test container with a Shotput binary: tests/containers/kde-run.sh PATH/TO/shotput
set -e
BIN=${1:?usage: kde-run.sh PATH/TO/shotput [WxH]}
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
SHOTS=/tmp/claude-1000/shotput-shots/ct
mkdir -p "$SHOTS" && chmod 777 "$SHOTS"
podman rm -f shotput-kde >/dev/null 2>&1 || true
# KWin carries the CAP_SYS_NICE file capability; a rootless container must allow it.
podman run -d --name shotput-kde --cap-add SYS_NICE ${KDE_GPU:+--device /dev/dri --group-add keep-groups} -v "$ROOT:/src:ro,z" -v "$SHOTS:$SHOTS:z" -v "$BIN:/usr/local/bin/shotput:ro,z" \
  ${SHOTPUT_NO_START:+-e SHOTPUT_NO_START=1} ${KWIN_WRAP:+-e KWIN_WRAP} ${KWIN_LOG:+-e KWIN_LOG} shotput-test:${KDE_IMAGE:-kde} /src/tests/containers/kde-inside.sh "${2:-1280x800}" >/dev/null
for i in $(seq 1 60); do
  podman exec shotput-kde test -S /tmp/rt/wl-kde 2>/dev/null && break
  sleep 1
done
sleep 4
echo ready
