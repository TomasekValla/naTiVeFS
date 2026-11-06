#!/bin/sh
# Builds naTiVeFS tuned for this very computer and leaves only the app in ~/.local/bin. Needs Rust (https://rustup.rs);
# everything else it installs itself. Most people should use install/linux.sh instead: a tuned build takes a while and
# does not update itself. See INSTALLATION.md for what it gains.
#   curl -fsSL https://raw.githubusercontent.com/TomasekValla/naTiVeFS/main/install/build-for-this-pc.sh | sh
set -eu

REPO=TomasekValla/naTiVeFS
command -v cargo >/dev/null 2>&1 || { echo "naTiVeFS: install Rust first: https://rustup.rs" >&2; exit 1; }
. /etc/os-release
root=""
[ "$(id -u)" = 0 ] || root=sudo
case " ${ID:-} ${ID_LIKE:-} " in
    *" arch "*)
        $root pacman -S --needed --noconfirm base-devel git cmake pkgconf fontconfig libxkbcommon libxkbcommon-x11 wayland libx11 libxcb mesa dbus ;;
    *" debian "* | *" ubuntu "*)
        $root apt-get install -y build-essential git cmake pkg-config libfontconfig1-dev libxkbcommon-dev libxkbcommon-x11-dev \
            libwayland-dev libx11-dev libxcb1-dev libgl1-mesa-dev libegl1-mesa-dev libdbus-1-dev ;;
    *" suse "* | *" opensuse "* | *" opensuse-leap "* | *" opensuse-tumbleweed "*)
        $root zypper --non-interactive install gcc gcc-c++ make cmake git pkg-config fontconfig-devel freetype2-devel libxkbcommon-devel \
            libxkbcommon-x11-devel wayland-devel libX11-devel libxcb-devel Mesa-libGL-devel Mesa-libEGL-devel dbus-1-devel ;;
    *" fedora "* | *" rhel "* | *" centos "*)
        $root dnf install -y gcc gcc-c++ make cmake git pkgconf-pkg-config fontconfig-devel freetype-devel libxkbcommon-devel \
            libxkbcommon-x11-devel wayland-devel libX11-devel libxcb-devel mesa-libGL-devel mesa-libEGL-devel dbus-devel ;;
    *) echo "naTiVeFS: unknown distribution, install the build packages listed in .github/workflows/ci.yml yourself" >&2 ;;
esac

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
git clone --quiet --depth 1 "https://github.com/$REPO.git" "$tmp/src"
# libopus is built with CMake, which would install into lib64 on RPM systems; audiopus_sys looks in lib
echo 'set(CMAKE_INSTALL_LIBDIR lib CACHE PATH "")' > "$tmp/libdir.cmake"
cd "$tmp/src"
CMAKE_TOOLCHAIN_FILE="$tmp/libdir.cmake" RUSTFLAGS="-C target-cpu=native" CARGO_PROFILE_RELEASE_LTO=fat \
    cargo build --release --locked -p nativefs-app -p nativefs-link
mkdir -p "$HOME/.local/bin"
install -m 755 target/release/naTiVeFS "$HOME/.local/bin/nativefs"
install -m 755 target/release/naTiVeFS-link "$HOME/.local/bin/nativefs-link"
echo "Done: ~/.local/bin/nativefs. Turn off Automatic updates in Settings, or an update replaces this build with the normal one."
