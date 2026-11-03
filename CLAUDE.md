# naTiVeFS — notes for AI coding agents

Native Rust client (Slint UI, no webview) for the self-hosted TVFS file service. Start with `README.md`, `docs/DECISIONS.md` and
`docs/TVFS_API_NOTES.md`. A git-ignored `CLAUDE.local.md` may hold the maintainer's private working notes.

Naming — use exactly this, never invent other names:
- Product / window / tray / docs: **naTiVeFS**.
- Executables: **`naTiVeFS.exe`** (the app) and **`naTiVeFS-link.exe`** (link router); on Linux/macOS Cargo builds `naTiVeFS` /
  `naTiVeFS-link`, and release packages ship them lowercase as `nativefs` / `nativefs-link` (`release.yml` renames them,
  `naTiVeFS-link` finds the app under either name).
- Cargo packages (must be lowercase): `nativefs-app`, `nativefs-link`, `nativefs-ipc`, `nativefs-audio` (+ `tvfs-core`).
- Release assets: `naTiVeFS-<windows-x64|windows-arm64|linux-x64|linux-arm64|macos-arm64>.<zip|tar.gz>` + `.minisig`.
- Config dir: `%APPDATA%\TomasekValla\naTiVeFS\config\` (Windows), `~/.config/nativefs/` (Linux). IPC socket: `nativefs-ipc-<user>`.

Rules:
- Code, comments, commit messages and docs in **English**.
- **Nothing personal in this repository** except the owner's public name (Tomáš Valla / TomasekValla): no other names, hardware,
  local paths, user / host names, quotes, personal test files or screenshots with personal data, in files or in commit messages. Decisions and test findings are written anonymized.
- Never guess the UI: the native UI must match the web TVFS 1:1. Render the web (`tools/web-reference/`) and measure computed styles.
- Say exactly what was compiled / run / measured, per OS. On errors, find the root cause by measuring; no speculative fixes.
- Idle CPU/GPU ≈ 0 is a hard criterion; stop timers and polling when the window is hidden.
- Never print or commit secrets (`.env`, sessions, users, registry, logs).
- Build: `cargo build --release --workspace --locked` (Rust ≥ 1.98); test: `cargo test --workspace --locked`. Keep `Cargo.lock` (Slint is pinned).
- Slint limits: no backdrop blur, no blend modes, no gradient text, `slint::Image` is not `Send`. Glass + the web's moving orbs are
  therefore rendered on the CPU (`crates/nativefs-app/src/bg.rs`) into a small texture.
- Slint 1.16 bug: an `if` element placed directly in a layout gets a wrong `absolute-position` (padding counted twice); wrap it in a layout.
- Video player = libmpv loaded at runtime. Windows: run `tools/fetch-libmpv.sh` once (puts `libmpv-2.dll` into `vendor/libmpv/`,
  `build.rs` copies it next to the exe; without it everything else works). Linux: install `mpv` (libmpv) and `ffmpeg` (camera).
- `build.rs` compiles the Slint UI in a thread with a 64 MB stack (Windows' 1 MB main-thread stack overflowed on the big windows).
- Slint `changed` handlers on properties inside `for` rows did not fire reliably; use an `if` element whose `init` runs instead
  (see the search reveal in `ui/reader.slint`). Ids inside `if` / `for` cannot be read from outside.
- Testing on a real desktop: `NATIVEFS_CONFIG=<file>` uses a throw-away config; quit a running instance (tray → Quit) before starting a
  new build, otherwise the new process hands over to the old one and exits. The version/build date is on the app settings page (gear icon).
