# naTiVeFS

Native Rust desktop client for [TVFS](https://github.com/TomasekValla/TVFS) (TomasekValla Filestream System) at
`files.tomasekvalla.cz`: upload and manage your files, open TVFS links in a local video player, reader and (bit-perfect) audio player.
No Electron, no webview: Rust + [Slint](https://slint.dev).

**Status:** pre-release (0.9.x coming). Windows x64/ARM64, Linux x64/ARM64, macOS ARM64 (best effort).
See `docs/DECISIONS.md` for why things work the way they do and `docs/TVFS_API_NOTES.md` for the server API.

Requires Rust 1.98 or newer (no other build tools on Windows besides the MSVC C++ build tools; Linux build packages are listed in `.github/workflows/ci.yml`).

```
cargo build --release --workspace --locked
cargo run --release -p nativefs-app        # builds target/release/naTiVeFS(.exe)
```

`vendor/` holds patched copies of crates.io crates (see `vendor/README.md`).

License: GPL-3.0-or-later.
