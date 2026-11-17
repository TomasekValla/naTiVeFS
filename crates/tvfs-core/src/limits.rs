//! The upload rules of the logged-in account (`GET /api/config`) and the expiry picker computed from them, the same
//! way the web page does: options a login can never use are hidden, options blocked only for now (file size, free
//! space, the host's time to midnight) are greyed out. The server applies the same rules itself at upload time.

use serde::Deserialize;

use crate::EXPIRATION_OPTIONS_MIN;

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SizeStep {
    pub up_to: u64,
    pub max_minutes: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpaceStep {
    pub up_to: f64,
    pub factor: f64,
}

/// `GET /api/config`
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadConfig {
    /// 0 = host, 1-3 = accounts (3 = admin).
    pub tier: i32,
    #[serde(default)]
    pub tier_max_minutes: Option<u32>,
    /// Host only: the next Prague midnight (ms since 1970), when host uploads and the host login end.
    #[serde(default)]
    pub until_midnight: Option<u64>,
    #[serde(default)]
    pub size_steps: Vec<SizeStep>,
    #[serde(default)]
    pub space_steps: Vec<SpaceStep>,
    #[serde(default)]
    pub max_file_bytes: u64,
    #[serde(default)]
    pub available_bytes: Option<u64>,
    /// What this account already keeps on the server (counts in the space rule).
    #[serde(default)]
    pub stored_bytes: u64,
}

/// How an option of the expiry picker is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PillState {
    Normal,
    /// Not possible right now (file size, free space, time to midnight).
    Grey,
    /// Never possible for this login: not shown at all.
    Hidden,
}

/// The longest expiry (minutes) for a file of `size` bytes right now; 0 = it cannot be uploaded now.
/// Same rule as the web's `allowedMinutesFor` and the server's `maxExpiryMinutes`.
pub fn allowed_minutes(c: &UploadConfig, size: u64, now_ms: u64) -> u32 {
    if c.max_file_bytes > 0 && size > c.max_file_bytes {
        return 0;
    }
    if c.available_bytes.is_some_and(|a| size > a) {
        return 0;
    }
    if c.tier == 0 {
        let left = c.until_midnight.unwrap_or(now_ms).saturating_sub(now_ms) / 60_000;
        return (left as u32).max(1);
    }
    let tier_max = c.tier_max_minutes.unwrap_or(0);
    if c.size_steps.is_empty() {
        return tier_max;
    }
    let mut max = c.size_steps.iter().find(|s| size <= s.up_to).map_or(0, |s| s.max_minutes);
    if let Some(avail) = c.available_bytes.filter(|a| *a > 0) {
        let share = (size + c.stored_bytes) as f64 / avail as f64;
        let by_share = c
            .space_steps
            .iter()
            .find(|s| share <= s.up_to)
            .map_or(1440, |s| ((tier_max as f64 * s.factor).floor() as u32).max(1440));
        max = max.min(by_share);
    }
    max
}

/// The most this login can ever choose (host: under a day, the rest: the tier's maximum).
pub fn ceiling_minutes(c: &UploadConfig) -> u32 {
    if c.tier == 0 { 1439 } else { c.tier_max_minutes.unwrap_or(u32::MAX) }
}

/// State of every option in [`EXPIRATION_OPTIONS_MIN`] for the largest selected file (`biggest`, 0 when none).
pub fn pill_states(c: &UploadConfig, biggest: u64, now_ms: u64) -> Vec<PillState> {
    let allowed = allowed_minutes(c, biggest, now_ms);
    let ceiling = ceiling_minutes(c);
    EXPIRATION_OPTIONS_MIN
        .iter()
        .map(|&m| {
            if m > ceiling {
                PillState::Hidden
            } else if m > allowed {
                PillState::Grey
            } else {
                PillState::Normal
            }
        })
        .collect()
}

/// The option to use when `wanted` is not possible: the longest one that is (or `wanted` when it is fine).
pub fn usable_index(states: &[PillState], wanted: usize) -> usize {
    if states.get(wanted) == Some(&PillState::Normal) {
        return wanted;
    }
    states.iter().rposition(|s| *s == PillState::Normal).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1024 * 1024 * 1024;
    const MB: u64 = 1024 * 1024;

    fn tier(t: i32) -> UploadConfig {
        let mut c = UploadConfig { tier: t, max_file_bytes: 5 * GB, available_bytes: Some(200 * GB), ..Default::default() };
        match t {
            0 => {
                c.max_file_bytes = 400 * MB;
                c.until_midnight = Some(10_000_000 + 90 * 60_000); // 90 minutes left
            }
            1 | 2 => {
                c.tier_max_minutes = Some(if t == 1 { 10_080 } else { 20_160 });
                c.size_steps = vec![
                    SizeStep { up_to: 100 * MB, max_minutes: if t == 1 { 10_080 } else { 20_160 } },
                    SizeStep { up_to: GB, max_minutes: if t == 1 { 7_200 } else { 14_400 } },
                    SizeStep { up_to: 3 * GB, max_minutes: if t == 1 { 4_320 } else { 10_080 } },
                    SizeStep { up_to: 5 * GB, max_minutes: if t == 1 { 1_440 } else { 4_320 } },
                ];
                c.space_steps = vec![
                    SpaceStep { up_to: 0.05, factor: 1.0 },
                    SpaceStep { up_to: 0.15, factor: 0.5 },
                    SpaceStep { up_to: 0.30, factor: 0.25 },
                ];
            }
            _ => c.tier_max_minutes = Some(43_200),
        }
        c
    }

    fn labels(c: &UploadConfig, size: u64) -> String {
        let st = pill_states(c, size, 10_000_000);
        crate::EXPIRATION_OPTIONS_MIN
            .iter()
            .zip(st)
            .filter(|(_, s)| *s != PillState::Hidden)
            .map(|(m, s)| format!("{m}{}", if s == PillState::Grey { "g" } else { "" }))
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn never_possible_options_are_hidden_and_temporary_limits_grey() {
        // host: under a day, 90 minutes left today
        assert_eq!(labels(&tier(0), 0), "5 30 60 360g 720g");
        // tier 1: up to 7 days
        assert_eq!(labels(&tier(1), 0), "5 30 60 360 720 1440 2880 4320 7200 10080");
        // tier 1, a 2 GB file: 3 days at most
        assert_eq!(labels(&tier(1), 2 * GB), "5 30 60 360 720 1440 2880 4320 7200g 10080g");
        // tier 2: up to 14 days, never 30
        assert!(labels(&tier(2), 0).ends_with("20160"));
        // tier 3: everything incl. 30 days
        assert_eq!(pill_states(&tier(3), 0, 0).iter().filter(|s| **s == PillState::Normal).count(), 13);
    }

    #[test]
    fn space_rule_counts_what_is_already_stored() {
        let mut c = tier(1);
        c.available_bytes = Some(10 * GB);
        c.stored_bytes = 2 * GB; // (0 + 2 GB) / 10 GB = 20 % -> factor 0.25 of 7 days = 1.75 days
        assert_eq!(allowed_minutes(&c, 0, 0), 2520);
    }

    #[test]
    fn too_big_or_no_space_allows_nothing() {
        assert_eq!(allowed_minutes(&tier(0), 500 * MB, 0), 0);
        let mut c = tier(3);
        c.available_bytes = Some(GB);
        assert_eq!(allowed_minutes(&c, 2 * GB, 0), 0);
    }

    #[test]
    fn a_blocked_choice_falls_back_to_the_longest_allowed() {
        let st = pill_states(&tier(0), 0, 10_000_000);
        assert_eq!(usable_index(&st, 9), 2); // 7 days -> 1 h
        assert_eq!(usable_index(&st, 1), 1);
    }
}
