//! A test boot with the stream's own user space (`lucina-reload --stream-root=overlay|initramfs --data=none|ro|rw`): what the
//! reload asked for, the stream's `/lucina-overlay/MANIFEST`, and the marker line. Pure, tested on the host;
//! `lucina-stream-root` does the mounts, `lucina-trial` and `lucina-data` read [`test_boot`] to stay out of the way.
//!
//! Only the `/chosen` properties `lucina,reload-root` and `lucina,reload-data` count: the running kernel's kexec path writes them
//! from the reload's words (kernel patch 1244), into the tree of that one reload, so any later boot is the installed generation
//! again. The same words on a command line are ignored (patch 1245; LucinaUEFI drops them from BOOT.CFG).
//!
//! MANIFEST (fixed interface with `scripts/juno-reload-overlay.sh`): first line
//! `lucina-overlay 1 installed=<40-hex> stream=<40-hex> files=<n> whiteouts=<m> bytes=<total>`, then one line per entry,
//! paths absolute, no `..`: `F <mode octal> <size> <sha256> <path>` (content at `root/<path>`), `L <target> <path>`, `W <path>`,
//! `D <mode> <path>`. `files` counts the `F` lines, `whiteouts` the `W` lines, `bytes` is the `F` sizes added up.

use std::{fs, path::Path};

/// Where the running kernel shows its device tree (patch 0403).
pub const CHOSEN: &str = "/proc/device-tree/chosen";
/// The folder of the stream's initramfs that holds the overlay.
pub const OVERLAY_DIR: &str = "/lucina-overlay";
/// The marker line of this boot, written by `lucina-stream-root` into the test root (a tmpfs or the initramfs, never a disk).
pub const MARKER_FILE: &str = "/etc/lucina/test-root";
/// What the installed root records as the commit it was built from (`installed=` of the MANIFEST must be this).
pub const COMMIT_FILE: &str = "/etc/lucina/commit";

/// What becomes of `/data` on a test boot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Data {
    /// Not mounted (the default).
    None,
    /// Mounted read-only, no check, no sync loop, no seed written.
    ReadOnly,
    /// Checked and mounted as usual, but never marked clean: the next installed boot checks it again.
    ReadWrite,
}

impl Data {
    pub fn name(self) -> &'static str {
        match self {
            Data::None => "none",
            Data::ReadOnly => "ro",
            Data::ReadWrite => "rw",
        }
    }
}

/// A test boot: what the reload asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestBoot {
    /// `overlay` or `initramfs` (any other value is no test boot: the kernel ignores it too).
    pub root: &'static str,
    pub data: Data,
}

/// This boot's test root, from `/proc/device-tree/chosen`.
pub fn test_boot() -> Option<TestBoot> {
    test_boot_in(Path::new(CHOSEN))
}

/// [`test_boot`] for a copy of `/chosen` at `dir`. A data value outside its set reads as `none`: the safe one.
pub fn test_boot_in(dir: &Path) -> Option<TestBoot> {
    let read = |name: &str| -> Option<String> {
        let raw = fs::read(dir.join(name)).ok()?;
        let raw = raw.strip_suffix(&[0]).unwrap_or(&raw);
        Some(String::from_utf8_lossy(raw).into_owned())
    };
    let root = match read("lucina,reload-root")?.as_str() {
        "overlay" => "overlay",
        "initramfs" => "initramfs",
        _ => return None,
    };
    let data = match read("lucina,reload-data").as_deref() {
        Some("ro") => Data::ReadOnly,
        Some("rw") => Data::ReadWrite,
        _ => Data::None,
    };
    Some(TestBoot { root, data })
}

/// One line of the MANIFEST after the first. Paths are absolute, as in the root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    File {
        mode: u32,
        size: u64,
        sha256: [u8; 32],
        path: String,
    },
    Link {
        target: String,
        path: String,
    },
    Whiteout {
        path: String,
    },
    Dir {
        mode: u32,
        path: String,
    },
}

impl Entry {
    pub fn path(&self) -> &str {
        match self {
            Entry::File { path, .. }
            | Entry::Link { path, .. }
            | Entry::Whiteout { path }
            | Entry::Dir { path, .. } => path,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub installed: String,
    pub stream: String,
    pub files: u64,
    pub whiteouts: u64,
    pub bytes: u64,
    pub entries: Vec<Entry>,
}

fn is_commit(s: &str) -> bool {
    s.len() == 40
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// An absolute path with no empty, `.` or `..` component, not `/` itself, no NUL, and nothing the line format cannot hold.
fn check_path(path: &str) -> Result<(), String> {
    let Some(rest) = path.strip_prefix('/') else {
        return Err(format!("{path:?} is not absolute"));
    };
    if rest.is_empty() {
        return Err("an entry for / itself".into());
    }
    if path
        .bytes()
        .any(|b| b == 0 || b == b'\n' || b == b'\t' || b == b'\\')
    {
        return Err(format!("{path:?} has a NUL, newline, tab or backslash"));
    }
    for c in rest.split('/') {
        if c.is_empty() || c == "." || c == ".." {
            return Err(format!("{path:?} has an empty, . or .. component"));
        }
    }
    if path == OVERLAY_DIR || path.starts_with("/lucina-overlay/") {
        return Err(format!("{path:?} is the overlay's own folder"));
    }
    Ok(())
}

fn mode(s: &str) -> Result<u32, String> {
    let m = u32::from_str_radix(s, 8).map_err(|_| format!("mode {s:?} is not octal"))?;
    if m > 0o7777 {
        return Err(format!("mode {s} has more than permission bits"));
    }
    Ok(m)
}

fn sha256(s: &str) -> Result<[u8; 32], String> {
    if s.len() != 64 {
        return Err(format!("sha256 {s:?} is not 64 hex digits"));
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[2 * i..2 * i + 2], 16)
            .map_err(|_| format!("sha256 {s:?} is not hex"))?;
    }
    Ok(out)
}

/// Parses and checks a MANIFEST: the header, every line, no path twice, and that the header's counts are the lines'. Any error
/// refuses the whole overlay.
pub fn parse(text: &str) -> Result<Manifest, String> {
    let mut lines = text.lines();
    let head = lines.next().ok_or("an empty MANIFEST")?;
    let mut w = head.split(' ');
    if (w.next(), w.next()) != (Some("lucina-overlay"), Some("1")) {
        return Err(format!(
            "the first line is not `lucina-overlay 1 ...`: {head:?}"
        ));
    }
    let mut field = |key: &str| -> Result<String, String> {
        w.next()
            .and_then(|f| f.strip_prefix(key))
            .map(str::to_string)
            .ok_or_else(|| format!("the first line has no {key} where it should: {head:?}"))
    };
    let installed = field("installed=")?;
    let stream = field("stream=")?;
    let number = |s: String, what: &str| {
        s.parse::<u64>()
            .map_err(|_| format!("{what}{s:?} is not a number"))
    };
    let files = number(field("files=")?, "files=")?;
    let whiteouts = number(field("whiteouts=")?, "whiteouts=")?;
    let bytes = number(field("bytes=")?, "bytes=")?;
    if w.next().is_some() {
        return Err(format!(
            "the first line has more than its six fields: {head:?}"
        ));
    }
    if !is_commit(&installed) || !is_commit(&stream) {
        return Err("installed= and stream= must be 40 lowercase hex digits".into());
    }
    let mut entries = Vec::new();
    for (n, line) in lines.enumerate() {
        let at = |e: String| format!("MANIFEST line {}: {e}", n + 2);
        if line.is_empty() {
            continue;
        }
        let (kind, rest) = line
            .split_once(' ')
            .ok_or_else(|| at(format!("{line:?} has no fields")))?;
        let entry = match kind {
            "F" => {
                let mut f = rest.splitn(4, ' ');
                let (Some(m), Some(size), Some(sum), Some(path)) =
                    (f.next(), f.next(), f.next(), f.next())
                else {
                    return Err(at(format!("{line:?}: F needs mode, size, sha256 and path")));
                };
                Entry::File {
                    mode: mode(m).map_err(at)?,
                    size: size
                        .parse()
                        .map_err(|_| at(format!("size {size:?} is not a number")))?,
                    sha256: sha256(sum).map_err(at)?,
                    path: path.to_string(),
                }
            }
            "D" => {
                let (m, path) = rest
                    .split_once(' ')
                    .ok_or_else(|| at(format!("{line:?}: D needs mode and path")))?;
                Entry::Dir {
                    mode: mode(m).map_err(at)?,
                    path: path.to_string(),
                }
            }
            "W" => Entry::Whiteout {
                path: rest.to_string(),
            },
            "L" => {
                // `L <target> <path>`: a target or path with a space would be ambiguous, so neither may have one.
                let (target, path) = rest
                    .split_once(' ')
                    .ok_or_else(|| at(format!("{line:?}: L needs target and path")))?;
                if path.contains(' ') || target.is_empty() || target.bytes().any(|b| b == 0) {
                    return Err(at(format!(
                        "{line:?}: a symlink line with a space in it, or an empty target, is ambiguous"
                    )));
                }
                Entry::Link {
                    target: target.to_string(),
                    path: path.to_string(),
                }
            }
            other => return Err(at(format!("unknown kind {other:?}"))),
        };
        check_path(entry.path()).map_err(at)?;
        entries.push(entry);
    }
    let mut paths: Vec<&str> = entries.iter().map(Entry::path).collect();
    paths.sort_unstable();
    if let Some(p) = paths.windows(2).find(|p| p[0] == p[1]) {
        return Err(format!("{:?} is in the MANIFEST twice", p[0]));
    }
    let (mut f, mut wo, mut b) = (0u64, 0u64, 0u64);
    for e in &entries {
        match e {
            Entry::File { size, .. } => {
                f += 1;
                b = b.checked_add(*size).ok_or("the sizes overflow")?;
            }
            Entry::Whiteout { .. } => wo += 1,
            _ => {}
        }
    }
    if (f, wo, b) != (files, whiteouts, bytes) {
        return Err(format!(
            "the first line says files={files} whiteouts={whiteouts} bytes={bytes}, the lines have {f} files, {wo} whiteouts, {b} bytes"
        ));
    }
    Ok(Manifest {
        installed,
        stream,
        files,
        whiteouts,
        bytes,
        entries,
    })
}

/// The order the entries are applied in: directories first, parents before children, then files and links, then whiteouts.
pub fn apply_order(entries: &[Entry]) -> Vec<&Entry> {
    let rank = |e: &Entry| match e {
        Entry::Dir { .. } => 0,
        Entry::File { .. } | Entry::Link { .. } => 1,
        Entry::Whiteout { .. } => 2,
    };
    let mut out: Vec<&Entry> = entries.iter().collect();
    out.sort_by(|a, b| rank(a).cmp(&rank(b)).then(a.path().cmp(b.path())));
    out
}

/// The parents of `path` from the top, `/a/b/c` -> `/a`, `/a/b`.
pub fn parents(path: &str) -> Vec<&str> {
    path.match_indices('/')
        .skip(1)
        .map(|(i, _)| &path[..i])
        .collect()
}

/// The first 12 digits of a commit, or `-` when there is none.
pub fn short(commit: Option<&str>) -> &str {
    commit.map_or("-", |c| &c[..c.len().min(12)])
}

/// The marker: the first line init writes to the kernel log, the saved console and [`MARKER_FILE`] on a test boot, so that a reset
/// during it is never read as a reset of the installed generation.
pub fn marker(
    mode: &str,
    data: Data,
    installed: Option<&str>,
    stream: Option<&str>,
    files: u64,
    whiteouts: u64,
) -> String {
    format!(
        "test root: stream (mode {mode}, data {}, installed {}, stream {}, overlaid {files} files, {whiteouts} whiteouts)",
        data.name(),
        short(installed),
        short(stream)
    )
}

/// `lucina.root.uuid=`'s form of an ext2 superblock's UUID: the 16 bytes at 1024 + 104 in lowercase hex.
pub fn superblock_uuid(first_2k: &[u8]) -> Option<String> {
    let uuid = first_2k.get(1024 + 104..1024 + 120)?;
    Some(uuid.iter().map(|b| format!("{b:02x}")).collect())
}

/// The value of a kernel argument (before ` -- `) on a command line.
pub fn kernel_arg<'a>(cmdline: &'a str, key: &str) -> Option<&'a str> {
    cmdline
        .split_ascii_whitespace()
        .take_while(|w| *w != "--")
        .filter_map(|w| w.strip_prefix(key)?.strip_prefix('='))
        .last()
}

#[cfg(test)]
mod tests {
    use super::*;

    const I: &str = "1111111111111111111111111111111111111111";
    const S: &str = "2222222222222222222222222222222222222222";
    const SUM: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    fn head(files: u64, whiteouts: u64, bytes: u64) -> String {
        format!(
            "lucina-overlay 1 installed={I} stream={S} files={files} whiteouts={whiteouts} bytes={bytes}\n"
        )
    }

    #[test]
    fn a_manifest_as_the_build_writes_it() {
        let text = head(2, 1, 7)
            + &format!(
                "D 0755 /usr/share/x\nF 0755 0 {SUM} /usr/bin/my tool\nF 0644 7 {SUM} /usr/share/x/a\nL ../lib/y /usr/bin/z\nW /etc/old\n"
            );
        let m = parse(&text).unwrap();
        assert_eq!(
            (m.files, m.whiteouts, m.bytes, m.entries.len()),
            (2, 1, 7, 5)
        );
        assert_eq!(m.entries[1].path(), "/usr/bin/my tool");
        assert_eq!(
            m.entries[0],
            Entry::Dir {
                mode: 0o755,
                path: "/usr/share/x".into()
            }
        );
        assert_eq!(
            m.entries[3],
            Entry::Link {
                target: "../lib/y".into(),
                path: "/usr/bin/z".into()
            }
        );
    }

    #[test]
    fn counts_that_disagree_refuse() {
        let e = parse(&(head(1, 0, 1) + &format!("F 0644 2 {SUM} /a\n"))).unwrap_err();
        assert!(e.contains("bytes=1"), "{e}");
        assert!(parse(&(head(0, 0, 0) + "W /a\n")).is_err());
    }

    #[test]
    fn bad_paths_refuse() {
        for p in [
            "a",
            "/",
            "/a/../b",
            "/a//b",
            "/a/./b",
            "/lucina-overlay/MANIFEST",
            "/lucina-overlay",
            "/a\\b",
        ] {
            assert!(
                parse(&(head(0, 1, 0) + &format!("W {p}\n"))).is_err(),
                "{p}"
            );
        }
        assert!(
            parse(&(head(0, 2, 0) + "W /a\nW /a\n"))
                .unwrap_err()
                .contains("twice")
        );
    }

    #[test]
    fn bad_lines_refuse() {
        for line in [
            format!("F 0644 0 {SUM}"),
            "F 0644 0 abc /a".to_string(),
            format!("F 9644 0 {SUM} /a"),
            "L a b /c".to_string(),
            "X /a".to_string(),
            "D /a".to_string(),
        ] {
            assert!(parse(&(head(1, 0, 0) + &line + "\n")).is_err(), "{line}");
        }
        assert!(parse("lucina-overlay 2 installed=").is_err());
        assert!(
            parse(&format!(
                "lucina-overlay 1 installed={I} stream=xyz files=0 whiteouts=0 bytes=0"
            ))
            .is_err()
        );
        assert!(
            parse(&format!(
                "lucina-overlay 1 installed={I} stream={S} files=0 whiteouts=0 bytes=0 more"
            ))
            .is_err()
        );
    }

    #[test]
    fn directories_go_first_and_parents_before_children() {
        let text =
            head(1, 1, 0) + &format!("W /a/x\nF 0644 0 {SUM} /a/b/c\nD 0700 /a/b\nD 0755 /a\n");
        let m = parse(&text).unwrap();
        let order: Vec<&str> = apply_order(&m.entries).iter().map(|e| e.path()).collect();
        assert_eq!(order, ["/a", "/a/b", "/a/b/c", "/a/x"]);
        assert_eq!(parents("/a/b/c"), ["/a", "/a/b"]);
        assert!(parents("/a").is_empty());
    }

    #[test]
    fn the_marker() {
        assert_eq!(
            marker("overlay", Data::ReadOnly, Some(I), Some(S), 3, 1),
            "test root: stream (mode overlay, data ro, installed 111111111111, stream 222222222222, overlaid 3 files, 1 whiteouts)"
        );
        assert_eq!(
            marker("initramfs", Data::None, None, None, 0, 0),
            "test root: stream (mode initramfs, data none, installed -, stream -, overlaid 0 files, 0 whiteouts)"
        );
    }

    #[test]
    fn the_chosen_properties() {
        let dir = std::env::temp_dir().join(format!("lucina-testroot-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(test_boot_in(&dir), None);
        fs::write(dir.join("lucina,reload-root"), b"overlay\0").unwrap();
        assert_eq!(
            test_boot_in(&dir),
            Some(TestBoot {
                root: "overlay",
                data: Data::None
            })
        );
        fs::write(dir.join("lucina,reload-data"), b"rw\0").unwrap();
        assert_eq!(test_boot_in(&dir).unwrap().data, Data::ReadWrite);
        fs::write(dir.join("lucina,reload-data"), b"what\0").unwrap();
        assert_eq!(test_boot_in(&dir).unwrap().data, Data::None);
        fs::write(dir.join("lucina,reload-root"), b"ram\0").unwrap();
        assert_eq!(test_boot_in(&dir), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn command_line_values() {
        let line = "root=/dev/a ro init=/etc/lucina/init.sh lucina.root.uuid=ab root=/dev/b -- root=/dev/c";
        assert_eq!(kernel_arg(line, "root"), Some("/dev/b"));
        assert_eq!(kernel_arg(line, "init"), Some("/etc/lucina/init.sh"));
        assert_eq!(kernel_arg(line, "rdinit"), None);
        let mut sb = vec![0u8; 2048];
        sb[1128] = 0xab;
        sb[1143] = 0x01;
        assert_eq!(
            superblock_uuid(&sb).unwrap(),
            "ab000000000000000000000000000001"
        );
    }
}
