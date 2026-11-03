#!/usr/bin/env bash
# Download the libmpv the Windows builds ship next to naTiVeFS.exe (video player).
#
#   tools/fetch-libmpv.sh [x86_64|aarch64 ...]     (default: both)
#
# Builds by shinchiro (github.com/shinchiro/mpv-winbuild-cmake), pinned by tag and SHA-256.
# Result: vendor/libmpv/win-<arch>/libmpv-2.dll (+ include/). build.rs copies the DLL next to the
# executable; without it the app still builds and runs, the player then says libmpv is missing.
# Linux / macOS use the system libmpv (packages depend on it; see vendor/libmpv/README.md).
set -euo pipefail

TAG=20261021
COMMIT=e471496c56
declare -A SHA=(
  [x86_64]=efc719eff3dc5954b4a5efc800632808bcec01cc9034940dd14e5eb7fe9ea633
  [aarch64]=015f133399b3525021b6d937cf54ace2761d4f70224d5e62f5df24191b28f398
)

ROOT="$(cd "$(dirname "$0")/.." && pwd)/vendor/libmpv"
SEVENZ="$(command -v 7z || command -v 7za || echo "/c/Program Files/7-Zip/7z.exe")"
mkdir -p "$ROOT/dl"
for arch in "${@:-x86_64 aarch64}"; do
  for a in $arch; do
    f="mpv-dev-$a-$TAG-git-$COMMIT.7z"
    if [ ! -f "$ROOT/dl/$f" ]; then
      curl -fsSL -o "$ROOT/dl/$f" "https://github.com/shinchiro/mpv-winbuild-cmake/releases/download/$TAG/$f"
    fi
    echo "${SHA[$a]}  $ROOT/dl/$f" | sha256sum -c -
    rm -rf "$ROOT/win-$a"
    mkdir -p "$ROOT/win-$a"
    "$SEVENZ" x -y -o"$ROOT/win-$a" "$ROOT/dl/$f" > /dev/null
    ls -la "$ROOT/win-$a/libmpv-2.dll"
  done
done
