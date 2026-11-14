#!/bin/sh
# Installs naTiVeFS on a Mac with Apple Silicon into /Applications (a running copy is quit first, the new one started):
#   curl -fsSL https://raw.githubusercontent.com/TomasekValla/naTiVeFS/main/install/macos.sh | sh
set -eu

REPO=TomasekValla/naTiVeFS
KEY=RWSQa45JZ1TSMKJ+gVjv+fsOf40z6FcRFMCMz2CplMxl40yszLg0URRu
APP=/Applications/naTiVeFS.app

# Signature check of a release file against its .minisig (prints "verified", "mismatch", or why it could not run).
# minisign when installed, else OpenSSL 3 (Ed25519 on raw data + BLAKE2b-512); LibreSSL (macOS) has neither.
b64d() { base64 -d 2>/dev/null || base64 -D; }
check_signature() {
    if command -v minisign >/dev/null 2>&1; then
        if minisign -Vm "$1" -x "$2" -P "$KEY" >/dev/null 2>&1; then echo verified; else echo mismatch; fi
        return 0
    fi
    ossl=""
    for o in openssl /opt/homebrew/opt/openssl@3/bin/openssl /usr/local/opt/openssl@3/bin/openssl; do
        command -v "$o" >/dev/null 2>&1 || continue
        "$o" version 2>/dev/null | grep -q '^OpenSSL [3-9]' || continue
        "$o" list -digest-algorithms 2>/dev/null | grep -qi 'blake2b512' || continue
        "$o" list -public-key-algorithms 2>/dev/null | grep -qi 'ed25519' || continue
        ossl=$o
        break
    done
    [ -n "$ossl" ] || { echo "neither minisign nor OpenSSL 3 is installed"; return 0; }
    v=$(mktemp -d) || { echo "no temporary folder"; return 0; }
    printf '%s' "$KEY" | b64d > "$v/pk" 2>/dev/null
    sed -n 2p "$2" | tr -d '\r' | b64d > "$v/sg" 2>/dev/null
    if [ "$(wc -c < "$v/pk")" -ne 42 ] || [ "$(wc -c < "$v/sg")" -ne 74 ]; then rm -rf "$v"; echo mismatch; return 0; fi
    # the signature must come from our key (same key id)
    if [ "$(od -An -tx1 -j2 -N8 "$v/pk")" != "$(od -An -tx1 -j2 -N8 "$v/sg")" ]; then rm -rf "$v"; echo mismatch; return 0; fi
    # public key as PEM: the fixed Ed25519 SubjectPublicKeyInfo prefix + the 32 key bytes
    { printf '\060\052\060\005\006\003\053\145\160\003\041\000'; tail -c 32 "$v/pk"; } > "$v/pk.der"
    { echo "-----BEGIN PUBLIC KEY-----"; base64 < "$v/pk.der" | tr -d '\n'; echo; echo "-----END PUBLIC KEY-----"; } > "$v/pk.pem"
    tail -c 64 "$v/sg" > "$v/sig"
    case "$(head -c 2 "$v/sg")" in
        ED) "$ossl" dgst -blake2b512 -binary "$1" > "$v/msg" || { rm -rf "$v"; echo "OpenSSL could not hash the file"; return 0; } ;;
        Ed) cp "$1" "$v/msg" ;;
        *) rm -rf "$v"; echo mismatch; return 0 ;;
    esac
    # the trusted comment (file name, time) is covered by the global signature over signature + comment
    sed -n 3p "$2" | tr -d '\r\n' | sed 's/^trusted comment: //' > "$v/comment"
    sed -n 4p "$2" | tr -d '\r' | b64d > "$v/global" 2>/dev/null
    cat "$v/sig" "$v/comment" > "$v/both"
    if "$ossl" pkeyutl -verify -pubin -inkey "$v/pk.pem" -rawin -in "$v/msg" -sigfile "$v/sig" >/dev/null 2>&1 \
        && "$ossl" pkeyutl -verify -pubin -inkey "$v/pk.pem" -rawin -in "$v/both" -sigfile "$v/global" >/dev/null 2>&1; then
        echo verified
    else
        echo mismatch
    fi
    rm -rf "$v"
}

# Big red warning when the download could not be checked; the install goes on at the user's own risk.
warn_unverified() {
    r=$(printf '\033[1;31m'); n=$(printf '\033[0m')
    [ -t 2 ] || { r=""; n=""; }
    {
        echo
        echo "${r}!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!${n}"
        echo "${r} WARNING: this copy of naTiVeFS could NOT be verified.${n}"
        echo "${r} Reason: $1${n}"
        echo "${r} It was downloaded and installed anyway. You use an unverified copy at your own risk.${n}"
        echo "${r} To check it next time, install minisign or OpenSSL 3 ($2) and run this again,${n}"
        echo "${r} or download it from https://github.com/$REPO/releases and verify the .minisig yourself.${n}"
        echo "${r}!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!${n}"
        echo
    } >&2
}
[ "$(uname -m)" = arm64 ] || { echo "naTiVeFS: there is only a build for Apple Silicon" >&2; exit 1; }
tag=$(curl -fsSL "https://api.github.com/repos/$REPO/releases?per_page=1" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1)
[ -n "$tag" ] || { echo "naTiVeFS: could not find the newest release" >&2; exit 1; }

tmp=$(mktemp -d)
trap 'hdiutil detach "$tmp/mnt" -quiet 2>/dev/null || true; rm -rf "$tmp"' EXIT
echo "Downloading naTiVeFS $tag"
url="https://github.com/$REPO/releases/download/$tag/naTiVeFS-macos-apple-silicon.dmg"
curl -fL --progress-bar -o "$tmp/naTiVeFS.dmg" "$url"
if curl -fsSL -o "$tmp/naTiVeFS.dmg.minisig" "$url.minisig"; then
    result=$(check_signature "$tmp/naTiVeFS.dmg" "$tmp/naTiVeFS.dmg.minisig")
else
    result="the signature file could not be downloaded"
fi
case "$result" in
    verified) echo "Signature verified." ;;
    mismatch) echo "naTiVeFS: the signature does NOT match: this file is not the published naTiVeFS. Nothing was installed." >&2; exit 1 ;;
    *) warn_unverified "$result" "brew install minisign" ;;
esac
mkdir "$tmp/mnt"
hdiutil attach "$tmp/naTiVeFS.dmg" -mountpoint "$tmp/mnt" -nobrowse -quiet
# a running copy (often hidden in the menu bar) must stop first, or a click on the new app only wakes the old process
if [ -x "$APP/Contents/MacOS/nativefs" ]; then
    "$APP/Contents/MacOS/nativefs" --quit 2>/dev/null || true
    sleep 2
fi
pkill -x nativefs 2>/dev/null || true
rm -rf "$APP"
cp -R "$tmp/mnt/naTiVeFS.app" /Applications/
# the app is not signed by Apple: this says you chose to run it
xattr -dr com.apple.quarantine "$APP" 2>/dev/null || true
echo "naTiVeFS $tag is in Applications. Starting it."
open "$APP" || true
command -v mpv >/dev/null 2>&1 || echo "For video install mpv: brew install mpv"
