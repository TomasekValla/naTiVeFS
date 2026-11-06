#!/bin/sh
# Installs naTiVeFS on a Mac with Apple Silicon into /Applications:
#   curl -fsSL https://raw.githubusercontent.com/TomasekValla/naTiVeFS/main/install/macos.sh | sh
set -eu

REPO=TomasekValla/naTiVeFS
[ "$(uname -m)" = arm64 ] || { echo "naTiVeFS: there is only a build for Apple Silicon" >&2; exit 1; }
tag=$(curl -fsSL "https://api.github.com/repos/$REPO/releases?per_page=1" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1)
[ -n "$tag" ] || { echo "naTiVeFS: could not find the newest release" >&2; exit 1; }

tmp=$(mktemp -d)
trap 'hdiutil detach "$tmp/mnt" -quiet 2>/dev/null || true; rm -rf "$tmp"' EXIT
echo "Downloading naTiVeFS $tag"
curl -fL --progress-bar -o "$tmp/naTiVeFS.dmg" "https://github.com/$REPO/releases/download/$tag/naTiVeFS-macos-apple-silicon.dmg"
mkdir "$tmp/mnt"
hdiutil attach "$tmp/naTiVeFS.dmg" -mountpoint "$tmp/mnt" -nobrowse -quiet
rm -rf /Applications/naTiVeFS.app
cp -R "$tmp/mnt/naTiVeFS.app" /Applications/
# the app is not signed by Apple: this says you chose to run it
xattr -dr com.apple.quarantine /Applications/naTiVeFS.app 2>/dev/null || true
echo "naTiVeFS $tag is in Applications."
command -v mpv >/dev/null 2>&1 || echo "For video install mpv: brew install mpv"
