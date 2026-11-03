# Vendored crates

Patched copies of crates.io crates, wired in with `[patch.crates-io]` in the workspace `Cargo.toml`.

| Crate | Version | License | Why patched |
|---|---|---|---|
| `avif-decode` | 3.0.0 | BSD-3-Clause (see `avif-decode/LICENSE`) | Upstream enables rav1d's x86 assembly, which needs NASM at build time. naTiVeFS builds rav1d without assembly on every target (pure Rust, no extra build tools) and uses one decoder thread per image. Only `Cargo.toml` (rav1d dependency) and `src/lib.rs` (thread count) differ from upstream. |
