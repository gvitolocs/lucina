//! Where Lucina's file tools may go: anywhere the signed-in user can (root on Juno), but never the sign-in's folders, devd's own
//! audit log, or what `offlimits` names (UFS LUNs 1-5, the Qualcomm firmware, the PMIC's charge and current limits, which may
//! be read but not written). Relative paths are in the owner's home (`/data/home`, or `/run/lucina/files` on a boot without
//! `/data`). A path is resolved first, `..` and symlinks included, and checked both as given and as resolved, so a link that
//! points at a refused place is refused like the place.
use std::ffi::OsStr;
use std::os::fd::AsRawFd;
use std::path::{Component, Path, PathBuf};

use crate::offlimits;

/// Never reachable through a file tool: the machine's sign-in (`lucina-accountd`) and this service's audit log, on /data or in
/// memory.
const DENIED: [&str; 4] = [
    "data/lucina/secretary",
    "run/lucina/secretary",
    "data/lucina/devd",
    "run/lucina/devd",
];

/// What a path is for: reading may see the charge limits, writing, removing and listing check every rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Use {
    Read,
    Change,
}

#[derive(Clone, Debug)]
pub struct Places {
    /// `/` on the laptop; a temporary folder in the tests.
    root: PathBuf,
}

impl Places {
    pub fn under(root: &Path) -> Places {
        Places {
            root: root.to_path_buf(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn at(&self, path: &str) -> PathBuf {
        self.root.join(path)
    }

    /// The owner's files, as `lucina_files::places::home` decides it.
    pub fn home(&self) -> PathBuf {
        if self.at("run/lucina/data-ready").exists() {
            self.at("data/home")
        } else {
            self.at("run/lucina/files")
        }
    }

    /// Where `play_file` may also look: the home and the music folders Player lists.
    pub fn media(&self) -> Vec<PathBuf> {
        vec![
            self.home(),
            self.at("data/music"),
            self.at("data/recordings"),
        ]
    }

    pub fn denied(&self) -> Vec<PathBuf> {
        DENIED.iter().map(|d| self.at(d)).collect()
    }

    /// Everything the user reaches: the whole tree.
    pub fn anywhere(&self) -> Vec<PathBuf> {
        vec![self.root.clone()]
    }

    /// A path to read: it must exist and be no refused place, as given or links followed.
    pub fn existing(&self, path: &str) -> Result<PathBuf, String> {
        self.existing_for(path, &self.anywhere(), Use::Read)
    }

    /// A folder to list: as for reading, with every rule.
    pub fn listing(&self, path: &str) -> Result<PathBuf, String> {
        self.existing_for(path, &self.anywhere(), Use::Change)
    }

    /// The same within any of `allowed`.
    pub fn existing_in(&self, path: &str, allowed: &[PathBuf]) -> Result<PathBuf, String> {
        self.existing_for(path, allowed, Use::Read)
    }

    fn existing_for(&self, path: &str, allowed: &[PathBuf], how: Use) -> Result<PathBuf, String> {
        let full = self.absolute(path)?;
        self.refuse_denied(&full)?;
        self.off_limits(&full, how)?;
        let real = std::fs::canonicalize(&full).map_err(|e| format!("{path}: {}", plain(&e)))?;
        self.inside(&real, allowed, path)?;
        self.off_limits(&real, how)?;
        Ok(real)
    }

    /// `offlimits` on a full path under this root.
    fn off_limits(&self, full: &Path, how: Use) -> Result<(), String> {
        let rel = full.strip_prefix(&self.root).unwrap_or(full);
        let as_root = Path::new("/").join(rel);
        let r = match how {
            Use::Read => offlimits::check_read(&self.root, &as_root),
            Use::Change => offlimits::check_path(&self.root, &as_root),
        };
        r.map_err(|offlimits::Refusal(why)| format!("{}: refused: {why}", as_root.display()))
    }

    /// A path to create, replace or remove: its folder must exist, and the name itself is not followed (a link there is the link).
    /// Never the home or the root itself.
    pub fn entry(&self, path: &str) -> Result<PathBuf, String> {
        let full = self.absolute(path)?;
        self.refuse_denied(&full)?;
        self.off_limits(&full, Use::Change)?;
        let Some(Component::Normal(name)) = full.components().next_back() else {
            return Err(format!("{path}: not a file name"));
        };
        let name = name.to_os_string();
        let parent = full.parent().ok_or_else(|| format!("{path}: no folder"))?;
        let real_parent = std::fs::canonicalize(parent)
            .map_err(|e| format!("{}: {}", parent.display(), plain(&e)))?;
        let real = real_parent.join(&name);
        self.inside(&real, &self.anywhere(), path)?;
        self.off_limits(&real, Use::Change)?;
        if Some(real.as_path()) == self.canonical(&self.home()).as_deref() {
            return Err(format!("{path}: that is the home folder itself"));
        }
        Ok(real)
    }

    /// Relative paths are in the home; `..` is taken lexically here and again by the file system when resolving.
    fn absolute(&self, path: &str) -> Result<PathBuf, String> {
        if path.is_empty() {
            return Err("no path".into());
        }
        if path.contains('\0') {
            return Err("a path cannot hold a NUL byte".into());
        }
        if path.len() > 4096 {
            return Err("the path is too long".into());
        }
        let p = Path::new(path);
        let full = if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.home().join(p)
        };
        let mut out = PathBuf::new();
        for c in full.components() {
            match c {
                Component::ParentDir => {
                    out.pop();
                }
                Component::CurDir => {}
                other => out.push(other.as_os_str()),
            }
        }
        Ok(out)
    }

    fn canonical(&self, p: &Path) -> Option<PathBuf> {
        std::fs::canonicalize(p).ok()
    }

    fn refuse_denied(&self, p: &Path) -> Result<(), String> {
        for d in self.denied() {
            let real = self.canonical(&d);
            if p.starts_with(&d) || real.as_ref().is_some_and(|r| p.starts_with(r)) {
                return Err(format!(
                    "{}: refused: the sign-in and devd's own files are not reachable",
                    p.display()
                ));
            }
        }
        Ok(())
    }

    fn inside(&self, real: &Path, allowed: &[PathBuf], asked: &str) -> Result<(), String> {
        self.refuse_denied(real)?;
        let ok = allowed
            .iter()
            .filter_map(|a| self.canonical(a))
            .any(|a| real.starts_with(&a));
        if ok {
            Ok(())
        } else {
            let names: Vec<String> = allowed.iter().map(|a| a.display().to_string()).collect();
            Err(format!(
                "{asked}: refused: only {} can be reached",
                names.join(" and ")
            ))
        }
    }

    /// After opening: where the descriptor really is (`/proc/self/fd`), checked again, so a link swapped in between the check
    /// and the open is caught. A kernel without that link keeps the first check.
    pub fn still_inside(&self, fd: &impl AsRawFd, allowed: &[PathBuf]) -> Result<(), String> {
        let Ok(real) = std::fs::read_link(format!("/proc/self/fd/{}", fd.as_raw_fd())) else {
            return Ok(());
        };
        // Only a path in the same tree can be compared (a test's temporary root is a real path too).
        if !real.is_absolute() {
            return Ok(());
        }
        self.inside(&real, allowed, &real.display().to_string())
    }
}

/// An I/O error as a short sentence, without Rust's "(os error 2)".
pub fn plain(e: &std::io::Error) -> String {
    let s = e.to_string();
    match s.find(" (os error") {
        Some(i) => s[..i].to_owned(),
        None => s,
    }
}

/// A file name that is ours for the moment: next to the target, so the rename stays on one file system.
pub fn temp_name(name: &OsStr) -> std::ffi::OsString {
    let mut t = std::ffi::OsString::from(".");
    t.push(name);
    t.push(format!(".devd-{}", std::process::id()));
    t
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    pub(crate) fn machine(name: &str) -> (PathBuf, Places) {
        let root =
            std::env::temp_dir().join(format!("lucina-devd-places-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in [
            "data/home/Docs",
            "data/lucina/secretary",
            "data/lucina/devd",
            "run/lucina/secretary",
            "data/music",
            "etc",
        ] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::fs::write(root.join("run/lucina/data-ready"), b"").unwrap();
        std::fs::write(root.join("data/home/Docs/a.txt"), b"hello").unwrap();
        std::fs::write(root.join("data/lucina/secretary/credential"), b"sec_x").unwrap();
        std::fs::write(root.join("etc/passwd"), b"root").unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        (root.clone(), Places::under(&root))
    }

    fn s(p: &Path) -> String {
        p.display().to_string()
    }

    #[test]
    fn the_home_follows_data_ready() {
        let (root, places) = machine("home");
        assert_eq!(places.home(), root.join("data/home"));
        std::fs::remove_file(root.join("run/lucina/data-ready")).unwrap();
        assert_eq!(places.home(), root.join("run/lucina/files"));
    }

    #[test]
    fn a_file_in_the_home_is_reachable_by_full_and_relative_path() {
        let (root, places) = machine("reach");
        let full = root.join("data/home/Docs/a.txt");
        assert_eq!(places.existing(&s(&full)).unwrap(), full);
        assert_eq!(places.existing("Docs/a.txt").unwrap(), full);
        assert_eq!(places.existing("Docs/../Docs/./a.txt").unwrap(), full);
    }

    #[test]
    fn paths_out_of_the_home_are_reachable_as_the_user() {
        let (root, places) = machine("dotdot");
        let passwd = root.join("etc/passwd");
        assert_eq!(
            places
                .existing(&s(&root.join("data/home/../../etc/passwd")))
                .unwrap(),
            passwd
        );
        assert_eq!(places.existing("../../etc/passwd").unwrap(), passwd);
        assert_eq!(places.entry("../x").unwrap(), root.join("data/x"));
        assert_eq!(
            places.listing(&s(&root.join("etc"))).unwrap(),
            root.join("etc")
        );
    }

    #[test]
    fn a_symlink_to_an_off_limits_place_is_refused_like_the_place() {
        let (root, places) = machine("symlink");
        let home = root.join("data/home");
        std::fs::create_dir_all(root.join("dev")).unwrap();
        std::fs::write(root.join("dev/ufs0lun2"), b"").unwrap();
        std::fs::create_dir_all(root.join("lib/firmware/qcom")).unwrap();
        std::fs::write(root.join("lib/firmware/qcom/a.mbn"), b"fw").unwrap();
        symlink(root.join("dev/ufs0lun2"), root.join("dev/disk-alias")).unwrap();
        symlink(root.join("lib/firmware/qcom"), home.join("fw")).unwrap();
        for p in [
            "/dev/ufs0lun2",
            "/dev/disk-alias",
            "/lib/firmware/qcom/a.mbn",
        ] {
            let full = s(&root.join(&p[1..]));
            assert!(
                places.existing(&full).unwrap_err().contains("off-limits"),
                "{p}"
            );
            assert!(
                places.entry(&full).unwrap_err().contains("off-limits"),
                "{p}"
            );
        }
        assert!(
            places
                .existing("fw/a.mbn")
                .unwrap_err()
                .contains("off-limits")
        );
        // Writing through a linked folder resolves the folder.
        assert!(places.entry("fw/new").unwrap_err().contains("off-limits"));
        // An ordinary link out of the home is the user's to follow, and removing it removes the link.
        symlink(root.join("etc/passwd"), home.join("passwd")).unwrap();
        assert_eq!(places.existing("passwd").unwrap(), root.join("etc/passwd"));
        assert_eq!(places.entry("passwd").unwrap(), home.join("passwd"));
    }

    #[test]
    fn charge_limits_may_be_read_but_not_written() {
        let (root, places) = machine("limits");
        let dir = root.join("sys/class/power_supply/battery");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("input_current_limit"), b"1500000").unwrap();
        std::fs::write(dir.join("capacity"), b"80").unwrap();
        let limit = s(&dir.join("input_current_limit"));
        assert!(places.existing(&limit).is_ok());
        assert!(places.entry(&limit).unwrap_err().contains("off-limits"));
        assert!(places.entry(&s(&dir.join("capacity"))).is_ok());
    }

    #[test]
    fn the_sign_in_and_devds_folder_are_refused_even_through_a_link() {
        let (root, places) = machine("denied");
        for p in [
            "data/lucina/secretary/credential",
            "run/lucina/secretary",
            "data/lucina/devd/audit.jsonl",
        ] {
            let e = places.existing(&s(&root.join(p))).unwrap_err();
            assert!(e.contains("refused"), "{p}: {e}");
            assert!(
                places
                    .entry(&s(&root.join(p)))
                    .unwrap_err()
                    .contains("refused")
            );
        }
        symlink(root.join("data/lucina/secretary"), root.join("data/home/s")).unwrap();
        let e = places.existing("s/credential").unwrap_err();
        assert!(e.contains("sign-in"), "{e}");
    }

    #[test]
    fn even_a_home_that_held_the_sign_in_would_not_reach_it() {
        // A machine whose home were a link to /data/lucina still refuses the folder by name.
        let (root, _) = machine("homelink");
        std::fs::remove_dir_all(root.join("data/home")).unwrap();
        symlink(root.join("data/lucina"), root.join("data/home")).unwrap();
        let places = Places::under(&root);
        assert!(
            places
                .existing("secretary/credential")
                .unwrap_err()
                .contains("refused")
        );
    }

    #[test]
    fn odd_paths_are_refused() {
        let (_, places) = machine("odd");
        assert!(places.existing("").is_err());
        assert!(places.existing("a\0b").is_err());
        assert!(places.existing(&"a/".repeat(3000)).is_err());
        assert!(places.entry("/").is_err());
        assert!(
            places
                .entry(&s(&places.home()))
                .unwrap_err()
                .contains("home folder itself")
        );
        assert!(
            places
                .existing("Docs/missing.txt")
                .unwrap_err()
                .contains("No such file")
        );
    }

    #[test]
    fn media_may_also_come_from_the_music_folder() {
        let (root, places) = machine("media");
        std::fs::write(root.join("data/music/song.mp3"), b"x").unwrap();
        let m = s(&root.join("data/music/song.mp3"));
        assert!(places.existing_in(&m, &[places.home()]).is_err());
        assert_eq!(
            places.existing_in(&m, &places.media()).unwrap(),
            root.join("data/music/song.mp3")
        );
    }

    #[test]
    fn an_open_file_is_checked_where_it_really_is() {
        let (root, places) = machine("fd");
        let inside = std::fs::File::open(root.join("data/home/Docs/a.txt")).unwrap();
        assert!(places.still_inside(&inside, &[places.home()]).is_ok());
        let outside = std::fs::File::open(root.join("etc/passwd")).unwrap();
        assert!(places.still_inside(&outside, &[places.home()]).is_err());
        assert!(places.still_inside(&outside, &places.anywhere()).is_ok());
    }
}
