# Installing naTiVeFS

One command per system. All of them install the newest release and are signed with [minisign](https://jedisct1.github.io/minisign/) (public key below).

## Windows

In PowerShell:

```powershell
irm https://raw.githubusercontent.com/TomasekValla/naTiVeFS/main/install/windows.ps1 | iex
```

Installs for your user, no administrator rights. Prefer a window with choices (just for me or for all users, desktop shortcut, start with Windows)? Download `naTiVeFS-windows-x64-setup.exe` (or `-arm64-`) from [Releases](https://github.com/TomasekValla/naTiVeFS/releases).

Windows SmartScreen may warn the first time, because the app has no paid certificate: **More info > Run anyway**.

## Linux

One command for Arch and CachyOS, Debian, Ubuntu and Mint, Fedora and RHEL, and openSUSE. It finds your distribution and installs the right package:

```bash
curl -fsSL https://raw.githubusercontent.com/TomasekValla/naTiVeFS/main/install/linux.sh | sh
```

Other distributions, or no root rights: a copy in `~/.local/bin` that updates itself.

```bash
curl -fsSL https://raw.githubusercontent.com/TomasekValla/naTiVeFS/main/install/linux.sh | sh -s -- --portable
```

Notes:
- The video player needs libmpv and the camera and voice notes need ffmpeg. Packages pull them in where the distribution offers them. RHEL, AlmaLinux and Rocky have them only in extra repositories (EPEL, RPM Fusion).
- openSUSE: the script installs with `--allow-unsigned-rpm`, because our packages are signed with minisign, not with an RPM key.
- GNOME shows the tray icon only with the AppIndicator extension.
- Also available: `naTiVeFS-linux-x64.AppImage` from [Releases](https://github.com/TomasekValla/naTiVeFS/releases), which uses the system's libmpv and ffmpeg.

## macOS (Apple Silicon)

```bash
curl -fsSL https://raw.githubusercontent.com/TomasekValla/naTiVeFS/main/install/macos.sh | sh
```

Best effort: nobody has run it on a real Mac yet. Audio opens in the browser for now, and video needs `brew install mpv`.

## Updates

Installed copies update themselves: the app checks at startup and every hour, downloads in the background and offers a restart. It restarts by itself only while it rests in the tray with nothing open. Linux packages, AppImage and macOS show a note instead; run the install command again.

Settings > Updates has the channel: Stable, or Beta for the newest version including pre-releases.

## Uninstalling

- **Windows:** Settings > Apps > naTiVeFS.
- **Linux package:** remove `nativefs` with your package manager. Run `nativefs --cleanup` first to remove your own menus, autostart and browser registration.
- **Linux copy in `~/.local/bin`:** `nativefs --cleanup`, then delete `nativefs` and `nativefs-link`.
- **macOS:** delete naTiVeFS from Applications.

Your settings stay in `%APPDATA%\TomasekValla\naTiVeFS\` or `~/.config/nativefs/`.

## Build it for your own computer

For most people the normal build above is the right choice. This one takes a while, needs [Rust](https://rustup.rs) and does not update itself, in return for a build tuned to your exact processor. It downloads what it needs, builds, cleans up after itself and leaves only the app.

Linux:

```bash
curl -fsSL https://raw.githubusercontent.com/TomasekValla/naTiVeFS/main/install/build-for-this-pc.sh | sh
```

Windows (also needs the Visual Studio C++ build tools, CMake, Git and 7-Zip):

```powershell
irm https://raw.githubusercontent.com/TomasekValla/naTiVeFS/main/install/build-for-this-pc.ps1 | iex
```

Turn off **Automatic updates** in Settings afterwards on Linux, or an update replaces the tuned build with the normal one.

## Checking a download yourself

Every file on the release page has a `.minisig` signature:

```bash
minisign -Vm naTiVeFS-linux-x64-portable.tar.gz -P RWSQa45JZ1TSMKJ+gVjv+fsOf40z6FcRFMCMz2CplMxl40yszLg0URRu
```
