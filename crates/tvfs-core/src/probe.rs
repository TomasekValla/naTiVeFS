//! The File Check page (web `check.html`): `POST /api/probe` returns an ffprobe summary of a
//! files.tomasekvalla.cz URL; this turns it into the rows the page shows, with the web's formatting.

use serde_json::Value;

use crate::fmt::to_fixed;

/// One label/value pair of a card (`stat()` in check.html).
#[derive(Debug, Clone, PartialEq)]
pub struct Stat {
    pub label: &'static str,
    pub value: String,
    /// Shown in the accent colour.
    pub accent: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamKind {
    Video,
    Audio,
    Other,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StreamView {
    pub kind: StreamKind,
    /// `codec_type` as the badge shows it ("VIDEO", "AUDIO", "SUBTITLE", "UNKNOWN", ...).
    pub badge: String,
    pub codec: String,
    pub codec_long: String,
    /// Empty for streams that are neither video nor audio (the web shows only the header).
    pub stats: Vec<Stat>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProbeView {
    /// `None` = "No format info".
    pub format: Option<Vec<Stat>>,
    pub streams: Vec<StreamView>,
    /// `JSON.stringify(data.raw, null, 2)`, keys in the server's order.
    pub raw: String,
}

/// Build the page from the response body of `/api/probe`.
pub fn parse(body: &str) -> Option<ProbeView> {
    let data: Value = serde_json::from_str(body).ok()?;
    let format = data.get("format").filter(|f| f.is_object()).map(|f| {
        vec![
            stat("Format", f.get("format_long_name").and_then(js_str).or_else(|| f.get("format_name").and_then(js_str)), true),
            stat("Duration", Some(fmt_duration(f.get("duration"))), true),
            stat("File Size", Some(fmt_bytes(f.get("size"))), false),
            stat("Overall Bitrate", Some(fmt_bitrate(f.get("bit_rate"))), true),
            stat("Streams", f.get("nb_streams").and_then(js_str), false),
            stat("Filename", f.get("filename").and_then(js_str).map(|n| url_file_name(&n)), false),
        ]
    });
    let streams = data
        .get("streams")
        .and_then(Value::as_array)
        .map(|list| list.iter().map(stream_view).collect())
        .unwrap_or_default();
    let raw = member_text(body, "raw").map(pretty_json).unwrap_or_else(|| "undefined".into());
    Some(ProbeView { format, streams, raw })
}

fn stream_view(s: &Value) -> StreamView {
    let ty = s.get("codec_type").and_then(js_str).unwrap_or_else(|| "unknown".into());
    let get = |k: &str| s.get(k);
    let (kind, stats) = match ty.as_str() {
        "video" => {
            let res = match (get("width").and_then(js_str), get("height").and_then(js_str)) {
                (Some(w), Some(h)) => format!("{w} × {h}"),
                _ => "—".into(),
            };
            (
                StreamKind::Video,
                vec![
                    stat("Resolution", Some(res), true),
                    stat("Frame Rate", Some(fmt_frame_rate(get("r_frame_rate"))), true),
                    stat("Avg Frame Rate", Some(fmt_frame_rate(get("avg_frame_rate"))), false),
                    stat("Bitrate", Some(fmt_bitrate(get("bit_rate"))), true),
                    stat("Pixel Format", get("pix_fmt").and_then(js_str), false),
                    stat("Profile", get("profile").and_then(js_str), false),
                    stat("Color Space", get("color_space").and_then(js_str), false),
                    stat("Color Range", get("color_range").and_then(js_str), false),
                    stat("Duration", Some(fmt_duration(get("duration"))), false),
                    stat("Frames", get("nb_frames").and_then(js_str), false),
                ],
            )
        }
        "audio" => (
            StreamKind::Audio,
            vec![
                stat("Sample Rate", get("sample_rate").and_then(js_str).map(|r| format!("{r} Hz")), true),
                stat("Channels", get("channels").and_then(js_str), true),
                stat("Layout", get("channel_layout").and_then(js_str), false),
                stat("Bitrate", Some(fmt_bitrate(get("bit_rate"))), true),
                stat("Duration", Some(fmt_duration(get("duration"))), false),
            ],
        ),
        _ => (StreamKind::Other, Vec::new()),
    };
    StreamView {
        kind,
        badge: ty.to_uppercase(),
        codec: get("codec_name").and_then(js_str).unwrap_or_else(|| "—".into()),
        codec_long: get("codec_long_name").and_then(js_str).unwrap_or_default(),
        stats,
    }
}

/// `stat(label, value || '—')`.
fn stat(label: &'static str, value: Option<String>, accent: bool) -> Stat {
    Stat { label, value: value.filter(|v| !v.is_empty()).unwrap_or_else(|| "—".into()), accent }
}

/// A JSON value as JavaScript would put it into a template, or `None` when it is falsy
/// (missing, null, "", 0, false).
fn js_str(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => {
            let f = n.as_f64()?;
            if f == 0.0 || f.is_nan() {
                None
            } else if let Some(i) = n.as_i64() {
                Some(i.to_string())
            } else if let Some(u) = n.as_u64() {
                Some(u.to_string())
            } else {
                Some(f.to_string())
            }
        }
        Value::Bool(true) => Some("true".into()),
        _ => None,
    }
}

/// `parseFloat` / `parseInt` of a field (a string or a number in ffprobe's output); `None` when the
/// field is falsy, like the web's `if (!x) return '—'`.
fn js_number(v: Option<&Value>, int: bool) -> Option<f64> {
    let v = v?;
    let s = js_str(v)?;
    let t = s.trim_start();
    // the longest numeric prefix, as parseFloat/parseInt read it
    let mut end = 0;
    let bytes = t.as_bytes();
    let mut seen_dot = false;
    let mut seen_exp = false;
    while end < bytes.len() {
        let c = bytes[end];
        let ok = c.is_ascii_digit()
            || ((c == b'-' || c == b'+') && (end == 0 || (!int && matches!(bytes[end - 1], b'e' | b'E'))))
            || (!int && c == b'.' && !seen_dot && !seen_exp)
            || (!int && (c == b'e' || c == b'E') && !seen_exp && end > 0);
        if !ok {
            break;
        }
        seen_dot |= c == b'.';
        seen_exp |= c == b'e' || c == b'E';
        end += 1;
    }
    let mut num = &t[..end];
    while num.ends_with(['e', 'E', '+', '-']) {
        num = &num[..num.len() - 1];
    }
    let x: f64 = num.parse().ok()?;
    Some(if int { x.trunc() } else { x })
}

/// check.html `fmtBytes`: "12.34 MB" (binary units).
fn fmt_bytes(v: Option<&Value>) -> String {
    let Some(b) = js_number(v, true).filter(|b| *b >= 1.0) else { return "—".into() };
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let i = ((b.ln() / 1024f64.ln()).floor() as usize).min(UNITS.len() - 1);
    format!("{} {}", to_fixed(b / 1024f64.powi(i as i32), 2), UNITS[i])
}

/// check.html `fmtDuration`: "1h 2m 3.456s", "2m 3.456s", "3.456s".
fn fmt_duration(v: Option<&Value>) -> String {
    let Some(s) = js_number(v, false) else { return "—".into() };
    let h = (s / 3600.0).floor();
    let m = ((s % 3600.0) / 60.0).floor();
    let sec = to_fixed(s % 60.0, 3);
    if h > 0.0 {
        format!("{h}h {m}m {sec}s")
    } else if m > 0.0 {
        format!("{m}m {sec}s")
    } else {
        format!("{sec}s")
    }
}

/// check.html `fmtBitrate`: "4.20 Mbps" / "128 kbps".
fn fmt_bitrate(v: Option<&Value>) -> String {
    let Some(bps) = js_number(v, true) else { return "—".into() };
    if bps >= 1_000_000.0 {
        format!("{} Mbps", to_fixed(bps / 1_000_000.0, 2))
    } else {
        format!("{} kbps", to_fixed(bps / 1000.0, 0))
    }
}

/// check.html `fmtFrameRate`: "30000/1001" -> "29.97 fps".
fn fmt_frame_rate(v: Option<&Value>) -> String {
    let Some(fr) = v.and_then(js_str) else { return "—".into() };
    if fr == "0/0" {
        return "—".into();
    }
    let Some((a, b)) = fr.split_once('/') else { return format!("{fr} fps") };
    let num = |s: &str| if s.trim().is_empty() { Some(0.0) } else { s.trim().parse::<f64>().ok() };
    let (a, b) = (num(a).unwrap_or(f64::NAN), num(b.split('/').next().unwrap_or("")).unwrap_or(f64::NAN));
    if b == 0.0 {
        return "—".into();
    }
    // toFixed(3) then .replace(/\.?0+$/, '')
    let s = to_fixed(a / b, 3);
    let s = s.trim_end_matches('0');
    let s = s.strip_suffix('.').unwrap_or(s);
    format!("{s} fps")
}

/// `new URL(name).pathname.split('/').pop()`, or the name itself when it is not a URL.
fn url_file_name(name: &str) -> String {
    match url::Url::parse(name) {
        Ok(u) => u.path().rsplit('/').next().unwrap_or("").to_string(),
        Err(_) => name.to_string(),
    }
}

/// The source text of member `key` of the top-level JSON object in `json`.
fn member_text<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let b = json.as_bytes();
    let mut i = skip_ws(b, 0);
    if b.get(i) != Some(&b'{') {
        return None;
    }
    i += 1;
    loop {
        i = skip_ws(b, i);
        if b.get(i) != Some(&b'"') {
            return None;
        }
        let k_end = string_end(b, i)?;
        let name: String = serde_json::from_str(&json[i..k_end]).ok()?;
        i = skip_ws(b, k_end);
        if b.get(i) != Some(&b':') {
            return None;
        }
        let v_start = skip_ws(b, i + 1);
        let v_end = value_end(b, v_start)?;
        if name == key {
            return Some(&json[v_start..v_end]);
        }
        i = skip_ws(b, v_end);
        match b.get(i) {
            Some(b',') => i += 1,
            _ => return None,
        }
    }
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n' | b'\r') {
        i += 1;
    }
    i
}

/// `b[start]` is a quote; returns the index after the closing quote.
fn string_end(b: &[u8], start: usize) -> Option<usize> {
    let mut i = start + 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'"' => return Some(i + 1),
            _ => i += 1,
        }
    }
    None
}

/// Index after the JSON value that starts at `start`.
fn value_end(b: &[u8], start: usize) -> Option<usize> {
    match *b.get(start)? {
        b'"' => string_end(b, start),
        b'{' | b'[' => {
            let mut depth = 0usize;
            let mut i = start;
            while i < b.len() {
                match b[i] {
                    b'"' => {
                        i = string_end(b, i)?;
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(i + 1);
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            None
        }
        _ => {
            let mut i = start;
            while i < b.len() && !matches!(b[i], b',' | b'}' | b']' | b' ' | b'\t' | b'\n' | b'\r') {
                i += 1;
            }
            (i > start).then_some(i)
        }
    }
}

/// Re-indent JSON text like `JSON.stringify(value, null, 2)`, keeping keys and tokens as sent.
pub fn pretty_json(json: &str) -> String {
    let b = json.as_bytes();
    let mut out = String::with_capacity(json.len() * 2);
    let mut depth = 0usize;
    let mut i = 0;
    let newline = |out: &mut String, depth: usize| {
        out.push('\n');
        for _ in 0..depth {
            out.push_str("  ");
        }
    };
    while i < b.len() {
        match b[i] {
            b' ' | b'\t' | b'\n' | b'\r' => i += 1,
            b'"' => {
                let end = string_end(b, i).unwrap_or(b.len());
                out.push_str(&json[i..end]);
                i = end;
            }
            open @ (b'{' | b'[') => {
                let close = if open == b'{' { b'}' } else { b']' };
                let next = skip_ws(b, i + 1);
                if b.get(next) == Some(&close) {
                    out.push(open as char);
                    out.push(close as char);
                    i = next + 1;
                } else {
                    out.push(open as char);
                    depth += 1;
                    newline(&mut out, depth);
                    i += 1;
                }
            }
            close @ (b'}' | b']') => {
                depth = depth.saturating_sub(1);
                newline(&mut out, depth);
                out.push(close as char);
                i += 1;
            }
            b',' => {
                out.push(',');
                newline(&mut out, depth);
                i += 1;
            }
            b':' => {
                out.push_str(": ");
                i += 1;
            }
            _ => {
                let start = i;
                while i < b.len() && !matches!(b[i], b',' | b'}' | b']' | b':' | b' ' | b'\t' | b'\n' | b'\r' | b'"') {
                    i += 1;
                }
                out.push_str(&json[start..i]);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn v(x: Value) -> Option<Value> {
        Some(x)
    }

    #[test]
    fn web_number_formats() {
        assert_eq!(fmt_bytes(v(json!("1536")).as_ref()), "1.50 KB");
        assert_eq!(fmt_bytes(v(json!("41943040")).as_ref()), "40.00 MB");
        assert_eq!(fmt_bytes(v(json!("512")).as_ref()), "512.00 B");
        assert_eq!(fmt_bytes(None), "—");
        assert_eq!(fmt_duration(v(json!("3725.5")).as_ref()), "1h 2m 5.500s");
        assert_eq!(fmt_duration(v(json!("125.5")).as_ref()), "2m 5.500s");
        assert_eq!(fmt_duration(v(json!("4.200000")).as_ref()), "4.200s");
        assert_eq!(fmt_duration(v(json!("")).as_ref()), "—");
        assert_eq!(fmt_bitrate(v(json!("4200000")).as_ref()), "4.20 Mbps");
        assert_eq!(fmt_bitrate(v(json!("128000")).as_ref()), "128 kbps");
        assert_eq!(fmt_bitrate(v(json!("0")).as_ref()), "0 kbps");
        assert_eq!(fmt_bitrate(None), "—");
        assert_eq!(fmt_frame_rate(v(json!("30000/1001")).as_ref()), "29.97 fps");
        assert_eq!(fmt_frame_rate(v(json!("30/1")).as_ref()), "30 fps");
        assert_eq!(fmt_frame_rate(v(json!("1/2")).as_ref()), "0.5 fps");
        assert_eq!(fmt_frame_rate(v(json!("100/1")).as_ref()), "100 fps");
        assert_eq!(fmt_frame_rate(v(json!("0/0")).as_ref()), "—");
        assert_eq!(fmt_frame_rate(v(json!("25/0")).as_ref()), "—");
        assert_eq!(fmt_frame_rate(v(json!("24")).as_ref()), "24 fps");
    }

    #[test]
    fn falsy_values_show_a_dash() {
        assert_eq!(stat("Channels", js_str(&json!(0)), true).value, "—");
        assert_eq!(js_str(&json!(0)), None);
        assert_eq!(js_str(&json!("")), None);
        assert_eq!(js_str(&json!(null)), None);
        assert_eq!(js_str(&json!(2)), Some("2".into()));
        assert_eq!(js_str(&json!("0")), Some("0".into()));
    }

    #[test]
    fn file_name_from_url() {
        assert_eq!(url_file_name("https://files.tomasekvalla.cz/files/videos/a%20b.mp4"), "a%20b.mp4");
        assert_eq!(url_file_name("https://files.tomasekvalla.cz/files/videos/a b.mp4"), "a%20b.mp4");
        assert_eq!(url_file_name("clip.mp4"), "clip.mp4");
    }

    #[test]
    fn pretty_json_matches_json_stringify() {
        let src = r#"{"streams":[{"index":0,"tags":{}},{"index":1,"x":[]}],"format":{"filename":"a, \"b\": c","size":"12"}}"#;
        let want = "{\n  \"streams\": [\n    {\n      \"index\": 0,\n      \"tags\": {}\n    },\n    {\n      \"index\": 1,\n      \"x\": []\n    }\n  ],\n  \"format\": {\n    \"filename\": \"a, \\\"b\\\": c\",\n    \"size\": \"12\"\n  }\n}";
        assert_eq!(pretty_json(src), want);
    }

    #[test]
    fn parses_a_probe_response() {
        let body = r#"{"streams":[
            {"index":0,"codec_type":"video","codec_name":"h264","codec_long_name":"H.264 / AVC","width":1920,"height":1080,
             "r_frame_rate":"30000/1001","avg_frame_rate":"30000/1001","pix_fmt":"yuv420p","bit_rate":"4200000","profile":"High",
             "duration":"10.010000","nb_frames":"300"},
            {"index":1,"codec_type":"audio","codec_name":"aac","codec_long_name":"AAC","sample_rate":"48000","channels":2,
             "channel_layout":"stereo","bit_rate":"128000","duration":"10.000000"},
            {"index":2,"codec_type":"subtitle","codec_name":"mov_text"}],
          "format":{"filename":"https://files.tomasekvalla.cz/files/videos/clip.mp4","format_name":"mov,mp4","format_long_name":"QuickTime / MOV",
            "duration":"10.010000","size":"5400000","bit_rate":"4316000","nb_streams":3},
          "raw":{"streams":[],"format":{"z":1,"a":2}}}"#;
        let p = parse(body).unwrap();
        let f = p.format.unwrap();
        let vals: Vec<_> = f.iter().map(|s| s.value.as_str()).collect();
        assert_eq!(vals, ["QuickTime / MOV", "10.010s", "5.15 MB", "4.32 Mbps", "3", "clip.mp4"]);
        assert_eq!(p.streams.len(), 3);
        assert_eq!(p.streams[0].kind, StreamKind::Video);
        assert_eq!(p.streams[0].stats[0].value, "1920 × 1080");
        assert_eq!(p.streams[0].stats[1].value, "29.97 fps");
        assert_eq!(p.streams[0].stats[6].value, "—"); // color_space missing
        assert_eq!(p.streams[1].stats[0].value, "48000 Hz");
        assert_eq!(p.streams[1].stats[1].value, "2");
        assert_eq!(p.streams[2].kind, StreamKind::Other);
        assert_eq!(p.streams[2].badge, "SUBTITLE");
        assert!(p.streams[2].stats.is_empty());
        assert_eq!(p.streams[2].codec_long, "");
        // key order as sent (z before a), like JSON.stringify
        assert_eq!(p.raw, "{\n  \"streams\": [],\n  \"format\": {\n    \"z\": 1,\n    \"a\": 2\n  }\n}");
    }

    #[test]
    fn no_format_info() {
        let p = parse(r#"{"streams":[],"format":null,"raw":{}}"#).unwrap();
        assert!(p.format.is_none());
        assert_eq!(p.raw, "{}");
    }
}
