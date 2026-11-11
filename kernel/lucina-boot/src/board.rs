//! Knowing the machine: which board LucinaUEFI is running on, from the firmware device tree's root `compatible` and the SMBIOS type 1
//! `Manufacturer` and `Product Name`, matched against the table `\LUCINA\BOARDS` on the volume it booted from
//! (`docs/design/lucina-net-installer.md`, section 2). Allocation-free: everything borrows from the table text.
//!
//! The table is text, one rule per line, `#` starts a comment line:
//!
//! ```text
//! compatible "<string>" <board-id>
//! smbios "<manufacturer>" "<product>" <board-id>
//! ```
//!
//! Board ids are `[a-z0-9-]{1,40}`. Every string of the root `compatible` (most specific first) is tried against the `compatible`
//! rules, then the SMBIOS pair against the `smbios` rules. Matching is exact and case-sensitive, after trimming ASCII spaces at both
//! ends. The first rule that matches wins. A malformed line never matches and is counted ([`malformed`]).
//!
//! Verus proves the byte-level and decision helpers in this very file, not a copy of it (`verification/net-installer/rules.rs`; the
//! attributes are only seen by Verus): [`valid_id_bytes`] (what a board id is), [`smbios_rule_hit`] (an SMBIOS rule never matches
//! without SMBIOS) and [`choose`] (a `compatible` rule beats an `smbios` rule). Not proved, only unit-tested: the line parser, which
//! hands a board id on only through [`valid_board_id`], and the line numbers.
// Verus reads the plain comparisons of `valid_id_bytes`, so clippy's range and `is_empty` forms are left out.
#![allow(unexpected_cfgs, clippy::len_zero, clippy::manual_range_contains)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// The longest board id.
pub const MAX_ID: usize = 40; // `valid_id_bytes` says 40 literally: Verus reads no constant outside `verus!`.

#[cfg(verus_keep_ghost)]
verus! {

/// A byte a board id may hold: `a-z`, `0-9` or `-`.
pub open spec fn id_byte(b: u8) -> bool {
    (97 <= b && b <= 122) || (48 <= b && b <= 57) || b == 45
}

/// A board id: 1 to 40 bytes, every one an `id_byte`.
pub open spec fn id_ok(s: Seq<u8>) -> bool {
    1 <= s.len() && s.len() <= 40 && forall|i: int| 0 <= i < s.len() ==> id_byte(#[trigger] s[i])
}

} // verus!

/// Whether `s` is a board id, as bytes.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == id_ok(s@),
        r ==> (forall|i: int| 0 <= i < s@.len() ==> id_byte(#[trigger] s@[i])) && 1 <= s@.len() && s@.len() <= 40,
))]
pub fn valid_id_bytes(s: &[u8]) -> bool {
    if s.len() == 0 || s.len() > 40 {
        return false;
    }
    let mut i = 0;
    #[cfg_attr(verus_keep_ghost, verus_spec(
        invariant
            0 <= i <= s.len(),
            1 <= s.len() && s.len() <= 40,
            forall|j: int| 0 <= j < i ==> id_byte(#[trigger] s@[j]),
        decreases s.len() - i
    ))]
    while i < s.len() {
        let b = s[i];
        if !((b >= b'a' && b <= b'z') || (b >= b'0' && b <= b'9') || b == b'-') {
            return false;
        }
        i += 1;
    }
    true
}

/// Whether `s` is a board id: `[a-z0-9-]{1,40}`.
pub fn valid_board_id(s: &str) -> bool {
    valid_id_bytes(s.as_bytes())
}

/// An SMBIOS rule hits only when the firmware gave a manufacturer and a product, and both are equal to the rule's.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r ==> smbios_present && manufacturer_equal && product_equal,
        !smbios_present ==> !r,
))]
pub fn smbios_rule_hit(
    smbios_present: bool,
    manufacturer_equal: bool,
    product_equal: bool,
) -> bool {
    smbios_present && manufacturer_equal && product_equal
}

/// The first hit of each kind, as a table line number: which one counts. A `compatible` rule beats an `smbios` rule when both hit,
/// whatever their lines. Returns whether it is a `compatible` rule, and its line.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        compatible_line is Some ==> r == Some((true, compatible_line.unwrap())),
        (compatible_line is None && smbios_line is Some) ==> r == Some((false, smbios_line.unwrap())),
        (compatible_line is None && smbios_line is None) ==> r is None,
))]
// A plain `match`, not `Option::map`: Verus reads this file as it is.
#[allow(clippy::manual_map)]
pub fn choose(compatible_line: Option<usize>, smbios_line: Option<usize>) -> Option<(bool, usize)> {
    match compatible_line {
        Some(n) => Some((true, n)),
        None => match smbios_line {
            Some(n) => Some((false, n)),
            None => None,
        },
    }
}

/// One rule of the table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule<'a> {
    Compatible {
        compatible: &'a str,
        board: &'a str,
    },
    Smbios {
        manufacturer: &'a str,
        product: &'a str,
        board: &'a str,
    },
}

impl<'a> Rule<'a> {
    pub fn board(&self) -> &'a str {
        match self {
            Rule::Compatible { board, .. } | Rule::Smbios { board, .. } => board,
        }
    }
}

/// A line that is neither blank, a comment nor a rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Malformed;

fn trim_ws(s: &str) -> &str {
    s.trim_matches(|c| c == ' ' || c == '\t' || c == '\r')
}

/// The matching trim: ASCII spaces at both ends.
fn trim_sp(s: &str) -> &str {
    s.trim_matches(' ')
}

/// `"text"` at the start of `s` (after blanks): the text, and what follows the closing quote.
fn quoted(s: &str) -> Option<(&str, &str)> {
    let s = s.trim_start_matches([' ', '\t']);
    let s = s.strip_prefix('"')?;
    let end = s.find('"')?;
    Some((&s[..end], &s[end + 1..]))
}

/// The board id that ends a rule: one valid id and nothing else.
fn board_tail(s: &str) -> Option<&str> {
    let id = trim_ws(s);
    if valid_board_id(id) { Some(id) } else { None }
}

/// One line of the table. `Ok(None)` for a blank line or a comment, `Err` for a line that is neither a rule nor either of those.
pub fn parse_rule(line: &str) -> Result<Option<Rule<'_>>, Malformed> {
    let line = trim_ws(line);
    if line.is_empty() || line.starts_with('#') {
        return Ok(None);
    }
    if let Some(rest) = line.strip_prefix("compatible") {
        if !rest.starts_with([' ', '\t']) {
            return Err(Malformed);
        }
        let (compatible, rest) = quoted(rest).ok_or(Malformed)?;
        let board = board_tail(rest).ok_or(Malformed)?;
        return Ok(Some(Rule::Compatible { compatible, board }));
    }
    if let Some(rest) = line.strip_prefix("smbios") {
        if !rest.starts_with([' ', '\t']) {
            return Err(Malformed);
        }
        let (manufacturer, rest) = quoted(rest).ok_or(Malformed)?;
        let (product, rest) = quoted(rest).ok_or(Malformed)?;
        let board = board_tail(rest).ok_or(Malformed)?;
        return Ok(Some(Rule::Smbios {
            manufacturer,
            product,
            board,
        }));
    }
    Err(Malformed)
}

/// Whether `rule` matches the machine: `compatibles` are the strings of the root `compatible`, `smbios` the manufacturer and product
/// name. Exact and case-sensitive, after trimming ASCII spaces at both ends.
pub fn matches<'a>(
    rule: &Rule<'_>,
    mut compatibles: impl Iterator<Item = &'a str>,
    smbios: Option<(&str, &str)>,
) -> bool {
    match rule {
        Rule::Compatible { compatible, .. } => {
            let want = trim_sp(compatible);
            compatibles.any(|c| trim_sp(c) == want)
        }
        Rule::Smbios {
            manufacturer,
            product,
            ..
        } => {
            let (m, p) = smbios.unwrap_or(("", ""));
            smbios_rule_hit(
                smbios.is_some(),
                trim_sp(m) == trim_sp(manufacturer),
                trim_sp(p) == trim_sp(product),
            )
        }
    }
}

/// The board of the machine: the first `compatible` rule that matches the most specific string of the root `compatible` (a string
/// earlier in the list beats a rule earlier in the table), else the first `smbios` rule that matches. Returns the board id
/// and the rule's line number in the table (1-based). Malformed lines never match.
// Nested `if let`s, not let chains: Verus reads this file as edition 2021.
#[allow(clippy::collapsible_if)]
pub fn pick<'t, 'a>(
    table: &'t str,
    compatibles: impl Iterator<Item = &'a str>,
    smbios: Option<(&str, &str)>,
) -> Option<(&'t str, usize)> {
    let mut compatible_hit: Option<(&str, usize)> = None;
    // Most specific string first: the order of the device tree beats the order of the table.
    for c in compatibles {
        for (i, line) in table.lines().enumerate() {
            if let Ok(Some(rule @ Rule::Compatible { board, .. })) = parse_rule(line) {
                if matches(&rule, core::iter::once(c), None) {
                    compatible_hit = Some((board, i + 1));
                    break;
                }
            }
        }
        if compatible_hit.is_some() {
            break;
        }
    }
    let mut smbios_hit: Option<(&str, usize)> = None;
    for (i, line) in table.lines().enumerate() {
        if let Ok(Some(rule @ Rule::Smbios { board, .. })) = parse_rule(line) {
            if matches(&rule, core::iter::empty(), smbios) {
                smbios_hit = Some((board, i + 1));
                break;
            }
        }
    }
    match choose(compatible_hit.map(|h| h.1), smbios_hit.map(|h| h.1)) {
        Some((true, _)) => compatible_hit,
        Some((false, _)) => smbios_hit,
        None => None,
    }
}

/// The `index`-th (1-based) string of an SMBIOS structure's string set (NUL-separated, without the final terminator).
fn smbios_string(set: &[u8], index: u8) -> Option<&str> {
    let i = usize::from(index).checked_sub(1)?;
    let s = core::str::from_utf8(set.split(|&b| b == 0).nth(i)?).ok()?;
    if s.is_empty() { None } else { Some(s) }
}

/// The `Manufacturer` and `Product Name` of the first type 1 (System Information) structure in an SMBIOS structure table. `None`
/// when there is none, either string is missing or empty, or the table is cut short or malformed.
pub fn smbios_system(table: &[u8]) -> Option<(&str, &str)> {
    let mut pos = 0usize;
    while pos + 4 <= table.len() {
        let (typ, len) = (table[pos], usize::from(table[pos + 1]));
        if len < 4 || pos + len > table.len() {
            return None;
        }
        let strings_at = pos + len;
        // The string set ends at the first double NUL (an empty set is just that).
        let end = (strings_at..table.len().saturating_sub(1))
            .find(|&e| table[e] == 0 && table[e + 1] == 0)?;
        match typ {
            127 => return None,
            1 => {
                if len < 6 {
                    return None;
                }
                let set = &table[strings_at..end];
                return Some((
                    smbios_string(set, table[pos + 4])?,
                    smbios_string(set, table[pos + 5])?,
                ));
            }
            _ => pos = end + 2,
        }
    }
    None
}

/// How many lines of the table are malformed.
pub fn malformed(table: &str) -> usize {
    table.lines().filter(|l| parse_rule(l).is_err()).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &str = "\
# boards
compatible \"qcom,sdm845\" sdm845-board
compatible \"vendor,fancy\"   fancy-board
smbios \"SAMSUNG ELECTRONICS CO., LTD.\" \"Galaxy Book Go\" galaxy-book-go
smbios \"Acme\" \"Rocket 9\" acme-rocket
smbios \"Acme\" \"Rocket 9\" acme-other
";

    fn compat(strs: &'static [&'static str]) -> impl Iterator<Item = &'static str> + Clone {
        strs.iter().copied()
    }

    #[test]
    fn galaxy_book_go_by_smbios() {
        let got = pick(
            TABLE,
            compat(&[]),
            Some(("SAMSUNG ELECTRONICS CO., LTD.", "Galaxy Book Go")),
        );
        assert_eq!(got, Some(("galaxy-book-go", 4)));
    }

    #[test]
    fn device_tree_board_most_specific_first() {
        let got = pick(TABLE, compat(&["vendor,fancy", "qcom,sdm845"]), None);
        assert_eq!(got, Some(("fancy-board", 3)));
        let got = pick(TABLE, compat(&["x,y", "qcom,sdm845"]), None);
        assert_eq!(got, Some(("sdm845-board", 2)));
    }

    #[test]
    fn compatible_beats_smbios_even_when_later() {
        let got = pick(
            TABLE,
            compat(&["qcom,sdm845"]),
            Some(("SAMSUNG ELECTRONICS CO., LTD.", "Galaxy Book Go")),
        );
        assert_eq!(got, Some(("sdm845-board", 2)));
    }

    #[test]
    fn first_smbios_rule_wins() {
        let got = pick(TABLE, compat(&[]), Some(("Acme", "Rocket 9")));
        assert_eq!(got, Some(("acme-rocket", 5)));
    }

    #[test]
    fn unknown_board() {
        assert_eq!(
            pick(TABLE, compat(&["no,such"]), Some(("Acme", "Rocket 10"))),
            None
        );
        assert_eq!(pick(TABLE, compat(&[]), None), None);
        assert_eq!(
            pick("", compat(&["qcom,sdm845"]), Some(("Acme", "Rocket 9"))),
            None
        );
    }

    #[test]
    fn smbios_rule_never_matches_without_smbios() {
        let r = parse_rule("smbios \"\" \"\" empty").unwrap().unwrap();
        assert!(!matches(&r, compat(&[]), None));
        assert!(matches(&r, compat(&[]), Some(("", ""))));
    }

    #[test]
    fn malformed_lines_are_skipped_and_counted() {
        let table = "\
compatible qcom,sdm845 a
compatible \"qcom,sdm845\" Upper
compatible \"qcom,sdm845\"
smbios \"Acme\" rocket
compatible \"x\" a b
bogus line
compatible \"qcom,sdm845\" good
compatible\"q\" x
";
        assert_eq!(malformed(table), 7);
        assert_eq!(
            pick(table, compat(&["qcom,sdm845"]), None),
            Some(("good", 7))
        );
        assert_eq!(
            parse_rule("compatible \"qcom,sdm845\" Upper"),
            Err(Malformed)
        );
    }

    #[test]
    fn blank_and_comment_lines_are_not_rules() {
        assert_eq!(parse_rule(""), Ok(None));
        assert_eq!(parse_rule("   \t"), Ok(None));
        assert_eq!(parse_rule("# compatible \"a\" b"), Ok(None));
        assert_eq!(malformed("\n# x\n\n"), 0);
    }

    #[test]
    fn quoting_with_spaces_in_the_product_name() {
        let r = parse_rule("smbios \"Micro Star\" \"MS-7C35 Pro  Max\" msi-board").unwrap();
        assert_eq!(
            r,
            Some(Rule::Smbios {
                manufacturer: "Micro Star",
                product: "MS-7C35 Pro  Max",
                board: "msi-board"
            })
        );
        assert_eq!(
            pick(
                "smbios \"Micro Star\" \"MS-7C35 Pro  Max\" msi-board",
                compat(&[]),
                Some(("Micro Star", "MS-7C35 Pro  Max"))
            ),
            Some(("msi-board", 1))
        );
    }

    #[test]
    fn spaces_are_trimmed_at_both_ends_only() {
        let table = "smbios \" Acme \" \"Rocket 9\" a\ncompatible \"x,y\" b\n";
        assert_eq!(
            pick(table, compat(&[]), Some(("Acme  ", " Rocket 9"))),
            Some(("a", 1))
        );
        assert_eq!(pick(table, compat(&[]), Some(("Acme", "Rocket  9"))), None);
        assert_eq!(pick(table, compat(&[" x,y "]), None), Some(("b", 2)));
        assert_eq!(pick(table, compat(&["X,Y"]), None), None, "case-sensitive");
    }

    #[test]
    fn crlf_tables_work() {
        let table = "compatible \"a,b\" board-1\r\nsmbios \"M\" \"P\" board-2\r\n";
        assert_eq!(pick(table, compat(&["a,b"]), None), Some(("board-1", 1)));
        assert_eq!(
            pick(table, compat(&[]), Some(("M", "P"))),
            Some(("board-2", 2))
        );
    }

    fn structure(typ: u8, formatted: &[u8], strings: &[&str]) -> Vec<u8> {
        let mut v = vec![typ, (4 + formatted.len()) as u8, 0, 0];
        v.extend_from_slice(formatted);
        for s in strings {
            v.extend_from_slice(s.as_bytes());
            v.push(0);
        }
        if strings.is_empty() {
            v.push(0);
        }
        v.push(0);
        v
    }

    #[test]
    fn smbios_type_1_is_found_after_other_structures() {
        let mut t = structure(0, &[1, 2, 3, 4], &["BIOS vendor", "1.0"]);
        t.extend(structure(2, &[], &[]));
        t.extend(structure(
            1,
            &[1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            &["SAMSUNG ELECTRONICS CO., LTD.", "Galaxy Book Go", "serial"],
        ));
        t.extend(structure(127, &[], &[]));
        assert_eq!(
            smbios_system(&t),
            Some(("SAMSUNG ELECTRONICS CO., LTD.", "Galaxy Book Go"))
        );
    }

    #[test]
    fn smbios_without_type_1_or_with_missing_strings_is_none() {
        let mut t = structure(0, &[0; 4], &["x"]);
        t.extend(structure(127, &[], &[]));
        assert_eq!(smbios_system(&t), None);
        assert_eq!(smbios_system(&[]), None);
        let no_product = structure(1, &[1, 0], &["Acme"]);
        assert_eq!(smbios_system(&no_product), None);
        let cut = &structure(1, &[1, 2], &["Acme", "Rocket"])[..9];
        assert_eq!(smbios_system(cut), None);
        assert_eq!(
            smbios_system(&[1, 0, 0, 0]),
            None,
            "length below the header"
        );
    }

    #[test]
    fn board_ids() {
        assert!(valid_board_id("galaxy-book-go"));
        assert!(valid_board_id("a"));
        assert!(valid_board_id(&"a".repeat(40)));
        assert!(!valid_board_id(""));
        assert!(!valid_board_id(&"a".repeat(41)));
        assert!(!valid_board_id("Galaxy"));
        assert!(!valid_board_id("a_b"));
        assert!(!valid_board_id("a b"));
        assert!(!valid_board_id("é"));
    }
}
