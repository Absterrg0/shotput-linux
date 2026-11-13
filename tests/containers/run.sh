#!/bin/bash
# Runs a headless GNOME Shell of another version in a container, with Shotput and the test kit.
#   tests/containers/run.sh gnome46 [WxH]     then   tests/containers/eval.sh gnome46 'JS'
# Build the image first: podman build -t shotput-test:gnome46 -f ubuntu-24.04.Containerfile .
set -e
TAG=${1:?usage: run.sh TAG [WxH]}
SIZE=${2:-1280x800}
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
SHOTS=/tmp/claude-1000/shotput-shots
mkdir -p "$SHOTS"
podman rm -f "shotput-$TAG" >/dev/null 2>&1 || true
podman run -d --name "shotput-$TAG" -v "$ROOT:/src:ro,z" -v "$SHOTS:$SHOTS:z" \
  ${SHOTPUT_BIN:+-v "$SHOTPUT_BIN:/usr/local/bin/shotput:ro,z"} \
  "shotput-test:$TAG" /src/tests/containers/inside.sh "$SIZE" >/dev/null
for i in $(seq 1 90); do
  "$(dirname "$0")/eval.sh" "$TAG" '1' 2>/dev/null | grep -q true && break
  sleep 1
done
"$(dirname "$0")/eval.sh" "$TAG" "$(cat "$(dirname "$0")/../drive.js")" >/dev/null
"$(dirname "$0")/eval.sh" "$TAG" 'Main.overview.hide(); "ready"'
