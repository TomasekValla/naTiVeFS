#!/bin/sh
# Installs naTiVeFS on Linux: picks the package for this distribution, or a copy in ~/.local/bin elsewhere.
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

# the newest release, pre-releases included
tag=$(curl -fsSL "https://api.github.com/repos/$REPO/releases?per_page=1" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1)
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
if command -v minisign >/dev/null 2>&1; then
    curl -fsSL -o "$tmp/$file.minisig" "$url.minisig"
    minisign -Vm "$tmp/$file" -P "$KEY" >/dev/null || die "the signature does not match, nothing was installed"
    say "Signature verified."
fi

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
        tar -xzf "$tmp/$file" -C "$tmp" nativefs nativefs-link
        install -m 755 "$tmp/nativefs" "$tmp/nativefs-link" "$HOME/.local/bin/"
        case ":$PATH:" in
            *":$HOME/.local/bin:"*) ;;
            *) say "Add ~/.local/bin to your PATH to start it by name." ;;
        esac
        ;;
esac

say "naTiVeFS $tag is installed. Start it from your app menu or with: nativefs"
command -v ffmpeg >/dev/null 2>&1 || say "Without ffmpeg the camera and voice notes will not work."
ldconfig -p 2>/dev/null | grep -q 'libmpv\.so' || say "Without libmpv (mpv) the video player will not work."
