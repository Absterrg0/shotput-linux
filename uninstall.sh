#!/bin/bash
# Removes Shotput for Linux: the extension (giving GNOME its Print Screen keys back), the
# command, the app entry and Shotput's data. Your saved screenshots are not touched.
set -uo pipefail
UUID=shotput@shotput.github.io
APP=io.github.shotput.Shotput
DATA="${XDG_DATA_HOME:-$HOME/.local/share}"
EXT_DIR="$DATA/gnome-shell/extensions/$UUID"

echo "==> Turning the extension off"
# Turning it off gives GNOME's screenshot keys back.
gnome-extensions disable "$UUID" 2>/dev/null || true
sleep 1

# If the extension was not running, give the keys back from what it recorded.
if [ -d "$EXT_DIR/schemas" ]; then
  released=$(gsettings --schemadir "$EXT_DIR/schemas" get org.gnome.shell.extensions.shotput released-gnome-keys 2>/dev/null || echo "''")
  python3 - "$released" <<'PY' 2>/dev/null || true
import ast, json, subprocess, sys
raw = ast.literal_eval(sys.argv[1]) if sys.argv[1] else '{}'
for key, accels in json.loads(raw or '{}').items():
    current = ast.literal_eval(subprocess.run(['gsettings', 'get', 'org.gnome.shell.keybindings', key],
                                              capture_output=True, text=True).stdout.replace('@as ', '') or '[]')
    merged = current + [a for a in accels if a not in current]
    subprocess.run(['gsettings', 'set', 'org.gnome.shell.keybindings', key, str(merged)])
PY
fi

enabled=$(gsettings get org.gnome.shell enabled-extensions 2>/dev/null || echo "[]")
if [[ "$enabled" == *"$UUID"* ]]; then
  cleaned=$(echo "$enabled" | sed "s/, '$UUID'//; s/'$UUID', //; s/'$UUID'//")
  gsettings set org.gnome.shell enabled-extensions "$cleaned"
fi

echo "==> Removing files"
pkill -f "$HOME/.local/bin/shotput --gapplication-service" 2>/dev/null || true
rm -rf "$EXT_DIR" "$DATA/shotput" "${XDG_CACHE_HOME:-$HOME/.cache}/shotput"
rm -f "$HOME/.local/bin/shotput" "$DATA/applications/$APP.desktop" \
  "$DATA/icons/hicolor/scalable/apps/$APP.svg" "$DATA/dbus-1/services/$APP.service"
update-desktop-database -q "$DATA/applications" 2>/dev/null || true
dconf reset -f /org/gnome/shell/extensions/shotput/ 2>/dev/null || true

echo "==> Done. Shotput is removed; log out and back in to unload it from GNOME Shell."
