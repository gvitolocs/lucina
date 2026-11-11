//! What `e2fsck -f -y` said about /data after an unclean end, as numbers and a list of the files it touched. Pure text in, text out:
//! `lucina-data` feeds it `/run/lucina/e2fsck.log`, writes one line to the event log and copies what was moved or cut short
//! ([`crate::repair`]).
//!
//! e2fsck states a problem and asks a question, either in one line (`Inode 14, i_size is 100, should be 12288.  Fix? yes`) or in two
//! (`Unattached inode 13` then `Connect to /lost+found? yes`). With `-y` every answer is yes. A few statements need no answer
//! (`Illegal block #3 (99999999) in inode 13.  CLEARED.`).

use std::fmt::Write as _;

/// What e2fsck did to one inode, as far as the files can be told apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Blocks cleared or the size corrected: the file is shorter or has holes where there were bad block numbers.
    Truncated,
    /// The inode or a directory entry was cleared: the name is gone.
    Cleared,
    /// Connected to /lost+found as `#<inode>`.
    LostFound,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Truncated => "truncated",
            Kind::Cleared => "cleared",
            Kind::LostFound => "lost+found",
        }
    }
}

/// One file e2fsck reports having cut, cleared or moved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub kind: Kind,
    pub ino: u64,
    /// The path where e2fsck said it (`Entry 'a' in /d (12)`), without the /data prefix.
    pub path: Option<String>,
    /// e2fsck's own words, one or more lines joined with " | ".
    pub said: String,
}

/// How many repairs of each sort e2fsck answered yes to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub inodes: u32,
    pub blocks: u32,
    pub entries: u32,
    pub lost_found: u32,
    /// Bitmaps and the free/used counts of the groups and the whole file system.
    pub bitmaps: u32,
    pub other: u32,
}

impl Counts {
    pub fn total(&self) -> u32 {
        self.inodes + self.blocks + self.entries + self.lost_found + self.bitmaps + self.other
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub exit: i32,
    pub modified: bool,
    /// `17/64 files`, `30/512 blocks` from the last line.
    pub files: Option<(u64, u64)>,
    pub blocks: Option<(u64, u64)>,
    pub counts: Counts,
    pub findings: Vec<Finding>,
}

/// Number after `word` (`inode 13` -> 13), searching case-insensitively.
fn number_after(text: &str, word: &str) -> Option<u64> {
    let lower = text.to_ascii_lowercase();
    let at = lower.find(word)? + word.len();
    let digits: String = lower[at..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// `(/some/dir)` or `(/some/dir)` after the word "inode N": e2fsck prints the path in parentheses for some statements.
fn parenthesised_path(text: &str) -> Option<String> {
    let open = text.find("(/")?;
    let close = text[open..].find(')')? + open;
    Some(text[open + 1..close].to_string())
}

/// `Entry 'name' in /dir (2) has deleted/unused inode 12.` -> (`/dir/name`, 12).
fn entry_cleared(problem: &str) -> Option<(String, u64)> {
    let rest = problem.strip_prefix("Entry '")?;
    let end = rest.find("' in ")?;
    let name = &rest[..end];
    let after = &rest[end + 5..];
    let dir = after.split(" (").next()?.trim();
    let ino = number_after(after, "inode ")?;
    let path = if dir == "/" {
        format!("/{name}")
    } else {
        format!("{dir}/{name}")
    };
    Some((path, ino))
}

/// What an answered problem is, for the counts.
fn classify(problem: &str, question: &str) -> Class {
    let p = problem;
    if question.starts_with("Connect to /lost+found") {
        Class::LostFound
    } else if p.starts_with("Entry '") {
        Class::Entry
    } else if p.contains("bitmap differences")
        || p.contains(" count wrong")
        || p.starts_with("Directories count wrong")
    {
        Class::Bitmap
    } else if p.starts_with("Inode ")
        || p.starts_with("Deleted inode")
        || p.starts_with("Unattached")
    {
        Class::Inode
    } else if p.contains("block") || p.contains("Block") {
        Class::Block
    } else {
        Class::Other
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Inode,
    Block,
    Entry,
    LostFound,
    Bitmap,
    Other,
}

fn note(findings: &mut Vec<Finding>, kind: Kind, ino: u64, path: Option<String>, said: &str) {
    if let Some(f) = findings.iter_mut().find(|f| f.kind == kind && f.ino == ino) {
        if f.path.is_none() {
            f.path = path;
        }
        if !f.said.contains(said) {
            f.said.push_str(" | ");
            f.said.push_str(said);
        }
    } else {
        findings.push(Finding {
            kind,
            ino,
            path,
            said: said.to_string(),
        });
    }
}

/// `17/64 files (0.0% non-contiguous), 30/512 blocks`.
fn parse_totals(line: &str) -> Option<((u64, u64), (u64, u64))> {
    let pair = |s: &str| -> Option<(u64, u64)> {
        let (a, b) = s.split_once('/')?;
        Some((a.parse().ok()?, b.parse().ok()?))
    };
    let after_colon = line.split_once(": ")?.1;
    let mut words = after_colon.split_whitespace();
    let files = pair(words.next()?)?;
    if words.next()? != "files" {
        return None;
    }
    let blocks_at = after_colon.rfind(", ")? + 2;
    let mut tail = after_colon[blocks_at..].split_whitespace();
    let blocks = pair(tail.next()?)?;
    (tail.next()? == "blocks").then_some((files, blocks))
}

/// Reads e2fsck's output (stdout and stderr together) and its exit code.
pub fn parse(output: &str, exit: i32) -> Report {
    let mut report = Report {
        exit,
        ..Report::default()
    };
    let mut pending: Option<String> = None;
    for raw in output.lines() {
        let line = raw.trim_end();
        if line.is_empty() {
            continue;
        }
        if line.starts_with("Pass ") {
            pending = None;
            continue;
        }
        if line.contains("FILE SYSTEM WAS MODIFIED") {
            report.modified = true;
            continue;
        }
        if let Some((files, blocks)) = parse_totals(line) {
            report.files = Some(files);
            report.blocks = Some(blocks);
            continue;
        }
        if let Some(statement) = line.strip_suffix("CLEARED.") {
            // `Illegal block #3 (99999999) in inode 13.  CLEARED.`
            if let Some(ino) = number_after(statement, "in inode ") {
                note(
                    &mut report.findings,
                    Kind::Truncated,
                    ino,
                    None,
                    line.trim(),
                );
            }
            continue;
        }
        // Problem and question in one line, or a question alone after its problem.
        let (problem, question) = match line.rfind(".  ") {
            Some(at) if line.ends_with("? yes") => {
                (line[..at + 1].to_string(), line[at + 3..].to_string())
            }
            _ if line.ends_with("? yes") => (pending.take().unwrap_or_default(), line.to_string()),
            _ => {
                pending = Some(line.to_string());
                continue;
            }
        };
        pending = None;
        let said = format!("{problem}  {question}");
        match classify(&problem, &question) {
            Class::Inode => report.counts.inodes += 1,
            Class::Block => report.counts.blocks += 1,
            Class::Entry => report.counts.entries += 1,
            Class::LostFound => report.counts.lost_found += 1,
            Class::Bitmap => report.counts.bitmaps += 1,
            Class::Other => report.counts.other += 1,
        }
        if let Some((path, ino)) = entry_cleared(&problem) {
            note(&mut report.findings, Kind::Cleared, ino, Some(path), &said);
        } else if question.starts_with("Connect to /lost+found")
            && let Some(ino) = number_after(&problem, "inode ")
        {
            note(
                &mut report.findings,
                Kind::LostFound,
                ino,
                parenthesised_path(&problem),
                &said,
            );
        } else if problem.starts_with("Inode ")
            && !problem.contains("bitmap")
            && let Some(ino) = number_after(&problem, "inode ")
        {
            if problem.contains("illegal block") || problem.contains("i_size is") {
                note(
                    &mut report.findings,
                    Kind::Truncated,
                    ino,
                    parenthesised_path(&problem),
                    &said,
                );
            } else if question.starts_with("Clear?") {
                note(
                    &mut report.findings,
                    Kind::Cleared,
                    ino,
                    parenthesised_path(&problem),
                    &said,
                );
            }
        }
    }
    report
}

impl Report {
    /// Whether the run left something worth keeping a copy of.
    pub fn touched_files(&self) -> bool {
        !self.findings.is_empty()
    }

    /// One line for the event log, the kernel log and `/run/lucina/e2fsck.summary`.
    pub fn summary(&self, ms: u128) -> String {
        let c = &self.counts;
        let mut s = format!("e2fsck exit {} in {ms} ms", self.exit);
        if c.total() == 0 {
            s.push_str(": nothing to fix");
        } else {
            let _ = write!(
                s,
                ": fixed {} (inodes {}, blocks {}, directory entries {}, moved to lost+found {}, bitmaps and counts {}, other {})",
                c.total(),
                c.inodes,
                c.blocks,
                c.entries,
                c.lost_found,
                c.bitmaps,
                c.other
            );
        }
        if let Some((used, total)) = self.files {
            let _ = write!(s, "; {used}/{total} files");
        }
        if let Some((used, total)) = self.blocks {
            let _ = write!(s, ", {used}/{total} blocks");
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real run (e2fsck 1.47.2) on an image whose name for inode 13 was unlinked, a size field cut, and a group count wrong.
    const UNLINKED: &str = "e2fsck 1.47.2 (1-Jan-2025)
Pass 1: Checking inodes, blocks, and sizes
Inode 14, i_size is 100, should be 12288.  Fix? yes

Pass 2: Checking directory structure
Pass 3: Checking directory connectivity
Pass 4: Checking reference counts
Unattached inode 13
Connect to /lost+found? yes

Inode 13 ref count is 2, should be 1.  Fix? yes

Pass 5: Checking group summary information
Free blocks count wrong for group #0 (5, counted=482).
Fix? yes


a.img: ***** FILE SYSTEM WAS MODIFIED *****
a.img: 17/64 files (0.0% non-contiguous), 30/512 blocks
";

    /// A real run on an image with bad block numbers in two files and a cleared directory inode.
    const DAMAGED: &str = "e2fsck 1.47.2 (1-Jan-2025)
Pass 1: Checking inodes, blocks, and sizes
Inode 13 has illegal block(s).  Clear? yes

Illegal block #3 (99999999) in inode 13.  CLEARED.
Inode 13, i_blocks is 40, should be 32.  Fix? yes

Inode 15 has illegal block(s).  Clear? yes

Illegal block #0 (99999999) in inode 15.  CLEARED.
Inode 15, i_blocks is 8, should be 0.  Fix? yes

Pass 2: Checking directory structure
Entry 'd' in / (2) has deleted/unused inode 12.  Clear? yes

Pass 3: Checking directory connectivity
Pass 4: Checking reference counts
Inode 2 ref count is 4, should be 3.  Fix? yes

Unattached inode 16
Connect to /lost+found? yes

Inode 16 ref count is 2, should be 1.  Fix? yes

Pass 5: Checking group summary information
Block bitmap differences:  -12 -16 -21
Fix? yes

Free blocks count wrong for group #0 (482, counted=485).
Fix? yes

Free blocks count wrong (482, counted=485).
Fix? yes

Inode bitmap differences:  -12
Fix? yes

Free inodes count wrong for group #0 (47, counted=48).
Fix? yes

Directories count wrong for group #0 (3, counted=2).
Fix? yes

Free inodes count wrong (47, counted=48).
Fix? yes


b.img: ***** FILE SYSTEM WAS MODIFIED *****
b.img: 16/64 files (6.3% non-contiguous), 27/512 blocks
";

    const CLEAN: &str = "e2fsck 1.47.2 (1-Jan-2025)
Pass 1: Checking inodes, blocks, and sizes
Pass 2: Checking directory structure
Pass 3: Checking directory connectivity
Pass 4: Checking reference counts
Pass 5: Checking group summary information
c.img: 17/64 files (0.0% non-contiguous), 30/512 blocks
";

    #[test]
    fn a_run_that_found_nothing_has_no_counts_and_no_findings() {
        let r = parse(CLEAN, 0);
        assert_eq!(r.counts.total(), 0);
        assert!(!r.modified);
        assert!(r.findings.is_empty());
        assert_eq!(r.files, Some((17, 64)));
        assert_eq!(r.blocks, Some((30, 512)));
        assert_eq!(
            r.summary(120),
            "e2fsck exit 0 in 120 ms: nothing to fix; 17/64 files, 30/512 blocks"
        );
    }

    #[test]
    fn an_unattached_inode_goes_to_lost_found_and_a_short_size_is_a_cut() {
        let r = parse(UNLINKED, 1);
        assert!(r.modified);
        assert_eq!(
            r.counts,
            Counts {
                inodes: 2,
                blocks: 0,
                entries: 0,
                lost_found: 1,
                bitmaps: 1,
                other: 0
            }
        );
        let kinds: Vec<(Kind, u64)> = r.findings.iter().map(|f| (f.kind, f.ino)).collect();
        assert_eq!(kinds, [(Kind::Truncated, 14), (Kind::LostFound, 13)]);
        assert!(r.findings[1].said.contains("Unattached inode 13"));
    }

    #[test]
    fn bad_blocks_and_a_cleared_entry_are_named_by_inode_and_path() {
        let r = parse(DAMAGED, 1);
        assert_eq!(r.counts.entries, 1);
        assert_eq!(r.counts.lost_found, 1);
        // Inode and block fixes: i_blocks twice and "has illegal block(s)" twice, ref counts twice.
        assert_eq!(r.counts.inodes, 6);
        assert_eq!(r.counts.bitmaps, 7);
        assert_eq!(r.counts.total(), 15);
        let by = |kind, ino| r.findings.iter().find(|f| f.kind == kind && f.ino == ino);
        let cut = by(Kind::Truncated, 13).expect("inode 13 cut");
        assert!(
            cut.said
                .contains("Illegal block #3 (99999999) in inode 13.  CLEARED."),
            "{}",
            cut.said
        );
        assert!(by(Kind::Truncated, 15).is_some());
        let entry = by(Kind::Cleared, 12).expect("entry cleared");
        assert_eq!(entry.path.as_deref(), Some("/d"));
        assert!(by(Kind::LostFound, 16).is_some());
        assert_eq!(r.findings.len(), 4);
    }

    #[test]
    fn an_entry_below_a_directory_gets_the_whole_path() {
        let r = parse(
            "Pass 2: Checking directory structure\nEntry 'g' in /d/e (30) has deleted/unused inode 17.  Clear? yes\n\n",
            1,
        );
        assert_eq!(r.findings.len(), 1);
        assert_eq!(r.findings[0].path.as_deref(), Some("/d/e/g"));
        assert_eq!(r.findings[0].ino, 17);
    }

    #[test]
    fn a_path_in_parentheses_is_kept() {
        let r = parse(
            "Pass 3: Checking directory connectivity\nUnconnected directory inode 21 (/d/???)\nConnect to /lost+found? yes\n\n",
            1,
        );
        assert_eq!(r.findings[0].kind, Kind::LostFound);
        assert_eq!(r.findings[0].ino, 21);
        assert_eq!(r.findings[0].path.as_deref(), Some("/d/???"));
    }

    #[test]
    fn the_summary_names_each_sort() {
        let r = parse(UNLINKED, 1);
        assert_eq!(
            r.summary(86),
            "e2fsck exit 1 in 86 ms: fixed 4 (inodes 2, blocks 0, directory entries 0, moved to lost+found 1, bitmaps and counts 1, other 0); 17/64 files, 30/512 blocks"
        );
    }

    #[test]
    fn answers_of_no_are_not_counted() {
        let r = parse(
            "Pass 1: Checking inodes, blocks, and sizes\nInode 5, i_size is 1, should be 2.  Fix? no\n",
            4,
        );
        assert_eq!(r.counts.total(), 0);
        assert!(r.findings.is_empty());
    }
}
