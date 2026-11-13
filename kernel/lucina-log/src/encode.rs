//! One record as one JSON line of at most [`MAX_RECORD`] bytes, written by hand.

use crate::level::Level;
use crate::rules;
use crate::value::{REDACTED_TEXT, Value};

/// The whole line, newline included, is at most this many bytes (docs/design/juno-logging.md section 2).
pub const MAX_LINE: usize = 2048;
/// A record is cut to this many bytes: [`MAX_LINE`] minus the newline the service adds.
pub const MAX_RECORD: usize = MAX_LINE - 1;
/// At most this many fields; the rest are dropped.
pub const MAX_FIELDS: usize = 16;
/// A field key is 1 to this many bytes of `[a-z0-9_.]`.
pub const MAX_KEY: usize = 32;
/// The message is cut to this many bytes (escaped).
pub const MAX_MSG: usize = 512;
/// What a cut string ends with.
const ELLIPSIS: &str = "\u{2026}";
/// A string field value is cut to this many bytes, so a few large fields cannot crowd the message out.
pub const MAX_STR_FIELD: usize = 256;
/// The source name and the boot id are cut to this many bytes.
const MAX_ID: usize = 32;

/// What goes into one line.
pub struct Record<'a> {
    pub wall_ms: u64,
    pub mono_ms: u64,
    pub boot: &'a str,
    pub seq: u64,
    pub level: Level,
    pub source: &'a str,
    pub pid: u32,
    pub msg: &'a str,
    pub fields: &'a [(&'a str, Value)],
}

/// Whether `key` may be a field key: 1 to 32 bytes of `[a-z0-9_.]`.
pub fn valid_key(key: &str) -> bool {
    !key.is_empty() && key.len() <= MAX_KEY && key.bytes().all(rules::is_key_byte)
}

/// `s` as the inside of a JSON string, at most `limit` bytes of it, never splitting an escape or a character. Returns the
/// text, how many bytes of `s` it covers, and whether something was left out.
fn escape_limited(s: &str, limit: usize) -> (String, usize, bool) {
    let mut out = String::with_capacity(s.len().min(limit) + 2);
    let mut kept = 0;
    for c in s.chars() {
        let mut buf = [0u8; 4];
        let esc;
        let piece: &str = match c {
            '"' => "\\\"",
            '\\' => "\\\\",
            '\n' => "\\n",
            '\r' => "\\r",
            '\t' => "\\t",
            c if (c as u32) < 0x20 => {
                esc = format!("\\u{:04x}", c as u32);
                &esc
            }
            c => c.encode_utf8(&mut buf),
        };
        if out.len() + piece.len() > limit {
            return (out, kept, true);
        }
        out.push_str(piece);
        kept += c.len_utf8();
    }
    (out, kept, false)
}

/// `s` escaped into at most `limit` bytes. When it does not fit it is cut and ends with U+2026; the second value is the number
/// of bytes of `s` left out (0 when whole).
fn cut_escape(s: &str, limit: usize) -> (String, usize) {
    let (full, _, cut) = escape_limited(s, limit);
    if !cut {
        return (full, 0);
    }
    if limit < ELLIPSIS.len() {
        return (String::new(), s.len());
    }
    let (mut part, kept, _) = escape_limited(s, limit - ELLIPSIS.len());
    part.push_str(ELLIPSIS);
    (part, s.len() - kept)
}

fn render_value(v: &Value, limit: usize, dropped: &mut usize) -> String {
    match v {
        Value::Str(s) => {
            let (e, d) = cut_escape(s, limit);
            *dropped += d;
            format!("\"{e}\"")
        }
        Value::Int(i) => i.to_string(),
        Value::UInt(u) => u.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Redacted => format!("\"{REDACTED_TEXT}\""),
    }
}

/// The bytes a field that is dropped whole counts for in `tr`: its key and its value as written.
fn field_size(k: &str, v: &Value) -> usize {
    k.len()
        + match v {
            Value::Str(s) => s.len(),
            other => render_value(other, 0, &mut 0).len(),
        }
}

struct Head<'a> {
    r: &'a Record<'a>,
    boot: String,
    source: String,
}

fn render(h: &Head<'_>, esc_msg: &str, fields: &[(&str, String)], tr: usize) -> String {
    let r = h.r;
    let mut s = String::with_capacity(256 + esc_msg.len());
    s.push_str(&format!(
        "{{\"t\":{},\"m\":{},\"boot\":\"{}\",\"seq\":{},\"lvl\":\"{}\",\"src\":\"{}\",\"pid\":{},\"msg\":\"{}\",\"f\":{{",
        r.wall_ms,
        r.mono_ms,
        h.boot,
        r.seq,
        r.level.as_str(),
        h.source,
        r.pid,
        esc_msg
    ));
    for (i, (k, v)) in fields.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push('"');
        s.push_str(k);
        s.push_str("\":");
        s.push_str(v);
    }
    s.push('}');
    if tr > 0 {
        s.push_str(&format!(",\"tr\":{tr}"));
    }
    s.push('}');
    s
}

/// The record as one JSON line without the newline, at most [`MAX_RECORD`] bytes (docs/design/juno-logging.md section 2).
///
/// Fields with an invalid key are skipped. Over 16 valid fields: the rest are dropped. `msg` is cut to [`MAX_MSG`] bytes and a
/// string field to [`MAX_STR_FIELD`], at a character boundary, and the cut text ends with U+2026. A line still too long is cut
/// in the doc's order: strings in `f` from the longest, then `msg`, then fields from the last. Whenever anything was cut the
/// record carries `"tr":N`, N being the bytes of text (UTF-8 bytes of the original message and strings, key plus value for a
/// whole field) that are not in the line, the U+2026 not counted.
pub fn encode(r: &Record<'_>) -> String {
    let head = Head {
        r,
        boot: escape_limited(r.boot, MAX_ID).0,
        source: escape_limited(r.source, MAX_ID).0,
    };
    let valid: Vec<(&str, &Value)> = r
        .fields
        .iter()
        .filter(|(k, _)| valid_key(k))
        .map(|(k, v)| (*k, v))
        .collect();
    let mut tr_fixed: usize = valid
        .iter()
        .skip(MAX_FIELDS)
        .map(|(k, v)| field_size(k, v))
        .sum();
    let mut n = valid.len().min(MAX_FIELDS);
    let mut msg_limit = MAX_MSG;
    // per-field limit for strings; other values ignore it
    let mut limits = vec![MAX_STR_FIELD; n];
    loop {
        let mut tr = tr_fixed;
        let fields: Vec<(&str, String)> = valid[..n]
            .iter()
            .zip(&limits)
            .map(|((k, v), l)| (*k, render_value(v, *l, &mut tr)))
            .collect();
        let (msg, d) = cut_escape(r.msg, msg_limit);
        tr += d;
        let line = render(&head, &msg, &fields, tr);
        if line.len() <= MAX_RECORD {
            return line;
        }
        // Over the bound: shorten the longest string field, else the message, else drop the last field.
        let excess = line.len() - MAX_RECORD + 8;
        let longest = (0..n)
            .filter(|&i| {
                matches!(valid[i].1, Value::Str(_)) && fields[i].1.len() > 2 + ELLIPSIS.len()
            })
            .max_by_key(|&i| fields[i].1.len());
        if let Some(i) = longest {
            let cur = fields[i].1.len() - 2;
            limits[i] = cur.saturating_sub(excess);
        } else if !msg.is_empty() {
            msg_limit = msg.len().saturating_sub(excess);
        } else if n > 0 {
            n -= 1;
            tr_fixed += field_size(valid[n].0, valid[n].1);
            limits.truncate(n);
        } else {
            // Cannot happen: the head alone is a few hundred bytes. Never return an oversize line.
            debug_assert!(false, "head over the record bound");
            return line.chars().take(0).collect();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::redacted;

    fn rec<'a>(msg: &'a str, fields: &'a [(&'a str, Value)]) -> Record<'a> {
        Record {
            wall_ms: 1_700_000_000_123,
            mono_ms: 4567,
            boot: "0123456789abcdef0123456789abcdef",
            seq: 9,
            level: Level::Info,
            source: "shell",
            pid: 42,
            msg,
            fields,
        }
    }

    #[test]
    fn plain_record_has_the_documented_shape() {
        let f = [
            ("n", Value::from(3u32)),
            ("neg", Value::from(-4i64)),
            ("ok", Value::from(true)),
            ("who", Value::from("ann")),
        ];
        let line = encode(&rec("hello", &f));
        assert_eq!(
            line,
            "{\"t\":1700000000123,\"m\":4567,\"boot\":\"0123456789abcdef0123456789abcdef\",\"seq\":9,\"lvl\":\"info\",\"src\":\"shell\",\"pid\":42,\"msg\":\"hello\",\"f\":{\"n\":3,\"neg\":-4,\"ok\":true,\"who\":\"ann\"}}"
        );
    }

    #[test]
    fn escapes_per_json() {
        let line = encode(&rec(
            "a\"b\\c\nd\re\tf\u{1}g\u{7f}é日本",
            &[("k", Value::from("x\"y\n"))],
        ));
        assert!(line.contains(r#""msg":"a\"b\\c\nd\re\tf\u0001g"#), "{line}");
        assert!(line.contains("\u{7f}é日本"));
        assert!(line.contains(r#""k":"x\"y\n""#));
        assert!(!line.contains('\n'));
    }

    #[test]
    fn short_record_is_not_marked() {
        assert!(!encode(&rec("hi", &[])).contains("\"tr\""));
    }

    #[test]
    fn long_message_is_cut_to_512_at_a_char_boundary_with_tr() {
        let msg = "é".repeat(3000);
        let line = encode(&rec(&msg, &[]));
        assert!(line.len() <= MAX_RECORD, "{}", line.len());
        let start = line.find("\"msg\":\"").unwrap() + 7;
        let end = line.find("\",\"f\"").unwrap();
        let body = &line[start..end];
        assert!(body.len() <= MAX_MSG, "{}", body.len());
        assert!(body.ends_with('\u{2026}'));
        let kept = body.trim_end_matches('\u{2026}');
        assert!(kept.chars().all(|c| c == 'é'));
        // tr counts the bytes of the message that are not in the line
        let dropped = msg.len() - kept.len();
        assert!(line.ends_with(&format!(",\"tr\":{dropped}}}")), "{line}");
        // 512 bytes: 509 for text (254 whole é) + the ellipsis
        assert_eq!(kept.len(), 508);
    }

    #[test]
    fn msg_of_exactly_512_is_whole_and_513_is_cut() {
        let line = encode(&rec(&"x".repeat(512), &[]));
        assert!(!line.contains("\"tr\""));
        assert!(!line.contains('\u{2026}'));
        let line = encode(&rec(&"x".repeat(513), &[]));
        assert!(line.contains('\u{2026}'));
        assert!(line.ends_with(",\"tr\":4}"), "{line}");
    }

    #[test]
    fn cut_never_splits_an_escape() {
        for pad in 0..12 {
            let msg = format!("{}{}", "a".repeat(500 + pad), "\n\t\"\\\u{2}".repeat(100));
            let line = encode(&rec(&msg, &[]));
            assert!(line.len() <= MAX_RECORD);
            let start = line.find("\"msg\":\"").unwrap() + 7;
            let end = line.find("\",\"f\"").unwrap();
            let body = &line[start..end];
            assert!(body.len() <= MAX_MSG);
            assert!(body.ends_with('\u{2026}'));
            let body = body.trim_end_matches('\u{2026}');
            // an odd run of backslashes at the end would mean a split escape
            let tail = body.bytes().rev().take_while(|b| *b == b'\\').count();
            assert_eq!(tail % 2, 0, "{body}");
            assert!(
                !body.ends_with("\\u000") && !body.ends_with("\\u00"),
                "{body}"
            );
        }
    }

    #[test]
    fn a_long_string_field_is_cut_to_256_with_tr() {
        let v = "z".repeat(300);
        let line = encode(&rec("m", &[("k", Value::from(v.as_str()))]));
        assert!(
            line.contains(&format!("\"k\":\"{}\u{2026}\"", "z".repeat(253))),
            "{line}"
        );
        assert!(line.ends_with(",\"tr\":47}"), "{line}");
    }

    #[test]
    fn fields_past_16_are_dropped_and_counted_in_tr() {
        let names: Vec<String> = (0..18).map(|i| format!("k{i:02}")).collect();
        let f: Vec<(&str, Value)> = names
            .iter()
            .map(|n| (n.as_str(), Value::from(1u8)))
            .collect();
        let line = encode(&rec("m", &f));
        assert!(!line.contains("k16") && !line.contains("k17"));
        // two fields of key 3 bytes + value "1"
        assert!(line.ends_with(",\"tr\":8}"), "{line}");
    }

    #[test]
    fn big_fields_are_cut_longest_first_then_dropped_as_needed() {
        let big = "z".repeat(5000);
        let names: Vec<String> = (0..16).map(|i| format!("k{i}")).collect();
        let f: Vec<(&str, Value)> = names
            .iter()
            .map(|n| (n.as_str(), Value::from(big.as_str())))
            .collect();
        let line = encode(&rec("msg", &f));
        assert!(line.len() <= MAX_RECORD, "{}", line.len());
        assert!(line.contains("\"tr\":"));
        assert!(
            line.contains("\"msg\":\"msg\""),
            "message untouched while fields can give: {line}"
        );
        assert!(line.contains("\"k0\":"));
    }

    #[test]
    fn field_count_is_capped_at_16() {
        let names: Vec<String> = (0..20).map(|i| format!("k{i}")).collect();
        let f: Vec<(&str, Value)> = names
            .iter()
            .map(|n| (n.as_str(), Value::from(1u8)))
            .collect();
        let line = encode(&rec("m", &f));
        assert!(line.contains("\"k15\":1"));
        assert!(!line.contains("\"k16\""));
    }

    #[test]
    fn invalid_keys_are_skipped_and_do_not_use_a_slot() {
        let long = "k".repeat(33);
        let ok32 = "k".repeat(32);
        let f = [
            ("", Value::from(1u8)),
            ("Upper", Value::from(1u8)),
            ("a-b", Value::from(1u8)),
            ("a b", Value::from(1u8)),
            ("a\"b", Value::from(1u8)),
            ("é", Value::from(1u8)),
            (long.as_str(), Value::from(1u8)),
            (ok32.as_str(), Value::from(2u8)),
            ("good_1", Value::from(3u8)),
        ];
        let line = encode(&rec("m", &f));
        assert!(
            line.ends_with(&format!("\"f\":{{\"{ok32}\":2,\"good_1\":3}}}}")),
            "{line}"
        );
        assert!(valid_key("a0_"));
        assert!(valid_key("net.rx_bytes"));
        assert!(!valid_key("A"));
        assert!(!valid_key(""));
    }

    #[test]
    fn dotted_keys_are_kept() {
        let line = encode(&rec("m", &[("a.b", Value::from(1u8))]));
        assert!(line.contains("\"f\":{\"a.b\":1}"), "{line}");
    }

    /// Worst cases for the 2048-byte bound: every field slot full, 32-byte keys, 256-byte values of escapes and multi-byte
    /// characters, a huge message, the widest numbers, a source and boot id at their limits.
    #[test]
    fn worst_case_lines_fit_the_bound() {
        let key_names: Vec<String> = (0..20).map(|i| format!("{:0>31}{}", i, "k")).collect();
        let pieces: [&str; 6] = ["\n", "\"", "\u{1}", "é", "日", "😀"];
        let mut seed = 0x9e3779b97f4a7c15u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for round in 0..400 {
            let mut texts: Vec<String> = Vec::new();
            for _ in 0..18 {
                let mut t = String::new();
                for _ in 0..(next() % 400) {
                    t.push_str(pieces[(next() % 6) as usize]);
                }
                texts.push(t);
            }
            let fields: Vec<(&str, Value)> = key_names
                .iter()
                .zip(&texts)
                .enumerate()
                .map(|(i, (k, t))| {
                    let v = match i % 4 {
                        0 => Value::from(t.as_str()),
                        1 => Value::Int(i64::MIN),
                        2 => Value::UInt(u64::MAX),
                        _ => Value::from(t.as_str()),
                    };
                    (k.as_str(), v)
                })
                .collect();
            let msg: String = (0..(next() % 3000))
                .map(|_| pieces[(next() % 6) as usize])
                .collect();
            let mut r = rec(&msg, &fields);
            r.wall_ms = u64::MAX;
            r.mono_ms = u64::MAX;
            r.seq = u64::MAX;
            r.pid = u32::MAX;
            let src = "\u{1}é".repeat(40);
            if round % 2 == 0 {
                r.source = &src;
                r.boot = &src;
            }
            let line = encode(&r);
            assert!(line.len() <= MAX_RECORD, "round {round}: {}", line.len());
            assert!(line.len() < MAX_LINE, "newline included");
            assert!(!line.contains('\n'));
            assert!(line.starts_with("{\"t\":") && line.ends_with('}'));
            if let Some(at) = line.rfind(",\"tr\":") {
                let n = &line[at + 6..line.len() - 1];
                assert!(
                    !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()),
                    "{line}"
                );
            }
        }
    }

    #[test]
    fn secrets_print_as_redacted() {
        let secret = String::from("hunter2-very-secret");
        let f = [("pw", Value::from(redacted(&secret)))];
        let line = encode(&rec("login", &f));
        assert!(line.contains("\"pw\":\"<redacted>\""));
        assert!(!line.contains("hunter2"));
    }

    #[test]
    fn hostile_source_and_boot_cannot_break_the_line() {
        let mut r = rec("m", &[]);
        r.source = "a\"b\n";
        let long_boot = "f".repeat(100);
        r.boot = &long_boot;
        let line = encode(&r);
        assert!(!line.contains('\n'));
        assert!(line.contains("\"src\":\"a\\\"b\\n\""));
        assert!(line.contains(&format!("\"boot\":\"{}\"", "f".repeat(32))));
    }
}
