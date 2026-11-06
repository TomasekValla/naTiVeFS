# naTiVeFS

The native desktop client for [TVFS](https://github.com/TomasekValla/TVFS). Upload, share and open your files with real players instead of a browser tab. No Electron, no webview: Rust and [Slint](https://slint.dev), one small app.

**[Install it](INSTALLATION.md)** · [Releases](https://github.com/TomasekValla/naTiVeFS/releases) · `files.tomasekvalla.cz`

[![Vibecoded with Claude](https://img.shields.io/badge/vibecoded%20with-Claude-D97757?style=flat-square)](https://claude.ai)

```bash
# Linux: Arch, Debian, Ubuntu, Fedora, openSUSE and friends
curl -fsSL https://raw.githubusercontent.com/TomasekValla/naTiVeFS/main/install/linux.sh | sh
```

```powershell
# Windows
irm https://raw.githubusercontent.com/TomasekValla/naTiVeFS/main/install/windows.ps1 | iex
```

## The idea

TVFS keeps your files exactly as you uploaded them. naTiVeFS does the same on your side: it plays, reads and shows them with native tools, sends audio to your device untouched if you ask for it, and sits quietly in the tray when you don't need it. When nothing changes on screen, it draws nothing.

## What it does

- **Uploads the way the web does, plus more** — parallel chunks, resume after a dropped connection, two files at once, folders, batches, right-click **Upload to TVFS** in your file manager
- **Audio player with a bit-perfect mode** — the full TVFS effect rack and EQ, loudness normalization, exclusive output on Windows and Linux, or every effect off and the file's exact samples on your DAC
- **Video player** — libmpv with hardware decoding, VLC-style keys, subtitles and audio tracks, speed from 0.1× to 8×, statistics overlay
- **Readers and viewers** — Markdown and text in 20 fonts with Find and Copy all, images with animated GIF and WebP, each in its own window
- **Works on local files too** — open anything from your disk in the same tools, nothing is uploaded
- **Camera and voice notes** — take a photo, record a video or a voice note in Opus, AAC or FLAC, straight into the upload queue
- **TVFS links open in the app** — optional link router: TVFS links go to naTiVeFS, every other link goes on to your real browser
- **Lives in the tray** — player controls, open a file, upload, the link on your clipboard; start with the computer if you want
- **Updates itself, safely** — signed updates, downloaded in the background, never restarting in your face

## Platforms

| | Windows | Linux | macOS |
|---|---|---|---|
| x64 | Tested | Tested | |
| ARM64 | Built, not tried on hardware | Built, not tried on hardware | Built, not tried on hardware; no audio player yet |

## Build it yourself

Rust 1.98 or newer. Linux build packages are listed in [`.github/workflows/ci.yml`](.github/workflows/ci.yml).

```bash
cargo build --release --workspace --locked
```

[`docs/DECISIONS.md`](docs/DECISIONS.md) explains why things work the way they do, [`docs/TVFS_API_NOTES.md`](docs/TVFS_API_NOTES.md) covers the server API, and `vendor/` holds patched crates (see `vendor/README.md`).

## License

GPL-3.0-or-later. Third-party parts are listed in [`packaging/THIRD-PARTY.txt`](packaging/THIRD-PARTY.txt) and on the app's settings page.
