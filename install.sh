#!/bin/bash
# Installs Shotput for Linux for the current user: the editor and `shotput` command, its D-Bus
# activation file, the GNOME Shell extension, and a local Tesseract for "Copy text".
# Needs no root. Run it again to update.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
UUID=shotput@shotput.github.io
APP=io.github.shotput.Shotput
BIN_DIR="$HOME/.local/bin"
DATA="${XDG_DATA_HOME:-$HOME/.local/share}"
EXT_DIR="$DATA/gnome-shell/extensions/$UUID"

step() { printf '\033[1m==> %s\033[0m\n' "$*"; }

step "Building the editor"
if ! pkg-config --exists gtk4 libadwaita-1 2>/dev/null; then
  export PKG_CONFIG_PATH="$("$HERE/scripts/gtk-pkgconfig-shims.sh")${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
fi
( cd "$HERE/editor" && cargo build --release --quiet )

step "Installing the shotput command"
mkdir -p "$BIN_DIR"
install -m 755 "$HERE/editor/target/release/shotput" "$BIN_DIR/shotput.new"
mv -f "$BIN_DIR/shotput.new" "$BIN_DIR/shotput"

step "Installing the app entry, icon and D-Bus activation"
mkdir -p "$DATA/applications" "$DATA/icons/hicolor/scalable/apps" "$DATA/dbus-1/services"
sed "s#@BIN@#$BIN_DIR/shotput#g" "$HERE/data/$APP.desktop" > "$DATA/applications/$APP.desktop"
install -m 644 "$HERE/data/$APP.svg" "$DATA/icons/hicolor/scalable/apps/$APP.svg"
cat > "$DATA/dbus-1/services/$APP.service" <<SVC
[D-BUS Service]
Name=$APP
Exec=$BIN_DIR/shotput --gapplication-service
SVC
update-desktop-database -q "$DATA/applications" 2>/dev/null || true
xdg-mime default "$APP.desktop" x-scheme-handler/shotput 2>/dev/null || true
gtk-update-icon-cache -q -t "$DATA/icons/hicolor" 2>/dev/null || true

step "Installing the GNOME Shell extension"
rm -rf "$EXT_DIR.new"
cp -r "$HERE/extension/$UUID" "$EXT_DIR.new"
glib-compile-schemas "$EXT_DIR.new/schemas"
rm -rf "$EXT_DIR"
mv "$EXT_DIR.new" "$EXT_DIR"

step "Setting up text recognition"
"$HERE/scripts/install-tesseract.sh" "$DATA/shotput" || echo "   Tesseract could not be set up; Copy text will say so."

step "Enabling the extension"
enabled=$(gsettings get org.gnome.shell enabled-extensions)
if [[ "$enabled" != *"$UUID"* ]]; then
  if [[ "$enabled" == "@as []" || "$enabled" == "[]" ]]; then
    gsettings set org.gnome.shell enabled-extensions "['$UUID']"
  else
    gsettings set org.gnome.shell enabled-extensions "${enabled%]}, '$UUID']"
  fi
fi
gsettings set org.gnome.shell disable-user-extensions false
if gnome-extensions info "$UUID" >/dev/null 2>&1; then
  gnome-extensions enable "$UUID" 2>/dev/null || true
fi

# Restart a running editor so it picks up the new build.
pkill -f "$BIN_DIR/shotput --gapplication-service" 2>/dev/null || true

step "Done"
if gnome-extensions info "$UUID" 2>/dev/null | grep -q "State: ACTIVE"; then
  echo "Shotput is running."
else
  echo "Log out and back in once to start the Shotput extension. Until then, 'shotput area'"
  echo "and 'shotput annotate' work through GNOME's own screenshot picker."
fi
