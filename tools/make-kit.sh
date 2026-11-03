#!/usr/bin/env bash
# Build the cumulative kit zip for the next chat.
#
#   tools/make-kit.sh <stage-number> <label> [previous-kit-dir] [output-dir]
#
# stage-number       the stage you are working on
# label              V1, V2, V3, ... for work-in-progress versions; FINAL for the last one
#                    (use FINAL only when the owner ends the chat).
#                    Output: naTiVeFS-kit-stage-<N>-<label>.zip
# previous-kit-dir   the UNPACKED previous kit (the folder containing repo/, inputs/, history/);
#                    omit only for the very first kit. inputs/ and history/ are inherited unchanged.
# output-dir         where to write the zip (default: current directory)
#
# The kit only ever grows: nothing inherited is dropped.
set -euo pipefail

N="${1:?usage: make-kit.sh <stage-number> <label V1|V2|...|FINAL> [previous-kit-dir] [output-dir]}"
LABEL="${2:?label required: V1, V2, ... or FINAL}"
PREV="${3:-}"
OUT="${4:-$PWD}"
[[ "$LABEL" =~ ^(V[0-9]+|FINAL)$ ]] || { echo "label must be V<number> or FINAL" >&2; exit 1; }
NAME="naTiVeFS-kit-stage-$N-$LABEL"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
WORK="$(mktemp -d)"
KIT="$WORK/$NAME"
mkdir -p "$KIT/repo" "$KIT/inputs" "$KIT/history"

# 1. repo snapshot (no build output, no VCS data)
# (downloaded libmpv builds are left out too: ~200 MB, fetched again by tools/fetch-libmpv.sh)
tar -C "$REPO" --exclude='./target' --exclude='./.git' --exclude='./.claude' --exclude='./vendor/libmpv/dl' --exclude='./vendor/libmpv/win-*' -cf - . | tar -C "$KIT/repo" -xf -

# 2. inherit inputs/ and history/ from the previous kit
if [ -n "$PREV" ]; then
  [ -d "$PREV/inputs" ] && [ -d "$PREV/history" ] || { echo "previous kit dir must contain inputs/ and history/" >&2; exit 1; }
  cp -a "$PREV/inputs/." "$KIT/inputs/"
  cp -a "$PREV/history/." "$KIT/history/"
fi

# 3. snapshot of this stage
(cd "$KIT/repo" && zip -qr "$KIT/history/naTiVeFS-stage-$N-$LABEL.zip" .)

# 4. refuse to ship anything that looks like runtime data/secrets
BAD='(^|/)(\.env(\..*)?|sessions[^/]*\.json|file_registry\.json|nohup\.out|protected_storage|node_modules|users)(/|$)|\.(pem|key)$'
if { cd "$KIT" && find . -type f -o -type d; } | grep -Eq "$BAD"; then
  echo "refusing: secret-looking path in the kit:" >&2
  { cd "$KIT" && find . | grep -E "$BAD"; } >&2
  exit 1
fi
for z in "$KIT"/inputs/*.zip "$KIT"/history/*.zip; do
  [ -f "$z" ] || continue
  if unzip -Z1 "$z" | grep -Eq "$BAD"; then
    echo "refusing: secret-looking path inside $(basename "$z"):" >&2
    unzip -Z1 "$z" | grep -E "$BAD" >&2
    exit 1
  fi
done

# 5. index + checksums
cp "$REPO/docs/START_HERE.md" "$KIT/START_HERE.md"
(cd "$KIT" && find . -type f ! -name MANIFEST.sha256 -print0 | sort -z | xargs -0 sha256sum > MANIFEST.sha256)

# 6. zip
mkdir -p "$OUT"
rm -f "$OUT/$NAME.zip"
(cd "$WORK" && zip -qr "$OUT/$NAME.zip" "$NAME")
rm -rf "$WORK"
echo "wrote $OUT/$NAME.zip"
