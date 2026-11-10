//! Juno's apps as files on the EFI system partition, out of the boot image: read when one is opened.
//!
//! ```text
//! \lucinaos\apps\CURRENT            one line: the generation in use
//! \lucinaos\apps\PREVIOUS           one line: the one before it, whole, the way back
//! \lucinaos\apps\g-<12 hex>\<id>.app   the manifest
//! \lucinaos\apps\g-<12 hex>\<id>.bin   the program
//! \lucinaos\apps\g-<12 hex>\<id>.png   its icon, if it has one
//! \lucinaos\apps\g-<12 hex>\<id>.pkg   large data of its own, if it needs some: LUCINA_APP_DIR says where
//! ```
//!
//! `scripts/juno-apps-set.sh` makes the stream `lucina-install-disk apps` reads: a header of text, then the files one
//! after the other. The generation's name comes from the header, which holds every file's sum, so the same apps have the
//! same name. A new set goes to its own folder, is read back, and only then does CURRENT change, by one rename: a power
//! cut leaves the old set or the new set, and never touches what boots.

use std::path::Path;

/// What the first line of a set says.
pub const MAGIC: &str = "LUCINA-APPS 1";
/// The folder on the partition.
pub const FOLDER: &str = "lucinaos/apps";
/// Where the shell keeps the partition mounted, read-only.
pub const APPS_MOUNT: &str = "/mnt/lucina-apps";
const MAX_HEADER: usize = 64 * 1024;
const MAX_FILES: usize = 1024;

/// One file of a set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub size: u64,
    /// SHA-256 in hex.
    pub sum: String,
    pub name: String,
}

/// A set's header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub entries: Vec<Entry>,
    /// The folder's name: `g-` and 12 hex digits.
    pub generation: String,
}

/// A name that may be a file of a set: nothing that leaves the folder or that FAT would change.
pub fn valid_name(name: &str) -> bool {
    let Some((stem, ext)) = name.rsplit_once('.') else {
        return false;
    };
    !stem.is_empty()
        && stem.len() <= 64
        && stem
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && matches!(ext, "app" | "bin" | "png" | "jpg" | "webp" | "pkg")
}

/// A folder name that is a generation: only such folders are ever removed.
pub fn is_generation(name: &str) -> bool {
    name.len() == 14
        && name.starts_with("g-")
        && name[2..]
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

/// Reads the header at the start of a stream: its lines up to the first empty one. `sum` is SHA-256 in hex.
pub fn read_header(
    input: &mut impl std::io::BufRead,
    sum: impl Fn(&[u8]) -> String,
) -> Result<Header, String> {
    let mut text = String::new();
    loop {
        let before = text.len();
        let n = input
            .read_line(&mut text)
            .map_err(|e| format!("reading the header: {e}"))?;
        if n == 0 {
            return Err("the stream ends inside its header".into());
        }
        if text.len() > MAX_HEADER {
            return Err("the header is too long".into());
        }
        if text[before..] == *"\n" {
            break;
        }
    }
    let mut lines = text.lines();
    if lines.next() != Some(MAGIC) {
        return Err(format!("this is not a set of apps (no {MAGIC:?})"));
    }
    let mut entries: Vec<Entry> = Vec::new();
    for line in lines.take_while(|l| !l.is_empty()) {
        let mut w = line.split(' ');
        let (Some(size), Some(sum), Some(name), None) = (w.next(), w.next(), w.next(), w.next())
        else {
            return Err(format!("bad line in the header: {line:?}"));
        };
        let size = size
            .parse()
            .map_err(|_| format!("bad size in the header: {line:?}"))?;
        if sum.len() != 64 || !sum.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(format!("bad sum in the header: {line:?}"));
        }
        if !valid_name(name) {
            return Err(format!("refused file name {name:?}"));
        }
        if entries.iter().any(|e| e.name == name) {
            return Err(format!("{name} is there twice"));
        }
        entries.push(Entry {
            size,
            sum: sum.to_string(),
            name: name.to_string(),
        });
    }
    if entries.is_empty() || entries.len() > MAX_FILES {
        return Err(format!("a set of {} files", entries.len()));
    }
    let generation = format!("g-{}", &sum(text.as_bytes())[..12]);
    Ok(Header {
        entries,
        generation,
    })
}

/// The manifests' folder of the generation in use under `root` (a mounted partition), and its name.
pub fn current_folder(root: &Path) -> Result<(std::path::PathBuf, String), String> {
    let apps = root.join(FOLDER);
    let current = std::fs::read_to_string(apps.join("CURRENT"))
        .map_err(|_| "there is no set of apps on the disk".to_string())?;
    let current = current.trim();
    if !is_generation(current) {
        return Err(format!("CURRENT says {current:?}"));
    }
    Ok((apps.join(current), current.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_sum(b: &[u8]) -> String {
        format!("{:012x}", b.len())
    }

    const SUM: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn reads_a_header_and_stops_at_its_end() {
        let s = format!("LUCINA-APPS 1\n3 {SUM} hello.bin\n2 {SUM} hello.app\n\nfile bytes");
        let mut r = std::io::Cursor::new(s.as_bytes());
        let h = read_header(&mut r, fake_sum).unwrap();
        assert_eq!(h.entries.len(), 2);
        assert_eq!(h.entries[0].size, 3);
        assert_eq!(h.entries[1].name, "hello.app");
        assert!(is_generation(&h.generation), "{}", h.generation);
        // The contents are next.
        let mut rest = String::new();
        std::io::Read::read_to_string(&mut r, &mut rest).unwrap();
        assert_eq!(rest, "file bytes");
    }

    #[test]
    fn refuses_what_is_not_a_clean_set() {
        let bad = |s: String| read_header(&mut std::io::Cursor::new(s.into_bytes()), fake_sum);
        assert!(bad("nonsense\n\n".into()).is_err());
        assert!(bad("LUCINA-APPS 1\n\n".into()).is_err());
        assert!(bad(format!("LUCINA-APPS 1\n1 {SUM} a.bin")).is_err());
        for name in [
            "../x.bin", "a/b.bin", "CURRENT", "A.bin", "x.exe", ".bin", "x y.bin",
        ] {
            assert!(
                bad(format!("LUCINA-APPS 1\n1 {SUM} {name}\n\n")).is_err(),
                "{name}"
            );
        }
        assert!(bad(format!("LUCINA-APPS 1\n1 abc a.bin\n\n")).is_err());
        assert!(bad(format!("LUCINA-APPS 1\nx {SUM} a.bin\n\n")).is_err());
        assert!(bad(format!("LUCINA-APPS 1\n1 {SUM} a.bin\n1 {SUM} a.bin\n\n")).is_err());
    }

    #[test]
    fn generations_are_recognised_and_nothing_else() {
        assert!(is_generation("g-0123456789ab"));
        for n in [
            "g-0123456789a",
            "g-0123456789abc",
            "g-0123456789aB",
            "CURRENT",
            "x-0123456789ab",
        ] {
            assert!(!is_generation(n), "{n}");
        }
    }

    #[test]
    fn current_folder_reads_its_file() {
        let d = std::env::temp_dir().join(format!("lucina-appset-{}", std::process::id()));
        std::fs::create_dir_all(d.join(FOLDER)).unwrap();
        assert!(current_folder(&d).is_err());
        std::fs::write(d.join(FOLDER).join("CURRENT"), "../../etc\n").unwrap();
        assert!(current_folder(&d).is_err());
        std::fs::write(d.join(FOLDER).join("CURRENT"), "g-0123456789ab\n").unwrap();
        let (dir, generation) = current_folder(&d).unwrap();
        assert_eq!(generation, "g-0123456789ab");
        assert!(dir.ends_with("lucinaos/apps/g-0123456789ab"));
        std::fs::remove_dir_all(d).ok();
    }
}
