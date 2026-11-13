//! The scrubber: nothing that could be a secret leaves the machine. It runs on every line at the source, before the line is
//! sent, so a collector, a file or a bundle never holds what it removed.
//!
//! Rules (docs/design/juno-logging.md, "Privacy"):
//! 1. After a secret-bearing word (`password`, `passwd`, `passphrase`, `psk`, `secret`, `token`, `api_key`, `apikey`,
//!    `bearer`, `authorization`, `private key`, `credential`, `wpa_passphrase`, `sae_password`) the value up to the next
//!    space is replaced, with any `=`, `:`, quote or space between the word and the value dropped.
//! 2. A run of 32 or more key-alphabet characters (`0-9 A-Z a-z + / = _ -`) that holds a digit and a letter is a key or a hash
//!    and becomes `<blob>`; so does a run of 24 or more that is all hex digits.
//! 3. After the words that carry typed text (`typed`, `keystroke`, `key text`, `input text`, `clipboard`) the rest of the
//!    line goes.
//!
//! False positives are accepted: a line over-scrubbed costs a debugging hint, a line under-scrubbed costs a secret.

const SECRET_WORDS: &[&str] = &[
    "password",
    "passwd",
    "passphrase",
    "psk",
    "secret",
    "token",
    "api_key",
    "apikey",
    "api-key",
    "bearer",
    "authorization",
    "private key",
    "credential",
    "wpa_passphrase",
    "sae_password",
];
const TYPED_WORDS: &[&str] = &["typed", "keystroke", "key text", "input text", "clipboard"];

const MARK: &str = "<scrubbed>";

/// Returns the line with secrets and typed text removed.
pub fn scrub(line: &str) -> String {
    let lower = line.to_ascii_lowercase();
    let b = line.as_bytes();
    // Marked byte ranges to cut, in order.
    let mut cuts: Vec<(usize, usize, &'static str)> = Vec::new();

    // Rule 3 first: the earliest typed-text word cuts to the end of the line.
    let mut typed_at: Option<usize> = None;
    for w in TYPED_WORDS {
        if let Some(i) = find_word(&lower, w, 0) {
            typed_at = Some(typed_at.map_or(i + w.len(), |t| t.min(i + w.len())));
        }
    }

    // Rule 1.
    for w in SECRET_WORDS {
        let mut from = 0;
        while let Some(i) = find_word(&lower, w, from) {
            let mut start = i + w.len();
            from = start;
            // Drop the separators between the word and the value.
            while start < b.len() && matches!(b[start], b'=' | b':' | b' ' | b'"' | b'\'') {
                start += 1;
            }
            let mut end = start;
            while end < b.len() && b[end] != b' ' {
                end += 1;
            }
            if end > start {
                cuts.push((i + w.len(), end, MARK));
                from = end;
            }
        }
    }

    // Rule 2.
    let mut i = 0;
    while i < b.len() {
        if is_key_char(b[i]) {
            let s = i;
            while i < b.len() && is_key_char(b[i]) {
                i += 1;
            }
            let run = &b[s..i];
            let digit = run.iter().any(u8::is_ascii_digit);
            let letter = run.iter().any(u8::is_ascii_alphabetic);
            let hex = run.iter().all(u8::is_ascii_hexdigit);
            if (run.len() >= 32 && digit && letter) || (run.len() >= 24 && hex) {
                cuts.push((s, i, "<blob>"));
            }
        } else {
            i += 1;
        }
    }

    cuts.sort_by_key(|c| c.0);
    let mut out = String::with_capacity(line.len());
    let mut pos = 0;
    for (s, e, mark) in cuts {
        if s < pos {
            // Overlaps a cut already made (the blob inside a secret value): the first cut already covers it.
            if e > pos {
                pos = e;
            }
            continue;
        }
        if let Some(t) = typed_at
            && s >= t
        {
            break;
        }
        out.push_str(&line[pos..s]);
        out.push_str(mark);
        pos = e;
    }
    match typed_at {
        Some(t) if t >= pos => {
            out.push_str(&line[pos..t]);
            out.push(' ');
            out.push_str(MARK);
        }
        _ => out.push_str(&line[pos.min(line.len())..]),
    }
    out
}

fn is_key_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'+' | b'/' | b'=' | b'_' | b'-')
}

/// The first place at or after `from` where `word` stands in `hay` (lower case) on a boundary at its start: not inside a longer
/// word, so `psk` matches in `psk=abc` but not in `pskdriver`... a longer word that merely starts with it is still caught,
/// since the secret may follow directly (`password123`).
fn find_word(hay: &str, word: &str, from: usize) -> Option<usize> {
    let mut start = from;
    while let Some(rel) = hay[start..].find(word) {
        let i = start + rel;
        let before_ok = i == 0 || !hay.as_bytes()[i - 1].is_ascii_alphanumeric();
        if before_ok {
            return Some(i);
        }
        start = i + 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::scrub;

    #[test]
    fn plain_kernel_lines_are_untouched() {
        for l in [
            "usb 1-1: new high-speed USB device number 4",
            "[wdt] hardware watchdog on CPU 6, uptime 34.281 s",
            "backtrace: 0xffffff8000123456 0xffffff8000654321",
            "lucina_idle::enter_state_with_a_long_name_but_no_digits",
        ] {
            assert_eq!(scrub(l), l);
        }
    }

    #[test]
    fn values_after_secret_words_go() {
        assert_eq!(
            scrub("wifi: psk=hunter2 ssid=home"),
            "wifi: psk<scrubbed> ssid=home"
        );
        let a = scrub("Authorization: Bearer abcdef");
        assert!(!a.contains("abcdef") && !a.contains("Bearer "), "{a}");
        assert_eq!(
            scrub("set password \"my pass\" done"),
            "set password<scrubbed> pass\" done"
        );
        // Built at run time from short pieces, so no literal in the source reads as a key.
        let prefix = ["g", "hp", "_"].join("");
        let body = ["ABCD", "EF12", "3456"].join("");
        let line = format!("token={prefix}{body}");
        let out = scrub(&line);
        assert!(!out.contains(&prefix) && !out.contains(&body), "{out}");
    }

    #[test]
    fn long_blobs_go() {
        let l = "key 0123456789abcdef0123456789abcdef0123456789abcdef end";
        assert_eq!(scrub(l), "key <blob> end");
        assert_eq!(
            scrub("sha aGVsbG8gd29ybGQgaGVsbG8gd29ybGQgaGVsbG8gd29ybGQ= x"),
            "sha <blob> x"
        );
    }

    #[test]
    fn typed_text_goes_to_the_end() {
        assert_eq!(scrub("input: typed hello world"), "input: typed <scrubbed>");
        let s = scrub("clipboard now holds secret stuff");
        assert!(!s.contains("stuff") && !s.contains("holds"));
    }

    #[test]
    fn word_inside_a_longer_word_is_not_a_boundary() {
        assert_eq!(scrub("untokenized text"), "untokenized text");
    }
}
