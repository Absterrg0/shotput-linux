#!/bin/bash
# Vercel build: copies the latest release's downloads into download/, so the installer can fetch
# them from this site when GitHub's download host is slow or blocked on someone's network.
# A failed fetch never breaks the site; the installer then falls back to GitHub.
set -uo pipefail
REPO=Absterrg0/shotput-linux
mkdir -p download
urls=$(curl -fsSL --max-time 30 "https://api.github.com/repos/$REPO/releases/latest" |
  grep -o '"browser_download_url": *"[^"]*"' | sed 's/.*"\(https[^"]*\)"/\1/') || urls=""
for url in $urls; do
  curl -fsSL --max-time 120 "$url" -o "download/$(basename "$url" | sed "s/%40/@/g")" || echo "skipped $url"
done
curl -fsSL --max-time 30 "https://api.github.com/repos/$REPO/releases/latest" |
  grep -o '"tag_name": *"[^"]*"' | sed 's/.*"\([^"]*\)"$/\1/' > download/VERSION || true
ls -la download
