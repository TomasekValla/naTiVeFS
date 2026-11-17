#!/bin/sh
# Installs naTiVeFS on Linux: picks the package for this distribution, or a copy in ~/.local/bin elsewhere.
# The download is checked against its minisign signature; a mismatch stops, an impossible check warns in red.
#   curl -fsSL https://raw.githubusercontent.com/TomasekValla/naTiVeFS/main/install/linux.sh | sh
#   ... | sh -s -- --portable     always the copy in ~/.local/bin (no root, updates itself)
set -eu

REPO=TomasekValla/naTiVeFS
KEY=RWSQa45JZ1TSMKJ+gVjv+fsOf40z6FcRFMCMz2CplMxl40yszLg0URRu
portable=0
[ "${1:-}" = "--portable" ] && portable=1

say() { printf '%s\n' "$*"; }
die() { say "naTiVeFS: $*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "this needs $1"; }

# Signature check of a release file against its .minisig (prints "verified", "mismatch", or why it could not run).
# minisign when installed, else OpenSSL 3 (Ed25519 on raw data + BLAKE2b-512); LibreSSL (macOS) has neither.
# Every step may fail on a malformed signature file: that is a "mismatch", never an abort or a "could not check".
b64d() { base64 -d 2>/dev/null || base64 -D 2>/dev/null || :; }
# the signed ("trusted") comment must name exactly this file ($2), so no other signed file can be passed off as it
signed_name_ok() {
    t=$(printf '\t')
    c=$(sed -n 3p "$1" | tr -d '\r' | sed 's/^trusted comment: //') || return 1
    case "$t$c$t" in *"${t}file:$2$t"*) return 0 ;; *) return 1 ;; esac
}
check_signature() {
    if command -v minisign >/dev/null 2>&1; then
        if minisign -Vm "$1" -x "$2" -P "$KEY" >/dev/null 2>&1 && signed_name_ok "$2" "$3"; then echo verified; else echo mismatch; fi
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
    v=$(mktemp -d) || { echo error; return 0; }
    printf '%s' "$KEY" | b64d > "$v/pk" || :
    sed -n 2p "$2" | tr -d '\r' | b64d > "$v/sg" || :
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
    sed -n 4p "$2" | tr -d '\r' | b64d > "$v/global" || :
    cat "$v/sig" "$v/comment" > "$v/both"
    if "$ossl" pkeyutl -verify -pubin -inkey "$v/pk.pem" -rawin -in "$v/msg" -sigfile "$v/sig" >/dev/null 2>&1 \
        && "$ossl" pkeyutl -verify -pubin -inkey "$v/pk.pem" -rawin -in "$v/both" -sigfile "$v/global" >/dev/null 2>&1 \
        && signed_name_ok "$2" "$3"; then
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
        echo "${r} WARNING: this copy of naTiVeFS could NOT be verified (no tool to check signatures here).${n}"
        echo "${r} Reason: $1${n}"
        echo "${r} It was downloaded and installed anyway. You use an unverified copy at your own risk.${n}"
        echo "${r} To check it next time, install minisign or OpenSSL 3 ($2) and run this again,${n}"
        echo "${r} or download it from https://github.com/$REPO/releases and verify the .minisig yourself.${n}"
        echo "${r}!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!${n}"
        echo
    } >&2
}

need curl
need tar
case "$(uname -m)" in
    x86_64 | amd64) arch=x64 ;;
    aarch64 | arm64) arch=arm64 ;;
    *) die "no build for $(uname -m)" ;;
esac

family=other
if [ "$portable" = 0 ] && [ -r /etc/os-release ]; then
    . /etc/os-release
    case " ${ID:-} ${ID_LIKE:-} " in
        *" arch "*) family=arch ;;
        *" debian "* | *" ubuntu "*) family=debian ;;
        *" suse "* | *" opensuse "* | *" opensuse-leap "* | *" opensuse-tumbleweed "*) family=suse ;;
        *" fedora "* | *" rhel "* | *" centos "*) family=fedora ;;
    esac
fi

# the newest stable release (pre-releases are for testers, like in the app's updater)
tag=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1)
[ -n "$tag" ] || die "could not find the newest release"

case "$family" in
    arch) file="naTiVeFS-linux-$arch-arch.pkg.tar.zst" ;;
    debian) file="naTiVeFS-linux-$arch-debian-ubuntu.deb" ;;
    fedora | suse) file="naTiVeFS-linux-$arch-fedora-opensuse.rpm" ;;
    *) file="naTiVeFS-linux-$arch-portable.tar.gz" ;;
esac

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
say "Downloading naTiVeFS $tag ($file)"
url="https://github.com/$REPO/releases/download/$tag/$file"
curl -fL --progress-bar -o "$tmp/$file" "$url"
# every release is signed: a missing signature is suspicious, not a missing tool, so it stops the install
curl -fsSL -o "$tmp/$file.minisig" "$url.minisig" \
    || die "the signature file could not be downloaded, so this download cannot be trusted. Nothing was installed."
result=$(check_signature "$tmp/$file" "$tmp/$file.minisig" "$file") || result=mismatch
case "$result" in
    verified) say "Signature verified." ;;
    mismatch) die "the signature does NOT match: this file is not the published naTiVeFS. Nothing was installed." ;;
    error) die "the signature could not be checked (no temporary folder could be created). Nothing was installed." ;;
    *) warn_unverified "$result" "e.g. sudo apt install minisign / sudo dnf install minisign / sudo pacman -S minisign" ;;
esac

root=""
if [ "$(id -u)" != 0 ] && [ "$family" != other ]; then
    need sudo
    root=sudo
fi
case "$family" in
    arch) $root pacman -U --noconfirm "$tmp/$file" ;;
    debian) $root apt-get install -y "$tmp/$file" ;;
    fedora) $root dnf install -y "$tmp/$file" ;;
    # our packages are signed with minisign, not with an RPM key
    suse) $root zypper --non-interactive install --allow-unsigned-rpm "$tmp/$file" ;;
    *)
        mkdir -p "$HOME/.local/bin"
        mkdir -p "$tmp/x"
        tar -xzf "$tmp/$file" -C "$tmp/x"
        install -m 755 "$tmp/x/nativefs" "$tmp/x/nativefs-link" "$HOME/.local/bin/"
        # the command line client ships from the release after 1.0.0 on
        if [ -f "$tmp/x/nativefs-cli" ]; then
            install -m 755 "$tmp/x/nativefs-cli" "$HOME/.local/bin/"
            ln -sf nativefs-cli "$HOME/.local/bin/nativefscli"
            ln -sf nativefs-cli "$HOME/.local/bin/nativecli"
        fi
        case ":$PATH:" in
            *":$HOME/.local/bin:"*) ;;
            *) say "Add ~/.local/bin to your PATH to start it by name." ;;
        esac
        ;;
esac

say "naTiVeFS $tag is installed. Start it from your app menu or with: nativefs"
if [ "$family" != other ] && [ -e "$HOME/.local/bin/nativefs" ]; then
    say "An older copy in ~/.local/bin would start instead of this one. Run it with --cleanup and delete it."
fi
command -v ffmpeg >/dev/null 2>&1 || say "Without ffmpeg the camera and voice notes will not work."
{ ldconfig -p || /sbin/ldconfig -p; } 2>/dev/null | grep -q 'libmpv\.so' || say "Without libmpv (mpv) the video player will not work."
