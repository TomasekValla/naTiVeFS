# Design decisions

The short, public list of why naTiVeFS works the way it does. "Owner" = the project owner.

## General
- The native UI follows the TVFS web client 1:1 (measured from the web's computed styles); native extras are added only where
  a desktop app clearly benefits.
- Idle CPU / GPU close to zero is a hard rule: nothing redraws or polls while nothing changes or the window is hidden.
- Debug builds use their own IPC socket, autostart entry, browser registration and file-manager labels, so testing never touches an
  installed release.

## Uploads and sharing
- Chunking matches the web client (auto chunk size from measured speed, the same limits and retries); two files upload at once
  through one shared connection pool.
- Small robustness deviations from the web: 401 / 403 fail at once, retries only where the server cannot have taken the data,
  `/complete` falls back to `/api/user/sync`. Unfinished uploads resume from their stored path.
- File Check is a native page with the web's content; long names are shortened instead of the web's marquee (idle rule).

## Viewers
- Every viewer is its own window: video player (libmpv, loaded at runtime, OpenGL render API under Slint), reader (Markdown / text,
  with Find and Copy all), image viewer (animated GIF / WebP), download and batch windows, audio player.
- Video player: VLC-like keys, speed 0.1-8x with pitch correction, subtitles / audio track by the system language, optional
  statistics, hardware decoding on by default. Known issue: frame drops at 120 fps on high-refresh monitors (fix planned: mpv on its
  own render thread with shared GL textures).
- Audio player: follows the web player (effect rack, EQ, presets, particles, tour); loudness normalization at -14 LUFS plus a
  separate Bit-perfect mode that bypasses all processing. Output: WASAPI exclusive by default (released on pause) on Windows,
  ALSA `hw:` / `default` on Linux; macOS opens audio in the browser for now. Devices without hardware volume never start playing
  by themselves in exclusive mode.
- Local files can be opened in the viewers ("Use TVFS tools locally"); nothing is uploaded.
- Camera and voice note: Media Foundation on Windows, ffmpeg on Linux; voice notes as Opus, AAC or FLAC.

## OS integration (stage 5)
- Tray icon with a small control centre (player controls, open local file, upload, clipboard link, voice note, settings).
- Start with the computer and start in the background (tray only), per user, per OS.
- Link router (opt-in, offered once on the main page): naTiVeFS registers as a browser; TVFS links open in the app, all other links go
  to the user's real browser (the previous default or a picked / typed one). Windows lets only the user pick the default browser,
  so the app opens the right Settings page. A browser command that would start naTiVeFS is refused (no loops). Shared links stay
  normal `https://` links; `tvfs://` only lets the TVFS web hand a page to the app.
- File manager menus "Upload to TVFS" and "Open locally in naTiVeFS": Windows classic menu + Send To (per user), Dolphin, Nautilus,
  Nemo, Thunar. The app claims its single-instance socket first thing, so many files sent at once all reach one instance.
- Windows installer offers per-user and all-users installs.
