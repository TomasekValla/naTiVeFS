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

## Performance and layout (2026-11)
- The main window's column geometry is computed in Rust (`layout.rs`) and handed to Slint as plain values. Measured on Windows
  (femtovg): with bindings on the window width every resize step re-wrapped the text of every card (~8 ms per step, also where no
  card changes width), because Slint marks all dependants dirty even when a value stays the same. The layout switch glides the
  cards' x / y and snaps their width (animating the width re-wrapped text on every frame).
- The Markdown / text reader groups its blocks into runs of 32 (`ReaderChunk`) and hides the runs more than a screen away from
  the view (their place in the layout stays). Measured on a ~3,900-line document: a scroll frame went from ~5.6 ms to ~2.4 ms
  of rendering; Slint's own clipping did not skip the off-screen blocks.
- Big documents (64 runs and more) are virtualized: Slint gets only the runs near the view (one screen above, two below); the
  rest of the page is two empty spaces above and below them (a search match outside moves the view there first). Runs keep an
  estimated height, replaced by the measured one once a run has
  been shown (the view is shifted when a run above it changes height, so nothing jumps). Display rows are built on all cores
  (rayon), off the UI thread for big documents; a loading bar shows while the file is read and the page is built. Measured on
  an 8 MB / 273,441-block file (debug build): content after ~3 s instead of > 60 s frozen, worst UI pause 184 ms; scrolling
  40 x 800 px: worst pause 72 ms, p90 36 ms (139 / 74 ms while every run was still in the model as an empty box).
- App-only preferences without a field in the shared config (compact layout, the recorder's input level / auto level) are kept
  in `app-prefs.json` beside the config.
- Pages that naTiVeFS or the link router hands to the browser carry `inbrowser=1`, so the web page does not send them straight
  back to the app. The foreground right (Windows) is passed only to the process that receives the hand-over.

## TVFS hosts and trusted URLs (1.2)
- TVFS serves pages and the API from `tvfs.tomasekvalla.cz` and uploaded files from `tvfs-usercontent.tomasekvalla.cz`; the
  old `files.tomasekvalla.cz` keeps working. The app treats all three as TVFS and takes the current API address from the
  server (`GET /api/hosts`, cached in `hosts.json`, used from the next start). Only a TVFS page host can become the API
  address, so the server cannot point the app elsewhere. The login cookie is kept for both page hosts, never for the
  user-content host.
- Every URL from outside (a link, a generated page, the sync list, a share) goes through one check (`hosts::TrustedUrl`):
  https on a TVFS host, default port, no user info; a file URL read from a page must be a TVFS file (`/files/`,
  `/api/protected/`). Links that leave the app are only http, https and mailto.
- Updates: the signed comment of a release file names the file and the version; the app installs only a file signed for
  that asset and a version newer than its own, keeps the download in a private folder and checks it again right before
  running it. Camera and voice captures, generated files and downloads use a private per-user folder, not the shared temp.
- The expiry picker follows the account's rules from `GET /api/config` exactly like the web: options a login can never use
  are not shown, options blocked only for now (file size, free space, the host's time to midnight) are greyed out; the
  server applies the same rules itself.
