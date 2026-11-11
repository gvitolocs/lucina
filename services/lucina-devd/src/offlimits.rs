//! What Lucina may never touch on the laptop, whatever she is otherwise allowed: the UFS LUNs that hold the boot set,
//! the Qualcomm firmware, the PMIC charge and current limits, and the sign-in token's folders. Pure functions, so every
//! tool calls the same decision.

use std::path::{Path, PathBuf};

/// Why something is refused: a short English sentence for the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal(pub String);

/// UFS LUNs 1 to 5 and their partitions; LUN 0 is the one she may use.
const UFS_LUNS: &str = "/dev/ufs0lun";
/// The Qualcomm firmware and the partitions that mount it. Reading is refused too.
const FIRMWARE: &[&str] = &[
    "/lib/firmware/qcom",
    "/lib/firmware/ath10k",
    "/vendor/firmware",
    "/vendor/firmware_mnt",
    "/firmware",
    "/dsp",
];
/// Under `/sys/class/power_supply/<any>/`: any file whose name starts with one of these is a charge or current limit.
const LIMIT_PREFIX: &str = "/sys/class/power_supply/";
const LIMIT_NAMES: &[&str] = &["constant_charge", "input_current_limit", "charge_control"];
/// The sign-in token's folders: read and write.
const SECRETARY: &[&str] = &["/data/lucina/secretary", "/run/lucina/secretary"];

/// The path as `root` sees it: `root` prefixes every absolute path ("/" on the laptop, a temp dir in tests).
fn rooted(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        root.join(path.strip_prefix("/").unwrap_or(path))
    } else {
        root.join(path)
    }
}

/// The canonical form: the path itself if it exists, else its nearest existing parent canonicalized with the rest re-joined,
/// so a symlink alias is caught.
fn canonical(root: &Path, path: &Path) -> PathBuf {
    let full = rooted(root, path);
    if full.exists() {
        return std::fs::canonicalize(&full).unwrap_or(full);
    }
    let mut rest = full.clone();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    while !rest.exists() {
        match rest.file_name() {
            Some(name) => {
                tail.push(name.to_owned());
                rest.pop();
            }
            None => return full,
        }
    }
    let mut canon = std::fs::canonicalize(&rest).unwrap_or(rest);
    for name in tail.into_iter().rev() {
        canon.push(name);
    }
    canon
}

/// True when `path` (a `/`-separated absolute path string) is `prefix` or something under it.
fn under_prefix(path: &str, prefix: &str) -> bool {
    path == prefix || path.starts_with(prefix) && path.as_bytes().get(prefix.len()) == Some(&b'/')
}

/// Rule 1: UFS LUNs 1 to 5, with or without a partition suffix (`/dev/ufs0lun3p2`).
fn ufs_lun(path: &str) -> Option<usize> {
    let rest = path.strip_prefix(UFS_LUNS)?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    let lun: usize = digits.parse().ok()?;
    if !(1..=5).contains(&lun) {
        return None;
    }
    let suffix = &rest[digits.len()..];
    if suffix.is_empty()
        || (suffix.starts_with('p')
            && suffix[1..].bytes().all(|b| b.is_ascii_digit())
            && !suffix[1..].is_empty())
    {
        Some(lun)
    } else {
        None
    }
}

/// Rule 3: a PMIC charge or current limit file under `/sys/class/power_supply/<any>/`.
fn is_limit_file(path: &str) -> bool {
    path.strip_prefix(LIMIT_PREFIX).is_some_and(|rest| {
        let name = rest.rsplit('/').next().unwrap_or(rest);
        LIMIT_NAMES.iter().any(|p| name.starts_with(p))
    })
}

/// The rule `path` breaks, if any. `allow_limits` is `check_read`: reading the limits is fine.
fn rule_for(path: &str, allow_limits: bool) -> Option<Refusal> {
    if let Some(lun) = ufs_lun(path) {
        return Some(Refusal(format!("UFS LUN {lun} is off-limits")));
    }
    if FIRMWARE.iter().any(|p| under_prefix(path, p)) {
        return Some(Refusal("the Qualcomm firmware is off-limits".to_owned()));
    }
    if !allow_limits && is_limit_file(path) {
        return Some(Refusal(
            "the charge and current limits are off-limits".to_owned(),
        ));
    }
    if SECRETARY.iter().any(|p| under_prefix(path, p)) {
        return Some(Refusal(
            "the sign-in token's folder is off-limits".to_owned(),
        ));
    }
    None
}

/// Checks a path as given and in its canonical form. Used for writes, deletes and listings: rule 3 is refused.
pub fn check_path(root: &Path, path: &Path) -> Result<(), Refusal> {
    check(root, path, false)
}

/// Like [`check_path`], but reading the charge and current limits is allowed.
pub fn check_read(root: &Path, path: &Path) -> Result<(), Refusal> {
    check(root, path, true)
}

fn check(root: &Path, path: &Path, allow_limits: bool) -> Result<(), Refusal> {
    // The path as given, then its canonical form (under `root` in tests, compared after it).
    let given = path.to_string_lossy().replace('\\', "/");
    if let Some(r) = rule_for(&given, allow_limits) {
        return Err(r);
    }
    let canon = canonical(root, path).to_string_lossy().replace('\\', "/");
    let root_s = root.to_string_lossy().replace('\\', "/");
    let canon_rel = canon
        .strip_prefix(&root_s)
        .map(|s| {
            if s.starts_with('/') {
                s.to_owned()
            } else {
                format!("/{s}")
            }
        })
        .unwrap_or(canon);
    if let Some(r) = rule_for(&canon_rel, allow_limits) {
        return Err(r);
    }
    Ok(())
}

/// True when `text` contains `token` as a word: the character before it is not a name character.
fn names_word(text: &str, token: &str) -> bool {
    let mut start = 0;
    while let Some(i) = text[start..].find(token) {
        let at = start + i;
        let before_ok = at == 0
            || !text[..at]
                .chars()
                .last()
                .is_some_and(|c| c.is_alphanumeric() || c == '_');
        if before_ok {
            return true;
        }
        start = at + 1;
    }
    false
}

/// Write indicators: a redirect, or a program that writes its target.
fn writes(command: &str) -> bool {
    command.contains('>')
        || ["tee", "dd", "cp", "mv", "install"]
            .iter()
            .any(|p| names_word(command, p))
}

/// A shell command line: refuse if its text names any off-limits path or pattern. A guard against mistakes, not a sandbox.
pub fn check_command(command: &str) -> Result<(), Refusal> {
    // Rule 1: the node path, or the bare token with or without a partition suffix.
    for lun in 1..=5u8 {
        for suffix in ["", "p1", "p2", "p3"] {
            if names_word(command, &format!("ufs0lun{lun}{suffix}")) {
                return Err(Refusal(format!("UFS LUN {lun} is off-limits")));
            }
        }
    }
    // Rule 2: the firmware paths.
    for p in FIRMWARE {
        if command.contains(p) {
            return Err(Refusal("the Qualcomm firmware is off-limits".to_owned()));
        }
    }
    // Rule 3: a limit file named together with a write.
    if LIMIT_NAMES.iter().any(|n| command.contains(n)) && writes(command) {
        return Err(Refusal(
            "the charge and current limits are off-limits".to_owned(),
        ));
    }
    // Rule 3: `lucina-charge` with a first argument other than `status` or `report`.
    if names_word(command, "lucina-charge") {
        let after = command.split("lucina-charge").nth(1).unwrap_or("");
        let first = after.split_whitespace().next();
        if let Some(arg) = first
            && arg != "status"
            && arg != "report"
        {
            return Err(Refusal(
                "the charge and current limits are off-limits".to_owned(),
            ));
        }
    }
    // Rule 4: the token's folders.
    for p in SECRETARY {
        if command.contains(p) {
            return Err(Refusal(
                "the sign-in token's folder is off-limits".to_owned(),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Temp {
            let dir = std::env::temp_dir().join(format!(
                "offlimits-test-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Temp(dir)
        }
        fn root(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn setup() -> Temp {
        let t = Temp::new();
        for d in [
            "dev",
            "lib/firmware/qcom",
            "data/home",
            "data/lucina/secretary",
            "run/lucina/secretary",
            "sys/class/power_supply/battery",
        ] {
            std::fs::create_dir_all(t.root().join(d)).unwrap();
        }
        t
    }

    #[test]
    fn each_rule_is_refused_by_path_and_by_command() {
        let t = setup();
        let r = t.root();
        for lun in 1..=5 {
            assert!(check_path(r, Path::new(&format!("/dev/ufs0lun{lun}"))).is_err());
            assert!(check_path(r, Path::new(&format!("/dev/ufs0lun{lun}p2"))).is_err());
            assert!(check_command(&format!("dd if=/dev/zero of=/dev/ufs0lun{lun}")).is_err());
            assert!(check_command(&format!("ls ufs0lun{lun}p3")).is_err());
        }
        assert_eq!(
            check_path(r, Path::new("/lib/firmware/qcom/a.bin"))
                .unwrap_err()
                .0,
            "the Qualcomm firmware is off-limits"
        );
        assert!(check_path(r, Path::new("/vendor/firmware/x")).is_err());
        assert!(check_path(r, Path::new("/dsp/adsprpc-smd")).is_err());
        assert!(check_command("cat /lib/firmware/ath10k/firmware-5.bin").is_err());
        assert_eq!(
            check_path(
                r,
                Path::new("/sys/class/power_supply/battery/constant_charge_current")
            )
            .unwrap_err()
            .0,
            "the charge and current limits are off-limits"
        );
        assert!(
            check_path(
                r,
                Path::new("/sys/class/power_supply/battery/charge_control_limit_max")
            )
            .is_err()
        );
        assert!(
            check_command("echo 1 > /sys/class/power_supply/battery/input_current_limit").is_err()
        );
        assert!(
            check_command("tee /sys/class/power_supply/battery/charge_control_start_threshold")
                .is_err()
        );
        assert_eq!(
            check_path(r, Path::new("/data/lucina/secretary/token"))
                .unwrap_err()
                .0,
            "the sign-in token's folder is off-limits"
        );
        assert!(check_path(r, Path::new("/run/lucina/secretary")).is_err());
        assert!(check_command("cat /run/lucina/secretary/token").is_err());
    }

    #[test]
    fn allowed_paths_pass() {
        let t = setup();
        let r = t.root();
        assert!(check_path(r, Path::new("/dev/ufs0lun0p3")).is_ok());
        assert!(check_path(r, Path::new("/dev/ufs0lun0")).is_ok());
        assert!(check_path(r, Path::new("/data/home/x")).is_ok());
        assert!(check_command("ls /dev/ufs0lun0p3").is_ok());
        assert!(check_command("ls /data/home").is_ok());
    }

    #[test]
    fn a_symlink_alias_is_caught() {
        let t = setup();
        let r = t.root();
        std::os::unix::fs::symlink(r.join("lib/firmware/qcom"), r.join("data/home/fw")).unwrap();
        assert!(check_path(r, Path::new("/data/home/fw/a.bin")).is_err());
        // A not-yet-existing file under the symlinked folder: the nearest existing parent canonicalizes into the firmware.
        assert!(check_path(r, Path::new("/data/home/fw/new.bin")).is_err());
    }

    #[test]
    fn reading_the_limits_is_allowed_only_through_check_read() {
        let t = setup();
        let r = t.root();
        let f = "/sys/class/power_supply/battery/constant_charge_current";
        assert!(check_read(r, Path::new(f)).is_ok());
        assert!(check_path(r, Path::new(f)).is_err());
        assert!(check_command(&format!("cat {f}")).is_ok());
        assert!(
            check_command("echo 1 > /sys/class/power_supply/battery/input_current_limit").is_err()
        );
    }

    #[test]
    fn lucina_charge_status_is_allowed_a_change_is_not() {
        assert!(check_command("lucina-charge status").is_ok());
        assert!(check_command("lucina-charge report").is_ok());
        assert!(check_command("lucina-charge").is_ok());
        assert!(check_command("lucina-charge set 3000").is_err());
    }

    #[test]
    fn naming_the_node_in_a_listing_is_refused() {
        // Deliberately strict: `ls /dev | grep ufs0lun1` only names the node, but the command text is refused anyway.
        assert!(check_command("ls /dev | grep ufs0lun1").is_err());
    }
}
