//! The two search foldings of the web app, which differ on purpose: Keep folds for word-start matching over whole notes,
//! Shopping folds names to compare them.

use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

/// `web/src/lib/keepFilter.ts` `fold`: NFKD, U+0300-U+036F removed, lower case.
pub fn fold_note(s: &str) -> String {
    s.nfkd()
        .filter(|c| !('\u{300}'..='\u{36f}').contains(c))
        .flat_map(char::to_lowercase)
        .collect()
}

/// `web/src/lib/shoppingCatalog.ts` `fold`: lower case, special letters spelled out, NFD without marks, and every run of
/// anything but letters and digits one space.
pub fn fold_name(text: &str) -> String {
    let mut lowered = String::with_capacity(text.len());
    for c in text.chars().flat_map(char::to_lowercase) {
        match c {
            'æ' => lowered.push_str("ae"),
            'ø' => lowered.push_str("oe"),
            'å' => lowered.push_str("aa"),
            'œ' => lowered.push_str("oe"),
            'ß' => lowered.push_str("ss"),
            'ð' => lowered.push('d'),
            'þ' => lowered.push_str("th"),
            'ł' => lowered.push('l'),
            c => lowered.push(c),
        }
    }
    let mut out = String::with_capacity(lowered.len());
    let mut gap = false;
    for c in lowered.nfd().filter(|c| !is_combining_mark(*c)) {
        if c.is_alphanumeric() {
            if gap && !out.is_empty() {
                out.push(' ');
            }
            gap = false;
            out.push(c);
        } else {
            gap = true;
        }
    }
    out
}

/// `web/src/lib/keepFilter.ts` `wordMatch`: how `word` occurs in `hay` (both folded): 2 as a whole word, 1 at the start of
/// a word, 0 not at a word start. "rust" finds "Rust" and "rust-lang" but not "trust".
pub fn word_match(hay: &str, word: &str) -> u8 {
    if word.is_empty() {
        return 0;
    }
    let mut best = 0;
    let mut from = 0;
    while let Some(i) = hay[from..].find(word) {
        let at = from + i;
        let start = !hay[..at]
            .chars()
            .next_back()
            .is_some_and(char::is_alphanumeric);
        if start {
            let end = !hay[at + word.len()..]
                .chars()
                .next()
                .is_some_and(char::is_alphanumeric);
            if end {
                return 2;
            }
            best = 1;
        }
        from = at + hay[at..].chars().next().map_or(1, char::len_utf8);
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_folding_ignores_accents_and_case() {
        assert_eq!(fold_note("Caffè LATTE"), "caffe latte");
        assert_eq!(fold_note("ﬁne"), "fine"); // NFKD: the ligature opens.
    }

    #[test]
    fn name_folding_matches_the_catalogs_normalise() {
        assert_eq!(fold_name("Æbler & Øl"), "aebler oel");
        assert_eq!(fold_name("  Caffè-d'orzo "), "caffe d orzo");
        assert_eq!(fold_name("Grüße"), "grusse");
        assert_eq!(fold_name("---"), "");
    }

    // web/src/lib/keepFilter.test.ts: only word starts count.
    #[test]
    fn word_match_counts_word_starts_only() {
        assert_eq!(word_match("rust", "rust"), 2);
        assert_eq!(word_match("the rust-lang book", "rust"), 2);
        assert_eq!(word_match("rustacean", "rust"), 1);
        assert_eq!(word_match("trust me, frustrated", "rust"), 0);
        assert_eq!(word_match("trust rustacean rust", "rust"), 2);
        assert_eq!(word_match("anything", ""), 0);
    }
}
