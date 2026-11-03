# libmpv (video player)

The player (`crates/nativefs-app/src/player/`) loads libmpv at runtime (`libloading`); nothing is linked
at build time, so a missing library only disables the player.

| OS | Where libmpv comes from |
|---|---|
| Windows x64 / ARM64 | `libmpv-2.dll` shipped next to `naTiVeFS.exe`. Fetched by `tools/fetch-libmpv.sh` into `vendor/libmpv/win-<arch>/` (not in git, not in the kits: ~120 MB / ~84 MB). `build.rs` copies it to `target/<profile>/`. |
| Linux | the system library `libmpv.so.2` (Arch: `mpv`, Debian/Ubuntu: `libmpv2`). The AppImage must bundle it (stage 5). |
| macOS | Homebrew `libmpv` (`/opt/homebrew/lib`) or a copy in the app bundle (`Contents/Frameworks`), best effort. |

Windows build: shinchiro/mpv-winbuild-cmake, tag `20261021` (mpv git `e471496c56`), files
`mpv-dev-x86_64-20261021-git-e471496c56.7z` and `mpv-dev-aarch64-20261021-git-e471496c56.7z`
(SHA-256 in the script). It contains FFmpeg and other libraries statically.

**Licence:** mpv is GPL-2.0-or-later (this build includes GPL parts such as x264), which is compatible with naTiVeFS'
GPL-3.0-or-later. Releases that ship the DLL must point to the sources: mpv (github.com/mpv-player/mpv, commit
above) and the build recipe (github.com/shinchiro/mpv-winbuild-cmake). The app's settings page lists libmpv among the
open-source libraries.
