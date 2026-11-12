#!/bin/bash
# Gives Shotput a working `tesseract` without root: uses the system one when present, otherwise
# downloads Fedora's tesseract packages with dnf (no install) and unpacks them into DEST.
# The wrapper lands in DEST/bin/tesseract.
set -euo pipefail
DEST=${1:?usage: install-tesseract.sh DEST}
mkdir -p "$DEST/bin"
wrapper="$DEST/bin/tesseract"

if command -v tesseract >/dev/null 2>&1 && [ "$(command -v tesseract)" != "$wrapper" ]; then
  ln -sf "$(command -v tesseract)" "$wrapper"
  echo "   Using the system Tesseract."
  exit 0
fi

if [ ! -x "$DEST/tesseract/usr/bin/tesseract" ]; then
  command -v dnf >/dev/null || { echo "   No dnf to fetch Tesseract with."; exit 1; }
  work=${SHOTPUT_RPM_CACHE:-${XDG_CACHE_HOME:-$HOME/.cache}/shotput/rpms}
  mkdir -p "$work"
  if ! ls "$work"/tesseract-[0-9]*.rpm "$work"/tesseract-libs-*.rpm >/dev/null 2>&1; then
    ( cd "$work" && timeout 180 dnf download -q --arch "$(uname -m)" tesseract tesseract-libs >/dev/null 2>&1 ) ||
      { echo "   Could not download Tesseract."; exit 1; }
  fi
  mkdir -p "$DEST/tesseract"
  for rpm in "$work"/*.rpm; do
    ( cd "$DEST/tesseract" && rpm2cpio "$rpm" | cpio -idm --quiet )
  done
fi

# Language data: the system's langpacks if installed, otherwise the official fast English model.
tessdata=/usr/share/tesseract/tessdata
if [ ! -f "$tessdata/eng.traineddata" ]; then
  tessdata="$DEST/tessdata"
  mkdir -p "$tessdata"
  [ -f "$tessdata/eng.traineddata" ] || curl -fsSL -o "$tessdata/eng.traineddata" \
    https://github.com/tesseract-ocr/tessdata_fast/raw/main/eng.traineddata
fi

cat > "$wrapper" <<SH
#!/bin/sh
export LD_LIBRARY_PATH="$DEST/tesseract/usr/lib64\${LD_LIBRARY_PATH:+:\$LD_LIBRARY_PATH}"
export TESSDATA_PREFIX="\${TESSDATA_PREFIX:-$tessdata}"
exec "$DEST/tesseract/usr/bin/tesseract" "\$@"
SH
chmod 755 "$wrapper"
"$wrapper" --version >/dev/null 2>&1 && echo "   Tesseract ready." || { echo "   Tesseract did not start."; exit 1; }
