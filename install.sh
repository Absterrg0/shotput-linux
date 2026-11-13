#!/bin/bash
# Installs Shotput for Linux for the current user: the editor and `shotput` command, its D-Bus
# activation file, the GNOME Shell extension, and Tesseract for "Copy text" when it can.
# Needs no root. Run it again to update.
#
# From a release download it installs the prebuilt `bin/shotput`; from a source checkout it
# builds the editor with cargo first.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
UUID=shotput@shotput.github.io
APP=io.github.shotput.Shotput
BIN_DIR="$HOME/.local/bin"
DATA="${XDG_DATA_HOME:-$HOME/.local/share}"
EXT_DIR="$DATA/gnome-shell/extensions/$UUID"
SHARE="$DATA/shotput"

step() { printf '\033[1m==> %s\033[0m\n' "$*"; }
note() { printf '    %s\n' "$*"; }

hint_packages() {
  # Names of GTK 4 and libadwaita packages on common distributions.
  if command -v apt-get >/dev/null; then note "sudo apt install libgtk-4-1 libadwaita-1-0"
  elif command -v dnf >/dev/null; then note "sudo dnf install gtk4 libadwaita"
  elif command -v pacman >/dev/null; then note "sudo pacman -S gtk4 libadwaita"
  elif command -v zypper >/dev/null; then note "sudo zypper install libgtk-4-1 libadwaita-1-0"
  fi
}

if [ -x "$HERE/bin/shotput" ]; then
  binary="$HERE/bin/shotput"
else
  step "Building the editor"
  command -v cargo >/dev/null || { echo "cargo is needed to build from source. Install Rust from https://rustup.rs, or use a release download."; exit 1; }
  if ! pkg-config --exists gtk4 libadwaita-1 2>/dev/null; then
    export PKG_CONFIG_PATH="$("$HERE/scripts/gtk-pkgconfig-shims.sh")${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
  fi
  ( cd "$HERE/editor" && cargo build --release --quiet )
  binary="$HERE/editor/target/release/shotput"
fi

if ldd "$binary" 2>/dev/null | grep -q "not found"; then
  echo "Shotput needs GTK 4.14 and libadwaita 1.5 or newer (GNOME 46 or later). Missing:"
  ldd "$binary" | grep "not found" | sed 's/^/    /'
  hint_packages
  exit 1
fi

step "Installing the shotput command"
mkdir -p "$BIN_DIR"
install -m 755 "$binary" "$BIN_DIR/shotput.new"
mv -f "$BIN_DIR/shotput.new" "$BIN_DIR/shotput"

step "Installing the app entry, icon and D-Bus activation"
mkdir -p "$DATA/applications" "$DATA/icons/hicolor/scalable/apps" "$DATA/dbus-1/services" "$SHARE"
sed "s#@BIN@#$BIN_DIR/shotput#g" "$HERE/data/$APP.desktop" > "$DATA/applications/$APP.desktop"
install -m 644 "$HERE/data/$APP.svg" "$DATA/icons/hicolor/scalable/apps/$APP.svg"
cat > "$DATA/dbus-1/services/$APP.service" <<SVC
[D-BUS Service]
Name=$APP
Exec=$BIN_DIR/shotput --gapplication-service
SVC
install -m 755 "$HERE/uninstall.sh" "$SHARE/uninstall.sh"
update-desktop-database -q "$DATA/applications" 2>/dev/null || true
xdg-mime default "$APP.desktop" x-scheme-handler/shotput 2>/dev/null || true
gtk-update-icon-cache -q -t "$DATA/icons/hicolor" 2>/dev/null || true

step "Installing the GNOME Shell extension"
old_version=$(gnome-extensions info "$UUID" 2>/dev/null | sed -n 's/^ *Version: *//p' || true)
was_active=$(gnome-extensions info "$UUID" 2>/dev/null | grep -c "State: ACTIVE" || true)
rm -rf "$EXT_DIR.new"
mkdir -p "$(dirname "$EXT_DIR")"
cp -r "$HERE/extension/$UUID" "$EXT_DIR.new"
glib-compile-schemas "$EXT_DIR.new/schemas"
rm -rf "$EXT_DIR"
mv "$EXT_DIR.new" "$EXT_DIR"
new_version=$(sed -n 's/.*"version": *\([0-9]*\).*/\1/p' "$EXT_DIR/metadata.json")

shell_major=$(gnome-shell --version 2>/dev/null | sed -n 's/GNOME Shell \([0-9]*\).*/\1/p' || true)
if [ -z "$shell_major" ]; then
  note "GNOME Shell was not found. The editor works, and captures go through your desktop's"
  note "screenshot portal instead of the extension."
elif [ "$shell_major" -lt 46 ] || [ "$shell_major" -gt 50 ]; then
  note "This is GNOME $shell_major; the extension is made for GNOME 46 to 50 and may not load."
fi

step "Setting up text recognition"
"$HERE/scripts/install-tesseract.sh" "$SHARE" || {
  note "Copy text needs Tesseract. Install it with your package manager, for example:"
  if command -v apt-get >/dev/null; then note "sudo apt install tesseract-ocr"
  elif command -v pacman >/dev/null; then note "sudo pacman -S tesseract tesseract-data-eng"
  elif command -v zypper >/dev/null; then note "sudo zypper install tesseract-ocr tesseract-ocr-traineddata-english"
  else note "sudo dnf install tesseract"; fi
}

step "Enabling the extension"
if command -v gsettings >/dev/null && gsettings list-schemas | grep -qx org.gnome.shell; then
  enabled=$(gsettings get org.gnome.shell enabled-extensions)
  if [[ "$enabled" != *"$UUID"* ]]; then
    if [[ "$enabled" == "@as []" || "$enabled" == "[]" ]]; then
      gsettings set org.gnome.shell enabled-extensions "['$UUID']"
    else
      gsettings set org.gnome.shell enabled-extensions "${enabled%]}, '$UUID']"
    fi
  fi
  gsettings set org.gnome.shell disable-user-extensions false
  gnome-extensions enable "$UUID" 2>/dev/null || true
fi

# Restart a running editor so it picks up the new build.
pkill -f "$BIN_DIR/shotput --gapplication-service" 2>/dev/null || true

step "Done"
if [ "$was_active" -gt 0 ] && [ "$old_version" = "$new_version" ]; then
  echo "Shotput is running."
elif [ "$was_active" -gt 0 ]; then
  echo "Shotput is updated. Log out and back in to load the new extension; the editor is already new."
else
  echo "Log out and back in once to start Shotput, then press Print Screen and drag an area."
  echo "Until then, 'shotput area' works through GNOME's own screenshot picker."
fi
case ":$PATH:" in *":$BIN_DIR:"*) ;; *) note "Add $BIN_DIR to your PATH to use the shotput command.";; esac
