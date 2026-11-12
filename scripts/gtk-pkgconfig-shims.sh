#!/bin/bash
# When gtk4-devel / libadwaita-devel are not installed, writes minimal pkg-config files that point
# at the system's runtime libraries, so the Rust bindings can link without root. Prints the
# directory to add to PKG_CONFIG_PATH.
set -euo pipefail
DIR=${1:-${XDG_CACHE_HOME:-$HOME/.cache}/shotput-build/sys}
mkdir -p "$DIR/pkgconfig" "$DIR/lib"
lib() { ls /usr/lib64/$1 /usr/lib/x86_64-linux-gnu/$1 2>/dev/null | head -1; }
ver() { rpm -q --qf '%{VERSION}' "$1" 2>/dev/null | sed 's/\^.*//' || echo "$2"; }
ln -sf "$(lib 'libgtk-4.so.1')" "$DIR/lib/libgtk-4.so"
ln -sf "$(lib 'libadwaita-1.so.0')" "$DIR/lib/libadwaita-1.so"
ln -sf "$(lib 'libgraphene-1.0.so.0')" "$DIR/lib/libgraphene-1.0.so"
pc() { printf 'libdir=%s\nName: %s\nDescription: shim\nVersion: %s\nRequires: %s\nLibs: -L${libdir} -l%s\nCflags:\n' "$DIR/lib" "$1" "$2" "$3" "$4" > "$DIR/pkgconfig/$1.pc"; }
pkg-config --exists graphene-1.0 || pc graphene-1.0 "$(ver graphene 1.10.8)" "" graphene-1.0
pkg-config --exists graphene-gobject-1.0 || pc graphene-gobject-1.0 "$(ver graphene 1.10.8)" "graphene-1.0 gobject-2.0" graphene-1.0
pkg-config --exists gtk4 || pc gtk4 "$(ver gtk4 4.22.0)" "pango cairo cairo-gobject gdk-pixbuf-2.0 gio-2.0 graphene-gobject-1.0" gtk-4
pkg-config --exists libadwaita-1 || pc libadwaita-1 "$(ver libadwaita 1.9.0)" "gtk4 gio-2.0" adwaita-1
echo "$DIR/pkgconfig"
