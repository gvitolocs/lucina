//! The crash test's machinery: a disk that logs every write and flush, and a replay that stops at each write.
//!
//! A scenario runs against [`LogDisk`], which keeps the changes in memory. The log then rebuilds the disk as it would be
//! after a power cut, in two ways:
//!
//! - **In order:** every prefix of the writes.
//! - **Out of order:** a device may reorder what it holds in its cache until a flush returns, so the writes since the last
//!   flush may have landed in any combination. Each such group is tried without one write, with one write alone and
//!   with random subsets.
//!
//! A block write is assumed to be atomic: it lands whole or not at all. That is the one thing this test cannot show.
//!
//! Each state is checked by `fsck.fat -n` (only harmless findings allowed), the model of the firmware's driver
//! ([`super::edk2`]), this crate's own read-only mount, and, on a sample, mtools. What must be readable comes from the
//! [`Mark`]s that the scenario puts in the log.
#![allow(dead_code)]

use std::{
    collections::{BTreeMap, HashMap},
    fs::{File, OpenOptions},
    os::unix::fs::FileExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
};

use lucina_fat::{BlockIo, Error, Result};

use super::{Rng, edk2::Fw, ro_opts, tmp_dir, tree::Tree};

pub type Content = Arc<Vec<u8>>;

#[derive(Clone, Debug)]
pub enum Mark {
    /// From here on `path` may also read as this (`None`: may be absent).
    Allow(String, Option<Content>),
    /// A sync returned: `path` must read as this from here on.
    Promise(String, Option<Content>),
    /// From here on fsck.fat may report findings that contain this text.
    AllowFsck(&'static str),
    StopAllowFsck(&'static str),
}

#[derive(Clone, Debug)]
pub enum Op {
    Write(u64, Vec<u8>),
    Flush,
    Mark(Mark),
}

/// Fails the device from the given write or flush on, as a dying disk or a driver error would.
#[derive(Clone, Copy, Debug, Default)]
pub struct Fault {
    pub after_ops: Option<usize>,
}

/// A disk in memory over a base image, with a log of what was asked of it.
pub struct LogDisk {
    base: File,
    size: u64,
    overlay: HashMap<u64, Vec<u8>>,
    pub log: Arc<Mutex<Vec<Op>>>,
    pub fault: Fault,
    device_ops: usize,
}

impl LogDisk {
    pub fn open(base: &Path, log: Arc<Mutex<Vec<Op>>>) -> LogDisk {
        let f = File::open(base).unwrap();
        let size = f.metadata().unwrap().len();
        LogDisk {
            base: f,
            size,
            overlay: HashMap::new(),
            log,
            fault: Fault::default(),
            device_ops: 0,
        }
    }

    fn next_op_fails(&mut self) -> bool {
        self.device_ops += 1;
        self.fault.after_ops.is_some_and(|n| self.device_ops > n)
    }
}

impl BlockIo for LogDisk {
    fn size(&self) -> u64 {
        self.size
    }

    fn read(&mut self, offset: u64, buf: &mut [u8]) -> Result<()> {
        assert_eq!(offset % 4096, 0, "unaligned device read");
        assert!(buf.len() <= 4096);
        if let Some(b) = self.overlay.get(&offset) {
            buf.copy_from_slice(&b[..buf.len()]);
            return Ok(());
        }
        self.base.read_exact_at(buf, offset).map_err(|_| Error::Io)
    }

    fn write(&mut self, offset: u64, buf: &[u8]) -> Result<()> {
        assert_eq!(offset % 4096, 0, "unaligned device write");
        assert!(buf.len() <= 4096);
        if self.next_op_fails() {
            return Err(Error::Io);
        }
        self.overlay.insert(offset, buf.to_vec());
        self.log
            .lock()
            .unwrap()
            .push(Op::Write(offset, buf.to_vec()));
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        if self.next_op_fails() {
            return Err(Error::Io);
        }
        self.log.lock().unwrap().push(Op::Flush);
        Ok(())
    }
}

/// How thoroughly a log is replayed.
#[derive(Clone, Copy)]
pub struct Replay {
    /// Random subsets per group of unflushed writes.
    pub subsets: usize,
    /// Whether groups up to this many writes are also tried with each write alone and each write missing.
    pub exhaustive_up_to: usize,
    /// Run mtools on metadata states; 0 turns it off.
    pub mtools_samples: usize,
}

impl Default for Replay {
    fn default() -> Self {
        Replay {
            subsets: 8,
            exhaustive_up_to: 24,
            mtools_samples: 30,
        }
    }
}

#[derive(Default, Clone)]
struct Expect {
    allowed: BTreeMap<String, Vec<Option<Content>>>,
    fsck_extra: Vec<&'static str>,
}

impl Expect {
    fn apply(&mut self, m: &Mark) {
        match m {
            Mark::Allow(p, c) => self.allowed.entry(p.clone()).or_default().push(c.clone()),
            Mark::Promise(p, c) => {
                self.allowed.insert(p.clone(), vec![c.clone()]);
            }
            Mark::AllowFsck(s) => self.fsck_extra.push(s),
            Mark::StopAllowFsck(s) => self.fsck_extra.retain(|x| x != s),
        }
    }
}

/// Lines of `fsck.fat -n` that say nothing is wrong beyond lost clusters and hints.
const HARMLESS: &[&str] = &[
    "fsck.fat ",
    "Dirty bit is set",
    "Automatically removing dirty bit",
    "There are differences between boot sector and its backup",
    "This is mostly harmless",
    "Not automatically fixing",
    "FATs differ but appear to be intact",
    "Using first FAT",
    "Reclaimed ",
    "Free cluster summary",
    "Auto-correcting",
    "Leaving filesystem unchanged",
    "Performing changes",
];

fn fsck_verdict(img: &Path, extra: &[&'static str]) -> std::result::Result<(), String> {
    let out = Command::new("fsck.fat")
        .arg("-n")
        .arg(img)
        .stdin(Stdio::null())
        .output()
        .expect("fsck.fat is installed");
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    let summary = format!("{}: ", img.display());
    let bad: Vec<&str> = text
        .lines()
        .filter(|l| {
            let l2 = l.trim();
            !l2.is_empty()
                && !l.starts_with(&summary)
                && !HARMLESS.iter().any(|h| l2.starts_with(h))
                // The list under "Differences: (offset:original/backup)": only the dirty byte may differ.
                && !is_backup_diff_of_state_byte(l2)
                && !extra.iter().any(|h| l2.contains(h))
        })
        .collect();
    if bad.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "fsck.fat: {}\n--- whole output:\n{text}",
            bad.join(" | ")
        ))
    }
}

fn is_backup_diff_of_state_byte(l: &str) -> bool {
    let t = l.trim();
    t.starts_with("65:") && t.len() <= 9 && t.contains('/')
}

fn check_contents(img: &Path, want: &Expect) -> std::result::Result<(), String> {
    let fw = Fw::open(img).map_err(|e| format!("firmware refuses the volume: {e}"))?;
    fw.check_free_hint()?;
    let mut t = Tree::new(
        lucina_fat::Volume::mount(super::FileDisk::open(img), ro_opts())
            .map_err(|e| format!("own mount: {e:?}"))?,
    );
    for (path, allowed) in &want.allowed {
        let got = fw
            .read_file(path)
            .map_err(|e| format!("firmware reading {path}: {e}"))?;
        if !is_allowed(&got, allowed) {
            return Err(format!(
                "firmware reads {path} as {} which is none of {:?}",
                describe(&got),
                allowed.iter().map(describe_c).collect::<Vec<_>>()
            ));
        }
        let own = match t.read_file(path) {
            Ok(d) => Some(d),
            Err(Error::NotFound) => None,
            Err(e) => return Err(format!("own mount reading {path}: {e:?}")),
        };
        if !is_allowed(&own, allowed) {
            return Err(format!(
                "own mount reads {path} as {} which is none of {:?}",
                describe(&own),
                allowed.iter().map(describe_c).collect::<Vec<_>>()
            ));
        }
    }
    Ok(())
}

fn is_allowed(got: &Option<Vec<u8>>, allowed: &[Option<Content>]) -> bool {
    allowed.iter().any(|a| match (a, got) {
        (None, None) => true,
        (Some(a), Some(g)) => a.as_slice() == g.as_slice(),
        _ => false,
    })
}

fn describe(c: &Option<Vec<u8>>) -> String {
    c.as_ref()
        .map_or("absent".into(), |d| format!("{} bytes", d.len()))
}

fn describe_c(c: &Option<Content>) -> String {
    c.as_ref()
        .map_or("absent".into(), |d| format!("{} bytes", d.len()))
}

/// A long-lived mtools container, so that each check costs an `exec`, not a `run`.
struct MtoolsBox {
    name: String,
}

impl MtoolsBox {
    fn start(dir: &Path, name: &str) -> Option<MtoolsBox> {
        if !super::mtools_available() {
            return None;
        }
        let name = format!("lucina-fat-crash-{}-{name}", std::process::id());
        let ok = Command::new("docker")
            .args(["run", "-d", "--rm", "--network=none", "--name", &name, "-v"])
            .arg(format!("{}:/w", dir.display()))
            .args(["lucinaos-grub:trixie", "sleep", "3600"])
            .stdout(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        ok.then_some(MtoolsBox { name })
    }

    fn check(&self, file: &str, want: &Expect) -> std::result::Result<(), String> {
        for (path, allowed) in &want.allowed {
            let script = format!(
                "cd /w; MTOOLS_SKIP_CHECK=1 LC_ALL=C.UTF-8 mtype -i {file} '::{path}'; echo \"rc=$?\" >&2"
            );
            let out = Command::new("docker")
                .args(["exec", &self.name, "sh", "-c", &script])
                .output()
                .map_err(|e| e.to_string())?;
            let err = String::from_utf8_lossy(&out.stderr);
            let got = if err.contains("rc=0") {
                Some(out.stdout)
            } else {
                None
            };
            if !is_allowed(&got, allowed) {
                return Err(format!(
                    "mtools reads {path} as {} ({}) which is none of {:?}",
                    describe(&got),
                    err.trim(),
                    allowed.iter().map(describe_c).collect::<Vec<_>>()
                ));
            }
        }
        Ok(())
    }
}

impl Drop for MtoolsBox {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args(["rm", "-f", &self.name])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

#[derive(Default, Debug)]
pub struct Stats {
    pub writes: usize,
    pub flushes: usize,
    pub states: usize,
    pub mtools_states: usize,
}

/// Replays `log` on a copy of `base`, checking every state. Panics with the first failures.
pub fn replay(name: &str, base: &Path, log: &[Op], initial: &[Mark], how: Replay) -> Stats {
    let dir = tmp_dir().join("crash");
    std::fs::create_dir_all(&dir).unwrap();
    let file_name = format!("{name}.work.img");
    let work_path: PathBuf = dir.join(&file_name);
    assert!(
        Command::new("cp")
            .args(["--sparse=always", "--reflink=auto"])
            .arg(base)
            .arg(&work_path)
            .status()
            .unwrap()
            .success()
    );
    let work = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&work_path)
        .unwrap();
    let mtools = if how.mtools_samples > 0 {
        MtoolsBox::start(&dir, name)
    } else {
        None
    };
    let mut expect = Expect::default();
    for m in initial {
        expect.apply(m);
    }
    let mut stats = Stats {
        writes: log.iter().filter(|o| matches!(o, Op::Write(..))).count(),
        flushes: log.iter().filter(|o| matches!(o, Op::Flush)).count(),
        ..Stats::default()
    };
    let mut failures: Vec<String> = Vec::new();
    let mut rng = Rng(0x5eed ^ (name.len() as u64 * 0x1000_0001));
    // Which writes touch metadata: mtools is too slow for every state, so it samples those.
    let metadata_writes = log
        .iter()
        .filter(|o| matches!(o, Op::Write(off, _) if is_metadata(*off, base)))
        .count()
        .max(1);
    let mtools_every = (metadata_writes / how.mtools_samples.max(1)).max(1);
    let mut metadata_seen = 0usize;

    // The unflushed group: (offset, what the block held before the group, the write).
    let mut group: Vec<(u64, Vec<u8>, Vec<u8>)> = Vec::new();

    let check = |expect: &Expect,
                 what: String,
                 stats: &mut Stats,
                 with_mtools: bool,
                 failures: &mut Vec<String>| {
        stats.states += 1;
        let r = fsck_verdict(&work_path, &expect.fsck_extra)
            .and_then(|()| check_contents(&work_path, expect))
            .and_then(|()| match (&mtools, with_mtools) {
                (Some(m), true) => {
                    stats.mtools_states += 1;
                    m.check(&file_name, expect)
                }
                _ => Ok(()),
            });
        if let Err(e) = r {
            failures.push(format!("{what}: {e}"));
        }
    };

    check(
        &expect,
        "before anything".into(),
        &mut stats,
        mtools.is_some(),
        &mut failures,
    );
    for (i, op) in log.iter().enumerate() {
        if failures.len() >= 5 {
            break;
        }
        match op {
            Op::Mark(m) => expect.apply(m),
            Op::Write(off, data) => {
                let mut before = vec![0u8; data.len()];
                work.read_exact_at(&mut before, *off).unwrap();
                work.write_all_at(data, *off).unwrap();
                group.push((*off, before, data.clone()));
                let meta = is_metadata(*off, base);
                let mut with_mtools = false;
                if meta {
                    metadata_seen += 1;
                    with_mtools = metadata_seen.is_multiple_of(mtools_every);
                }
                check(
                    &expect,
                    format!("cut after op {i} (write at {off:#x})"),
                    &mut stats,
                    with_mtools,
                    &mut failures,
                );
            }
            Op::Flush => {
                try_reorderings(&work, &mut group, how, &mut rng, |subset_name| {
                    check(
                        &expect,
                        format!("before flush op {i}, {subset_name}"),
                        &mut stats,
                        false,
                        &mut failures,
                    );
                });
                group.clear();
            }
        }
    }
    if failures.len() < 5 {
        try_reorderings(&work, &mut group, how, &mut rng, |subset_name| {
            check(
                &expect,
                format!("at the end of the log, {subset_name}"),
                &mut stats,
                false,
                &mut failures,
            );
        });
    }
    drop(mtools);
    let _ = std::fs::remove_file(&work_path);
    assert!(
        failures.is_empty(),
        "{name}: {} failing state(s) of {} ({} writes, {} flushes):\n{}",
        failures.len(),
        stats.states,
        stats.writes,
        stats.flushes,
        failures.join("\n\n")
    );
    stats
}

/// Whether a device offset is outside the data area of the volume in `base`.
fn is_metadata(off: u64, base: &Path) -> bool {
    use std::cell::RefCell;
    thread_local! {
        static DATA_START: RefCell<HashMap<PathBuf, u64>> = RefCell::new(HashMap::new());
    }
    DATA_START.with(|m| {
        let mut m = m.borrow_mut();
        let start = *m.entry(base.to_path_buf()).or_insert_with(|| {
            let f = File::open(base).unwrap();
            let mut b = [0u8; 4096];
            f.read_exact_at(&mut b, 0).unwrap();
            let size = f.metadata().unwrap().len();
            lucina_fat::Geometry::parse(&b, size).unwrap().data_start
        });
        off < start
    })
}

/// Tries the group's writes in other combinations, leaving the file with all of them applied.
fn try_reorderings(
    work: &File,
    group: &mut [(u64, Vec<u8>, Vec<u8>)],
    how: Replay,
    rng: &mut Rng,
    mut check: impl FnMut(String),
) {
    let n = group.len();
    if n < 2 {
        return;
    }
    let apply = |keep: &dyn Fn(usize) -> bool| {
        // Undo in reverse, then redo the kept ones in order, so that a block written twice stays right.
        for (off, before, _) in group.iter().rev() {
            work.write_all_at(before, *off).unwrap();
        }
        for (k, (off, _, data)) in group.iter().enumerate() {
            if keep(k) {
                work.write_all_at(data, *off).unwrap();
            }
        }
    };
    if n <= how.exhaustive_up_to {
        for miss in 0..n {
            apply(&|k| k != miss);
            check(format!("group of {n} without write {miss}"));
        }
        for only in 0..n {
            apply(&|k| k == only);
            check(format!("group of {n} with only write {only}"));
        }
    }
    for s in 0..how.subsets {
        let seed = rng.next();
        apply(&|k| {
            (seed
                .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                .wrapping_add((k as u64).wrapping_mul(0xD6E8_FEB8_6659_FD93)))
            .wrapping_mul(0xFF51_AFD7_ED55_8CCD)
                >> 63
                == 1
        });
        check(format!("group of {n}, random subset {s}"));
    }
    apply(&|_| true);
}
