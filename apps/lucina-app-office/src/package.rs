//! `office.pkg` into place: its sum is checked against the one built into this program, it is unpacked into a fresh private folder,
//! and that folder replaces whatever was at the final path. So what is in `/tmp` before this runs is never used. Another program
//! running as the same user can still change the folder afterwards: only a sandbox can stop that.

use crate::member_path::member_path_ok;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::Path;

fn sum_of(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buffer).map_err(|e| format!("{}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Only plain files and folders, all the way down.
fn only_files_and_folders(dir: &Path) -> Result<(), String> {
    for entry in std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let kind = std::fs::symlink_metadata(entry.path()).map_err(|e| e.to_string())?.file_type();
        if kind.is_dir() {
            only_files_and_folders(&entry.path())?;
        } else if !kind.is_file() {
            return Err(format!("{} is not a file", entry.path().display()));
        }
    }
    Ok(())
}

fn remove(path: &Path) -> Result<(), String> {
    let Ok(meta) = std::fs::symlink_metadata(path) else { return Ok(()) };
    let done = if meta.file_type().is_dir() { std::fs::remove_dir_all(path) } else { std::fs::remove_file(path) };
    done.map_err(|e| format!("{}: {e}", path.display()))
}

fn unpack_into(pkg: &Path, dir: &Path, tar: &Path, also: Option<(&Path, &str)>) -> Result<(), String> {
    let listing = std::process::Command::new(tar).arg("tzf").arg(pkg).output().map_err(|e| format!("tar: {e}"))?;
    if !listing.status.success() {
        return Err(format!("reading {}: {}", pkg.display(), listing.status));
    }
    for name in listing.stdout.split(|&b| b == b'\n').filter(|n| !n.is_empty()) {
        if !member_path_ok(name) {
            return Err(format!("{} holds a path it may not: {}", pkg.display(), String::from_utf8_lossy(name)));
        }
    }
    let status = std::process::Command::new(tar).arg("xzf").arg(pkg).arg("-C").arg(dir).status().map_err(|e| format!("tar: {e}"))?;
    if !status.success() {
        return Err(format!("unpacking {}: {status}", pkg.display()));
    }
    only_files_and_folders(dir)?;
    if let Some((program, name)) = also {
        let to = dir.join(name);
        std::fs::copy(program, &to).map_err(|e| format!("{}: {e}", to.display()))?;
        std::fs::set_permissions(&to, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Puts the checked contents of `pkg` at `target`. `sum` is its SHA-256 in hex; `also` is one more program to put in, under a name.
pub fn install(pkg: &Path, sum: &str, target: &Path, tar: &Path, also: Option<(&Path, &str)>) -> Result<(), String> {
    if sum.is_empty() {
        return Err("this build does not know the sum of its package".into());
    }
    let found = sum_of(pkg)?;
    if found != sum {
        return Err(format!("{} is not the package this program was built with", pkg.display()));
    }
    let parent = target.parent().ok_or("no parent folder")?;
    // Fails if it exists, so nobody prepared it.
    let fresh = parent.join(format!(".{}.{}", target.file_name().and_then(|n| n.to_str()).unwrap_or("package"), unique()));
    std::fs::DirBuilder::new().mode(0o700).create(&fresh).map_err(|e| format!("{}: {e}", fresh.display()))?;
    let done = unpack_into(pkg, &fresh, tar, also).and_then(|()| {
        remove(target)?;
        std::fs::rename(&fresh, target).map_err(|e| format!("{}: {e}", target.display()))
    });
    if done.is_err() {
        let _ = remove(&fresh);
    }
    done
}

fn unique() -> String {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("{}-{nanos}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const TAR: &str = "tar";

    struct Fixture {
        root: PathBuf,
        pkg: PathBuf,
        sum: String,
    }

    /// A package made by the system's tar from `make`'s files, in a folder of its own.
    fn fixture(tag: &str, make: impl FnOnce(&Path)) -> Fixture {
        let root = std::env::temp_dir().join(format!("office-pkg-{tag}-{}", unique()));
        let tree = root.join("tree");
        std::fs::create_dir_all(&tree).unwrap();
        make(&tree);
        let pkg = root.join("office.pkg");
        let ok = std::process::Command::new(TAR).arg("czf").arg(&pkg).arg("-C").arg(&tree).arg(".").status().unwrap();
        assert!(ok.success());
        let sum = sum_of(&pkg).unwrap();
        Fixture { root, pkg, sum }
    }

    /// Without the leading `./` that `tar -C tree .` gives.
    fn clean_fixture(tag: &str, files: &[(&str, &str)]) -> Fixture {
        let root = std::env::temp_dir().join(format!("office-pkg-{tag}-{}", unique()));
        let tree = root.join("tree");
        std::fs::create_dir_all(&tree).unwrap();
        for (name, text) in files {
            let path = tree.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        let pkg = root.join("office.pkg");
        let names: Vec<_> = files.iter().map(|(n, _)| *n).collect();
        let ok = std::process::Command::new(TAR).arg("czf").arg(&pkg).arg("-C").arg(&tree).args(names).status().unwrap();
        assert!(ok.success());
        let sum = sum_of(&pkg).unwrap();
        Fixture { root, pkg, sum }
    }

    #[test]
    fn member_paths() {
        for good in ["a", "a/b", "fonts/x.ttf", "a/", "a/b/", "..a", "a..", "a/.b", "...", "a b"] {
            assert!(member_path_ok(good.as_bytes()), "{good}");
        }
        for bad in ["", "/", "/a", "..", "../a", "a/..", "a/../b", ".", "./a", "a/./b", "a//b", "a\\b", "a\0b", "//a", "a/b//"] {
            assert!(!member_path_ok(bad.as_bytes()), "{bad:?}");
        }
        assert!(!member_path_ok(&[b'a'; 513]));
        assert!(member_path_ok(&[b'a'; 512]));
    }

    #[test]
    fn a_good_package_replaces_what_was_planted() {
        let f = clean_fixture("good", &[("ibis-office", "real"), ("fonts/a.ttf", "font")]);
        let target = f.root.join("installed");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("ibis-office"), "planted").unwrap();
        std::fs::write(target.join("extra"), "planted").unwrap();
        install(&f.pkg, &f.sum, &target, Path::new(TAR), None).unwrap();
        assert_eq!(std::fs::read_to_string(target.join("ibis-office")).unwrap(), "real");
        assert_eq!(std::fs::read_to_string(target.join("fonts/a.ttf")).unwrap(), "font");
        assert!(!target.join("extra").exists(), "the planted tree must go");
        let _ = std::fs::remove_dir_all(&f.root);
    }

    #[test]
    fn a_planted_link_at_the_target_is_replaced_not_followed() {
        let f = clean_fixture("link", &[("ibis-office", "real")]);
        let elsewhere = f.root.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let target = f.root.join("installed");
        std::os::unix::fs::symlink(&elsewhere, &target).unwrap();
        install(&f.pkg, &f.sum, &target, Path::new(TAR), None).unwrap();
        assert!(std::fs::symlink_metadata(&target).unwrap().file_type().is_dir());
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&f.root);
    }

    #[test]
    fn a_wrong_sum_changes_nothing() {
        let f = clean_fixture("sum", &[("ibis-office", "real")]);
        let target = f.root.join("installed");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("keep"), "x").unwrap();
        let e = install(&f.pkg, &"0".repeat(64), &target, Path::new(TAR), None).unwrap_err();
        assert!(e.contains("not the package"), "{e}");
        assert!(target.join("keep").exists());
        assert!(install(&f.pkg, "", &target, Path::new(TAR), None).is_err());
        let _ = std::fs::remove_dir_all(&f.root);
    }

    #[test]
    fn members_it_may_not_write_stop_it_before_anything_is_unpacked() {
        // `.` and `./x`: what tar -C tree . writes.
        let f = fixture("dots", |tree| std::fs::write(tree.join("x"), "x").unwrap());
        let target = f.root.join("installed");
        let e = install(&f.pkg, &f.sum, &target, Path::new(TAR), None).unwrap_err();
        assert!(e.contains("may not"), "{e}");
        assert!(!target.exists());
        let left: Vec<_> = std::fs::read_dir(&f.root).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert!(!left.iter().any(|n| n.to_string_lossy().starts_with(".installed")), "{left:?}");
        let _ = std::fs::remove_dir_all(&f.root);
    }

    #[test]
    fn a_link_inside_the_package_is_refused() {
        let root = std::env::temp_dir().join(format!("office-pkg-inner-{}", unique()));
        let tree = root.join("tree");
        std::fs::create_dir_all(&tree).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", tree.join("a")).unwrap();
        let pkg = root.join("office.pkg");
        assert!(std::process::Command::new(TAR).arg("czf").arg(&pkg).arg("-C").arg(&tree).arg("a").status().unwrap().success());
        let sum = sum_of(&pkg).unwrap();
        let target = root.join("installed");
        let e = install(&pkg, &sum, &target, Path::new(TAR), None).unwrap_err();
        assert!(e.contains("not a file"), "{e}");
        assert!(!target.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_extra_program_goes_in_executable() {
        let f = clean_fixture("also", &[("ibis-office", "real")]);
        let program = f.root.join("self");
        std::fs::write(&program, "me").unwrap();
        let target = f.root.join("installed");
        install(&f.pkg, &f.sum, &target, Path::new(TAR), Some((&program, "list"))).unwrap();
        let meta = std::fs::metadata(target.join("list")).unwrap();
        assert_eq!(meta.permissions().mode() & 0o111, 0o111);
        let _ = std::fs::remove_dir_all(&f.root);
    }
}
