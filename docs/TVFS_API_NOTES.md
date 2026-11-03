# TVFS API notes (verified against the TVFS source at stage 1; upload, batch, protected, probe verified against the live server in stage 2)

Source of truth is the TVFS zip: `files_backend/upload.js` (~2700 lines, most routes), `protectedShare.js`,
`audioPlayer.js`, `userSync.js`, `probe.js`, `thumbnails.js`, and the web client `files_web/upload.html`.
Everything below was read from that code; items marked **(unverified)** were only seen as names.

## Topology
- nginx (`files_web/conf.d/default.conf`) serves static files and proxies `/api/...` to the Express backend.
- Uploaded files are served statically at `https://files.tomasekvalla.cz/files/<dir>/<name>` with
  `<dir>` in `pictures, videos, audio, download, batch, text, players`.
  `players/` holds generated HTML pages (audio player, text/Markdown readers).
- TVFS has **no file-listing endpoint**. The only per-user list is "recently uploaded" (`/api/user/sync`).
  The app is therefore: upload + recents + share + open links in native viewers.

## Auth (cookie)
- `POST /api/auth` JSON `{username, password, remember}` -> `200 {valid:true, tier}` and `Set-Cookie: tvfs_token=...`
  (httpOnly; ~24 h, or ~1 year with `remember`). Wrong credentials -> `403 {valid:false, tier:0}`.
- `POST /api/logout` clears the cookie. A rate limiter exists (`429`).
- `authenticateRequest` also accepts credentials in the body as fallback on some routes **(re-check before relying on it)**.

## Status / sync
- `GET /api/status` -> `{backend, usedBytes, usedGB:"1.23 GB", totalGB, tier, userVersion, pendingUploads[]}` (note the
  capital `GB`; stage 1 read `usedGb` and never showed the value). `pendingUploads[]` =
  `{uploadId, filename, fileSize, totalChunks?, receivedChunks, createdAt?, expiresAt?}`. The server caches this list for a
  few seconds, so an upload that just finished can still be listed (the app hides its ids for 60 s).
  The web polls it about every 10 s; `userVersion` changes when the recent list changes.
- `GET /api/user/sync[?since=N]` -> `{mode:"full", version, files:[entry]}` or
  `{mode:"diff", version, ops:[{v, type:"add", entry} | {v, type:"remove", fileId}]}`.
  `entry` = `{id, name, link, category, size, uploadedAt(ms), expiresAt(ms|null), thumbnail|null}`.

## Upload (chunked)
- `POST /api/upload/init` JSON `{filename, fileSize, mimeType, totalChunks?, uploadMode, expirationMinutes, batchId?}`
  -> `{uploadId, recommendedParallel?, message}`. Omit `totalChunks` for **dynamic chunking** (then send `isLastChunk`).
- `POST /api/upload/chunk` multipart, field order **uploadId, chunkIndex, chunkSize, [isLastChunk], chunk (file part, last)**
  -> `{success, chunkIndex, received, total, duplicate?}`. Duplicate chunks are tolerated (safe to retry).
- `POST /api/upload/complete` JSON `{uploadId, anonymize, removeTimestamp, expirationMinutes, batchId?}`
  -> `{link, type, filename, size, fileId, styledLinkPending?, styledLink?, textReaderLink?, textReaderStyledLink?, mdReaderLink?,
  mdReaderStyledLink?, audioPlayerLink?, audioPlayerStyledLink?, trackBpm?}` (the `*StyledLink` values are Markdown links).
- Expiry presets (minutes): `5, 30, 60, 360, 720, 1440, 2880, 4320, 7200, 10080 (default), 14400, 20160`.
- `DELETE /api/file/:id` removes a file.
- `GET /api/upload/status/:uploadId` -> `{active, receivedChunks:[i...], totalChunks?, filename?, fileSize?}`.
- `DELETE /api/upload/chunks/:uploadId` drops the stored chunks of a cancelled upload.
- `POST /api/upload` multipart (simple upload, files up to 25 MB in one request; same response as `/complete`).
- `POST /api/upload/batch/init` `{expirationMinutes}` -> `{batchId}`; every file then passes `batchId` to init/complete;
  `POST /api/upload/batch/complete` `{batchId}` -> `{batchId, link, landingPage?, styledLink?, files:[{id, originalName, link, styledLink?, size}]}`.
- `GET /api/upload/styled-link/:fileId` -> `{ready, styledLink?, thumbnail?}`; ready a few seconds after `/complete`
  (`styledLinkPending:true` in the complete response). The web polls it; so does the app.
- "TVFS Users Only" shares (`protectedShare.js`): `POST /api/protected/upload` (small, multipart) or
  `/api/protected/upload/{init,chunk,complete}` -> `{shareId, link, expiresAt?, files:[{index, relativePath, size, category?, toolLink?}]}`.
  The server does not list these in `/api/user/sync`; the web and the app keep them locally.
- `POST /api/probe` `{url}` (no login needed) -> `{streams:[...], format:{filename, format_name, format_long_name, duration, size,
  bit_rate, nb_streams}|null, raw:<full ffprobe JSON>}`; only `https://files.tomasekvalla.cz/...` URLs
  (`400 {error:"Only files.tomasekvalla.cz URLs are allowed."}`), `500 {error:"ffprobe failed"}` for missing/expired files.
  Used by the File Check page (`check.html`, native page 2).
- `/api/verify-password` exists but is not used. `/api/encrypted/*` is **legacy/archived: do not implement**.
- All upload constants (chunk sizes, connections, retries, weak/safe mode, back-pressure) are implemented 1:1 in
  `crates/tvfs-core/src/upload/plan.rs` / `backoff.rs`; the values and their origin are commented there.

## Generated pages (what the native viewers replace)
- Audio player page (`audioPlayer.js`): BPM-derived font choice, parametric EQ, pitch/speed, beat-reactive visuals, marquee title.
- Text / Markdown readers (generated in `upload.js`, e.g. `generateTextReaderHtml`, `buildReaderChromeHtml`) with the self-hosted fonts.
- There is **no video player page** in TVFS; the browser plays `/files/videos/...` natively. The native video player is new.

## Where files land and how the app resolves links (stage 3, `tvfs_core::content`)
- Directory by MIME type (`getDestDir`): browser-friendly image / video / audio types -> `pictures/`, `videos/`, `audio/`;
  `text/markdown` -> `text/`; **everything else -> `download/`**, including `.mkv`, `.avi`, `.txt`. The app therefore picks the
  viewer by **file extension**, not by directory (`content::kind_from_name`).
- Reader pages: `/files/text/<slug>_txt_tvfsreader.html` and `<slug>_md_tvfsreader.html` (public),
  `/files/protected/tools/<shareId>_<index>_reader.html` (protected). Title `<name> — TVFS Text Reader | MD Reader | Users Only Reader`,
  the file URL is the JSON string literal in `fetch(<url>)` (`safeScriptJson` = `JSON.stringify` with `<` escaped).
- Audio player pages: `/files/players/<slug>_<ms>_player.html` (public), `/files/protected/tools/<slug>_<shareId>_player.html`;
  the file URL is `var AUDIO_URL = <json>;`.
- Batch pages `/files/batch/<batchId>.html`: `const files = [{url, name, styled}]`, `const pageLink`, `const pageStyled`,
  `const batchId`, `<p class="subtitle">N files • size</p>`, `<span class="file-size">` per row, `<p class="expire-note">`.
- `POST /api/batch/:id/zip` (no auth) -> `{link}` of `files/download/batch_<id>.zip` (the server zips the batch; same expiry).
- Protected shares: landing page `/files/protected/pages/<shareId>.html`; `GET /api/protected/:id/meta` (no auth) ->
  `{id, fileCount, files:[{index, relativePath, size, category, toolLink}], totalSize, expiresAt}`;
  `GET /api/protected/:id/file/:index` needs the `tvfs_token` cookie (403 otherwise). The player sends the cookie to mpv as an
  HTTP header for these URLs.
- nginx answers missing files with an HTML 404 page; the client drops HTML error bodies (only the status is shown).
- **Server bug seen while testing (2026-10-25, not fixed, server code is out of scope):** a file uploaded under the same stored name
  as a file that just expired (e.g. `still.png` twice with the 5-minute expiry) disappeared within seconds, apparently removed by the
  cleanup of the old registry entry. Details in `docs/BUGS.md`.

## Fonts
`files_web/fonts/` = 442 `.woff2` files + `fonts.css` (Google Fonts unicode-range subsets, 20 families). Slint cannot load
woff2. Stage 3 bundles static TTF/OTF downloads instead (`tools/fetch-reader-fonts.py`, `assets/fonts/reader/`, OFL licences next to them).
The web serves almost every family only at weight 400 (bold is synthesized by the browser) and has no OpenDyslexic file in `fonts.css`
(that entry falls back to the default font on the web); the app has real bold / italic files and OpenDyslexic.

## Old distribution files (to be replaced in stage 5)
`files_web/get.html`, `files_web/get/`, `install-linux.sh`, `install-windows.ps1` belong to the **old** naTiVeFS.

## Not in scope
`files_backend/index.js` also mounts modules not in the open-source tree (spotify, speedtest, battery, moneygame, twc).
