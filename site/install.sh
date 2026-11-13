#!/bin/bash
# Shotput for Linux installer: https://shotput.abstergo.fyi
#
#   curl -fsSL https://shotput.abstergo.fyi/install.sh | bash
#   curl -fsSL https://shotput.abstergo.fyi/install.sh | bash -s -- --uninstall
#
# Downloads the latest release for this machine from GitHub, checks its SHA-256, and runs its
# installer, which puts everything under your home folder. No root needed.
set -euo pipefail
REPO=Absterrg0/shotput-linux

if [ "${1:-}" = "--uninstall" ]; then
  uninstall="${XDG_DATA_HOME:-$HOME/.local/share}/shotput/uninstall.sh"
  [ -x "$uninstall" ] || { echo "Shotput is not installed."; exit 1; }
  exec "$uninstall"
fi

if [ "$(id -u)" = 0 ]; then
  echo "Run this as yourself, not root: Shotput installs into your home folder."
  exit 1
fi

case "$(uname -m)" in
  x86_64|amd64) arch=x86_64 ;;
  aarch64|arm64) arch=aarch64 ;;
  *) echo "Shotput for Linux has builds for x86_64 and arm64, not $(uname -m)."; exit 1 ;;
esac

for tool in curl tar sha256sum; do
  command -v "$tool" >/dev/null || { echo "Needs $tool."; exit 1; }
done

name="shotput-linux-$arch"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# This site keeps a copy of every release, for networks where GitHub's download host is slow or
# blocked; GitHub is the fallback.
fetch() {
  curl -fsSL --connect-timeout 10 --max-time 300 "$1/$name.tar.gz" -o "$tmp/$name.tar.gz" &&
    curl -fsSL --connect-timeout 10 --max-time 60 "$1/$name.tar.gz.sha256" -o "$tmp/$name.tar.gz.sha256"
}
printf '\033[1m==> Downloading Shotput for Linux (%s)\033[0m\n' "$arch"
fetch "https://shotput.abstergo.fyi/download" ||
  fetch "https://github.com/$REPO/releases/latest/download" ||
  { echo "Could not download Shotput. Check your connection, or get it from https://github.com/$REPO/releases"; exit 1; }
( cd "$tmp" && sha256sum -c --quiet "$name.tar.gz.sha256" ) || { echo "The download did not match its checksum."; exit 1; }
tar -xzf "$tmp/$name.tar.gz" -C "$tmp"
"$tmp/$name/install.sh"
