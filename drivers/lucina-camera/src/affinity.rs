//! Running a thread on the fastest CPUs. The laptop (Snapdragon 7c, SC7180) has six Cortex-A55 "Silver" and two Cortex-A76
//! "Gold" CPUs, and its kernel numbers them in the order they started, not by type, so the Gold ones are found at run time from
//! `cpu_capacity` (Gold 1024, Silver 293, as Linux works it out). The camera preview's JPEG decoder runs there.

use std::io;
use std::path::Path;

/// The online CPUs with the highest `cpu_capacity`, read from `root` (normally `/sys/devices/system/cpu`), in ascending order.
/// Empty if `online` cannot be read or no CPU has a readable capacity; a CPU whose capacity file is missing or unparsable is skipped.
pub fn fastest_cpus_in(root: &Path) -> Vec<usize> {
    let Some(online) = std::fs::read_to_string(root.join("online"))
        .ok()
        .and_then(|s| parse_cpu_list(&s))
    else {
        return Vec::new();
    };
    let caps: Vec<(usize, u64)> = online
        .into_iter()
        .filter_map(|cpu| {
            let text = std::fs::read_to_string(root.join(format!("cpu{cpu}/cpu_capacity"))).ok()?;
            Some((cpu, text.trim().parse().ok()?))
        })
        .collect();
    let Some(best) = caps.iter().map(|&(_, c)| c).max() else {
        return Vec::new();
    };
    caps.into_iter()
        .filter(|&(_, c)| c == best)
        .map(|(cpu, _)| cpu)
        .collect()
}

/// [`fastest_cpus_in`] `/sys/devices/system/cpu`.
pub fn fastest_cpus() -> Vec<usize> {
    fastest_cpus_in(Path::new("/sys/devices/system/cpu"))
}

/// A Linux CPU list such as `0-3,5,7-8` (surrounding whitespace allowed) as ascending unique numbers. An empty list is
/// `Some(vec![])`; anything else that is not such a list is `None`.
pub fn parse_cpu_list(s: &str) -> Option<Vec<usize>> {
    let s = s.trim();
    let mut cpus = Vec::new();
    if s.is_empty() {
        return Some(cpus);
    }
    for part in s.split(',') {
        let part = part.trim();
        let (a, b) = match part.split_once('-') {
            Some((a, b)) => (
                a.trim().parse::<usize>().ok()?,
                b.trim().parse::<usize>().ok()?,
            ),
            None => {
                let n = part.parse::<usize>().ok()?;
                (n, n)
            }
        };
        if a > b || b >= libc::CPU_SETSIZE as usize {
            return None;
        }
        cpus.extend(a..=b);
    }
    cpus.sort_unstable();
    cpus.dedup();
    Some(cpus)
}

/// Restricts the calling thread to `cpus`. Nothing happens for an empty list; a CPU number past `CPU_SETSIZE` is `InvalidInput`.
pub fn pin_current_thread(cpus: &[usize]) -> io::Result<()> {
    if cpus.is_empty() {
        return Ok(());
    }
    if cpus.iter().any(|&c| c >= libc::CPU_SETSIZE as usize) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a CPU number past CPU_SETSIZE",
        ));
    }
    // SAFETY: `cpu_set_t` is plain bits; all zeroes is the empty set.
    let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    for &c in cpus {
        // SAFETY: `c` is below `CPU_SETSIZE`, checked above, so the bit is inside `set`.
        unsafe { libc::CPU_SET(c, &mut set) };
    }
    // SAFETY: pid 0 is the calling thread; `set` is a valid `cpu_set_t` of the size passed.
    if unsafe { libc::sched_setaffinity(0, size_of::<libc::cpu_set_t>(), &set) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The CPU the calling thread is on now.
pub fn current_cpu() -> Option<usize> {
    // SAFETY: `sched_getcpu` takes nothing and only returns a number.
    let c = unsafe { libc::sched_getcpu() };
    usize::try_from(c).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_lists() {
        assert_eq!(parse_cpu_list("0-7\n"), Some((0..=7).collect()));
        assert_eq!(parse_cpu_list("0-3,5,7-8"), Some(vec![0, 1, 2, 3, 5, 7, 8]));
        assert_eq!(parse_cpu_list("3,1,1"), Some(vec![1, 3]));
        assert_eq!(parse_cpu_list(""), Some(vec![]));
        assert_eq!(parse_cpu_list("a-b"), None);
        assert_eq!(parse_cpu_list("5-2"), None);
        assert_eq!(parse_cpu_list("1,,2x"), None);
        assert_eq!(parse_cpu_list("0-100000"), None);
    }

    #[test]
    fn the_gold_cores_are_the_fastest() {
        let root = std::env::temp_dir().join(format!("lucina-affinity-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("online"), "0-8\n").unwrap();
        for cpu in 0..8 {
            let dir = root.join(format!("cpu{cpu}"));
            std::fs::create_dir_all(&dir).unwrap();
            let cap = if cpu == 1 || cpu == 2 {
                "1024\n"
            } else {
                "293\n"
            };
            std::fs::write(dir.join("cpu_capacity"), cap).unwrap();
        }
        // cpu8 is online but says nothing about its capacity: skipped.
        assert_eq!(fastest_cpus_in(&root), vec![1, 2]);
        std::fs::remove_file(root.join("online")).unwrap();
        assert!(fastest_cpus_in(&root).is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn pinning() {
        assert!(pin_current_thread(&[]).is_ok());
        assert_eq!(
            pin_current_thread(&[100_000]).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        // In a thread of its own, so that the test runner's thread keeps every CPU.
        std::thread::spawn(|| {
            let here = current_cpu().expect("sched_getcpu");
            pin_current_thread(&[here]).unwrap();
            assert_eq!(current_cpu(), Some(here));
        })
        .join()
        .unwrap();
    }
}
