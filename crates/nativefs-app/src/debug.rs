//! Test switches: `NATIVEFS_*` environment variables that only debug builds read (release builds see every one of
//! them as unset). The list with what each does is in `docs/DEBUG_SWITCHES.md`. Variables that also work in release
//! builds (`NATIVEFS_CONFIG`, `NATIVEFS_BASE_URL`) are read by `tvfs-core` directly.

/// The value of a debug switch, `None` in release builds.
pub fn var(name: &str) -> Option<String> {
    if cfg!(debug_assertions) {
        std::env::var(name).ok()
    } else {
        None
    }
}

/// A debug switch is set (to anything); always `false` in release builds.
pub fn flag(name: &str) -> bool {
    cfg!(debug_assertions) && std::env::var_os(name).is_some()
}
