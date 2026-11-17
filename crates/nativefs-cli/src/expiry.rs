//! Expiry presets as typed on the command line (`5m` ... `14d`), matching the server's list.

use tvfs_core::EXPIRATION_OPTIONS_MIN;

pub const LABELS: [&str; 13] = ["5m", "30m", "1h", "6h", "12h", "1d", "2d", "3d", "5d", "7d", "10d", "14d", "30d"];

pub fn parse(s: &str) -> Option<u32> {
    let s = s.trim().to_ascii_lowercase();
    LABELS.iter().position(|l| *l == s).map(|i| EXPIRATION_OPTIONS_MIN[i])
}

pub fn label(minutes: u32) -> &'static str {
    EXPIRATION_OPTIONS_MIN.iter().position(|m| *m == minutes).map_or("7d", |i| LABELS[i])
}

pub fn index(minutes: u32) -> usize {
    EXPIRATION_OPTIONS_MIN.iter().position(|m| *m == minutes).unwrap_or(9)
}

pub fn labels() -> String {
    LABELS.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_map_to_minutes() {
        assert_eq!(parse("5m"), Some(5));
        assert_eq!(parse("1H"), Some(60));
        assert_eq!(parse("14d"), Some(20160));
        assert_eq!(parse("4d"), None);
        for (l, m) in LABELS.iter().zip(EXPIRATION_OPTIONS_MIN) {
            assert_eq!(label(m), *l);
        }
    }
}
