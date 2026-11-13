#!/bin/bash
# Builds a release download: dist/shotput-linux-ARCH.tar.gz with the prebuilt editor, the
# extension, the app entry and the installer. Used by the release workflow.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
ARCH=$(uname -m)
NAME=shotput-linux-$ARCH
OUT="$ROOT/dist/$NAME"

( cd "$ROOT/editor" && cargo build --release --locked )
rm -rf "$OUT" && mkdir -p "$OUT/bin" "$OUT/scripts"
install -m 755 "$ROOT/editor/target/release/shotput" "$OUT/bin/shotput"
cp -r "$ROOT/extension" "$ROOT/data" "$OUT/"
rm -f "$OUT/extension/"*/schemas/gschemas.compiled
install -m 755 "$ROOT/install.sh" "$ROOT/uninstall.sh" "$OUT/"
install -m 755 "$ROOT/scripts/install-tesseract.sh" "$OUT/scripts/"
cp "$ROOT/LICENSE" "$ROOT/README.md" "$OUT/"
tar -C "$ROOT/dist" -czf "$ROOT/dist/$NAME.tar.gz" "$NAME"

# The extension on its own, in the zip format `gnome-extensions install` takes.
( cd "$ROOT/extension/shotput@shotput.github.io" && \
  zip -qr "$ROOT/dist/shotput@shotput.github.io.shell-extension.zip" . -x 'schemas/gschemas.compiled' )
( cd "$ROOT/dist" && sha256sum "$NAME.tar.gz" > "$NAME.tar.gz.sha256" )
echo "$ROOT/dist/$NAME.tar.gz"
