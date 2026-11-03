/// Human readable size, e.g. `1.5 MB` (binary units, like the web UI).
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// "expires in 3d 4h" / "expired" / "no expiry" from epoch milliseconds.
pub fn describe_expiry(expires_at_ms: Option<i64>, now_ms: i64) -> String {
    let Some(at) = expires_at_ms else { return "no expiry".into() };
    let left = at - now_ms;
    if left <= 0 {
        return "expired".into();
    }
    let mins = left / 60_000;
    let (d, h, m) = (mins / 1440, (mins % 1440) / 60, mins % 60);
    match (d, h) {
        (0, 0) => format!("expires in {m}m"),
        (0, _) => format!("expires in {h}h {m}m"),
        _ => format!("expires in {d}d {h}h"),
    }
}

/// "6d 23h" / "3h 12m" / "12m" for a remaining time in milliseconds (like the web UI).
pub fn countdown(left_ms: i64) -> String {
    let mins = (left_ms.max(0)) / 60_000;
    let (d, h, m) = (mins / 1440, (mins % 1440) / 60, mins % 60);
    match (d, h) {
        (0, 0) => format!("{m}m"),
        (0, _) => format!("{h}h {m}m"),
        _ => format!("{d}d {h}h"),
    }
}

/// JavaScript's `Number.prototype.toFixed`: like Rust's `{:.d}` (both round the
/// exact binary value), except that an exact tie rounds up instead of to even
/// (`(1.25).toFixed(1)` is `"1.3"`, Rust gives `"1.2"`).
pub fn to_fixed(x: f64, digits: usize) -> String {
    if x.is_finite() && x >= 0.0 && digits <= 6 && is_exact_tie(x, digits as u32) {
        let p = 10f64.powi(digits as i32);
        return format!("{:.*}", digits, ((x * p).floor() + 1.0) / p);
    }
    format!("{x:.digits$}")
}

/// Is `x * 10^d` exactly `n + 1/2`? Decided on the exact binary value of `x`.
fn is_exact_tie(x: f64, d: u32) -> bool {
    let bits = x.to_bits();
    let exp = ((bits >> 52) & 0x7ff) as i32;
    let frac = bits & ((1u64 << 52) - 1);
    let (m, e) = if exp == 0 { (frac, -1074) } else { (frac | (1u64 << 52), exp - 1075) };
    if m == 0 || e >= 0 {
        return false;
    }
    let shift = (-e) as u32;
    if shift >= 127 {
        return false;
    }
    // x * 10^d = m * 10^d / 2^shift; a tie means the remainder is exactly half the divisor
    let num = m as u128 * 10u128.pow(d);
    num % (1u128 << shift) == 1u128 << (shift - 1)
}

/// The web's `fmtBytes`: two decimals, e.g. `1.50 MB` (progress, file list, sizes).
pub fn fmt_bytes(bytes: u64) -> String {
    if bytes == 0 {
        return "0 B".into();
    }
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let i = ((bytes as f64).ln() / 1024f64.ln()).floor().clamp(0.0, 4.0) as i32;
    format!("{} {}", to_fixed(bytes as f64 / 1024f64.powi(i), 2), UNITS[i as usize])
}

/// `fmtTime`: `42s`, `3m 7s`, `1h 12m`.
pub fn fmt_time(s: f64) -> String {
    let s = s.max(0.0);
    if s < 60.0 {
        format!("{}s", s.round())
    } else if s < 3600.0 {
        format!("{}m {}s", (s / 60.0).floor(), (s % 60.0).round())
    } else {
        format!("{}h {}m", (s / 3600.0).floor(), ((s % 3600.0) / 60.0).floor())
    }
}

/// `fmtSpeedMBs`: `1.3 MB/s`, below 1 MB/s in KB/s.
pub fn fmt_speed_mbs(mbs: f64) -> String {
    if mbs < 1.0 {
        format!("{} KB/s", to_fixed(mbs * 1024.0, 0))
    } else {
        format!("{} MB/s", to_fixed(mbs, 1))
    }
}

/// `fmtSpeedMbps`: bits, `12.0 Mbps`, below 1 Mbps in Kbps.
pub fn fmt_speed_mbps(mbs: f64) -> String {
    let mbps = mbs * 8.0;
    if mbps < 1.0 {
        format!("{} Kbps", to_fixed(mbps * 1024.0, 0))
    } else {
        format!("{} Mbps", to_fixed(mbps, 1))
    }
}

/// `fmtSpeedMBsParts`: value and unit separately (stats tiles).
pub fn fmt_speed_mbs_parts(mbs: f64) -> (String, &'static str) {
    if mbs < 1.0 {
        (to_fixed(mbs * 1024.0, 0), "KB/s")
    } else {
        (to_fixed(mbs, 1), "MB/s")
    }
}

/// Duration on the stats card: `0.42 s`, `12.3 s`, `2 m 05 s`.
pub fn fmt_elapsed_stats(sec: f64) -> String {
    if sec < 1.0 {
        format!("{} s", to_fixed(sec.max(0.01), 2))
    } else if sec < 60.0 {
        format!("{} s", to_fixed(sec, 1))
    } else {
        let (mut m, mut s) = ((sec / 60.0).floor() as u64, (sec % 60.0).round() as u64);
        if s == 60 {
            m += 1;
            s = 0;
        }
        format!("{m} m {s:02} s")
    }
}

/// "1 GB would upload in ...": `8.2 s`, `41 s`, `3 m 09 s`, `2 h 5 m`.
pub fn fmt_1gb_time(sec: f64) -> String {
    if sec < 60.0 {
        return if sec < 10.0 { format!("{} s", to_fixed(sec, 1)) } else { format!("{} s", sec.round()) };
    }
    let (mut m, mut s) = ((sec / 60.0).floor() as u64, (sec % 60.0).round() as u64);
    if s == 60 {
        m += 1;
        s = 0;
    }
    if m < 60 {
        return format!("{m} m {s:02} s");
    }
    let (h, m) = (m / 60, m % 60);
    if m > 0 {
        format!("{h} h {m} m")
    } else {
        format!("{h} h")
    }
}

/// `fmtCountdown` of the recent list: `06:23:59:59` (dd:hh:mm:ss), `None` when elapsed.
pub fn fmt_countdown(left_ms: i64) -> Option<String> {
    if left_ms <= 0 {
        return None;
    }
    let t = left_ms / 1000;
    Some(format!("{:02}:{:02}:{:02}:{:02}", t / 86_400, (t % 86_400) / 3600, (t % 3600) / 60, t % 60))
}

/// `truncateFilename`: `really_long_fil....mp4` (keeps the extension when it is short).
pub fn truncate_filename(name: &str, max_len: usize) -> String {
    let chars: Vec<char> = name.chars().collect();
    if chars.len() <= max_len {
        return name.to_owned();
    }
    let dot = chars.iter().rposition(|c| *c == '.');
    let ext_len = match dot {
        Some(d) if d > 0 && chars.len() - d <= 6 => chars.len() - d,
        _ => 0,
    };
    let base = &chars[..chars.len() - ext_len];
    let ext = &chars[chars.len() - ext_len..];
    let keep = max_len.saturating_sub(ext_len + 3).max(3).min(base.len());
    format!("{}...{}", base[..keep].iter().collect::<String>(), ext.iter().collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_formats() {
        assert_eq!(fmt_bytes(0), "0 B");
        assert_eq!(fmt_bytes(1536), "1.50 KB");
        assert_eq!(fmt_bytes(3 * 1024 * 1024 * 1024), "3.00 GB");
        assert_eq!(fmt_time(42.4), "42s");
        assert_eq!(fmt_time(187.0), "3m 7s");
        assert_eq!(fmt_time(4320.0), "1h 12m");
        assert_eq!(fmt_speed_mbs(0.5), "512 KB/s");
        assert_eq!(fmt_speed_mbs(1.25), "1.3 MB/s");
        assert_eq!(fmt_speed_mbps(1.5), "12.0 Mbps");
        assert_eq!(fmt_speed_mbps(0.1), "819 Kbps");
        assert_eq!(fmt_elapsed_stats(0.001), "0.01 s");
        assert_eq!(fmt_elapsed_stats(125.0), "2 m 05 s");
        assert_eq!(fmt_elapsed_stats(119.7), "2 m 00 s");
        assert_eq!(fmt_1gb_time(8.24), "8.2 s");
        assert_eq!(fmt_1gb_time(7500.0), "2 h 5 m");
        assert_eq!(fmt_countdown(0), None);
        assert_eq!(fmt_countdown(6 * 86_400_000 + 23 * 3_600_000 + 59 * 60_000 + 59_999).unwrap(), "06:23:59:59");
        assert_eq!(truncate_filename("short.mp4", 26), "short.mp4");
        assert_eq!(truncate_filename("really_long_filename_thing_here.mp4", 26), "really_long_filenam....mp4");
        assert_eq!(truncate_filename("abcdefghijklmnopqrstuvwxyz0123", 26), "abcdefghijklmnopqrstuvw...");
        // JS toFixed: exact ties round up, everything else as Rust
        assert_eq!(to_fixed(1.25, 1), "1.3");
        assert_eq!(to_fixed(0.125, 2), "0.13");
        assert_eq!(to_fixed(2.5, 0), "3");
        assert_eq!(to_fixed(1.005, 2), "1.00"); // 1.00499999999999989...
        assert_eq!(to_fixed(0.15, 1), "0.1"); // 0.1499999999999999944...
        assert_eq!(to_fixed(1.35, 1), "1.4"); // 1.350000000000000088...
        assert_eq!(to_fixed(0.0, 2), "0.00");
    }

    #[test]
    fn bytes() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(5 * 1024 * 1024 * 1024), "5.0 GB");
    }

    #[test]
    fn countdown_format() {
        assert_eq!(countdown(90 * 60_000), "1h 30m");
        assert_eq!(countdown(6 * 86_400_000 + 23 * 3_600_000), "6d 23h");
        assert_eq!(countdown(-5), "0m");
    }

    #[test]
    fn expiry() {
        assert_eq!(describe_expiry(None, 0), "no expiry");
        assert_eq!(describe_expiry(Some(10), 100), "expired");
        assert_eq!(describe_expiry(Some(90 * 60_000), 0), "expires in 1h 30m");
        assert_eq!(describe_expiry(Some(3 * 86_400_000 + 4 * 3_600_000), 0), "expires in 3d 4h");
    }
}
