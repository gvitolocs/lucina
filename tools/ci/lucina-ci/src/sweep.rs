//! Keeps the fast-check cache under a size limit (docs/ci/SHIFT-LEFT.md). The cache root (`~/.cache/lucina-check-target`) holds one Cargo
//! target per worktree (`wt-<hash>/`): Cargo gives a workspace member the same artifact hash in every worktree, so a target shared by
//! worktrees on different commits let one overwrite the others' rlibs and test binaries (seen twice on 2026-11-11, a false pass
//! waiting to happen).
//! Over the limit, the least recently used worktree targets go whole, oldest first, down to a lower mark. Old layouts (a shared
//! `debug/` and `release/` straight under the root) count as never used and go first — but **only once past the in-use test**: a
//! sweep never removes anything from a target a running cargo holds (coordinator allow 2026-11-18, finding GATE-09). A target is
//! *in use*, and skipped, when a cargo build lock is held in it or one of its `debug/`/`release/` folders, **or** when one of the
//! target's top-level entries or one of the entries of a `deps/` folder (the target's, or a `debug`/`release` profile's) was written
//! in the last `IN_USE_WINDOW_SECS` (10 minutes) — nothing deeper is read —, **or** when a process has its executable, its working
//! directory, an open file or a memory mapping inside it, however long ago anything was written: cargo drops the `.cargo-lock`
//! between compiling and running the test binaries, and a sweep that fired in that window stripped `<cache_root>/debug` whole from
//! a live `cargo test` whose binaries it had not yet reached. A target whose top level cannot be read is skipped too (unreadable),
//! never removed on a guess. The locks are only probed and are released at once; what closes the race with a build starting now is
//! the removal itself: the target is renamed to `.sweep-trash-<pid>-<name>` beside it in one step and only that renamed tree is
//! deleted, so a cargo that starts afterwards makes a fresh folder at the old name and never meets the deletion. A `.sweep-trash-*`
//! left by a crashed sweep goes on the next run. The rename and the delete are done relative to a descriptor of the cache root that
//! is opened (O_DIRECTORY|O_NOFOLLOW) right after the `--dir` check — renameat, openat with O_NOFOLLOW, unlinkat — so a symlink
//! swapped into the path afterwards is never followed, and the delete never enters a directory whose `st_dev` is not the root's (it
//! leaves that subtree and says `contains a mount point`). Both modes work only under the canonical `$HOME/.cache` the CLI passes;
//! the process table is a parameter (`/proc` in the CLI), never an environment variable. The in-use check sees only processes of our own
//! uid (one of ours whose `/proc` entries cannot be read counts only when its command line names a path in the target; a process of
//! another uid is never counted): the cache is per-user, so a build by another uid into it is not protected. `--single` reads the
//! target (profiles, `deps/`, `incremental/`, `build/`, sizes) and takes its `.cargo-lock` files only through the root descriptor,
//! with O_NOFOLLOW, and skips a profile on another filesystem. Every skip, every removal and every failed removal prints one line on stderr, so
//! `check-fast`'s output and the merge job log name the folder, its size and its age. A removed target costs a rebuild, never
//! correctness.
//! `--single` caps ONE Cargo target dir instead of a root of them (the app and landing caches under `~/.cache/cargo-target`,
//! docs/ci/SHIFT-LEFT.md "Other cargo targets"): over the limit the least recently used build artifacts of that one target go, oldest
//! first, down to the lower mark, and nothing else - never the binaries in the profile folder. Here a build running in the target means
//! the whole sweep is skipped, because half a target cleaned under a live build is worse than a dirty one.

use std::ffi::{CStr, CString, OsStr, OsString};
use std::fs;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    pub bytes: u64,
    /// Seconds since the epoch of the last use (0: an old layout, never used by the per-worktree check).
    pub used: u64,
}

/// Which entries to remove, oldest first, so that `total - removed <= to_bytes`. Nothing when the total is already within `limit_bytes`.
pub fn plan(entries: &[Entry], total: u64, limit_bytes: u64, to_bytes: u64) -> Vec<usize> {
    if total <= limit_bytes {
        return Vec::new();
    }
    let mut order: Vec<usize> = (0..entries.len()).collect();
    order.sort_by_key(|&i| (entries[i].used, entries[i].path.clone()));
    let mut left = total;
    let mut out = Vec::new();
    for i in order {
        if left <= to_bytes {
            break;
        }
        left = left.saturating_sub(entries[i].bytes);
        out.push(i);
    }
    out
}

fn secs(t: SystemTime) -> u64 {
    t.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn tree_bytes(p: &Path) -> u64 {
    let Ok(md) = fs::symlink_metadata(p) else {
        return 0;
    };
    if md.is_dir() {
        fs::read_dir(p)
            .map(|rd| rd.flatten().map(|e| tree_bytes(&e.path())).sum())
            .unwrap_or(0)
    } else {
        md.len()
    }
}

/// The cache entries that may go: each folder under the root except `keep` (the target in use now).
pub fn candidates(root: &Path, keep: Option<&Path>) -> Vec<Entry> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(root) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        if !p.is_dir()
            || keep.is_some_and(|k| k == p)
            || e.file_name().to_string_lossy().starts_with(TRASH_PREFIX)
        {
            continue;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        let used = if name.starts_with("wt-") {
            fs::metadata(p.join(".last-used"))
                .or_else(|_| fs::metadata(&p))
                .and_then(|m| m.modified())
                .map_or(0, secs)
        } else {
            0
        };
        out.push(Entry {
            bytes: tree_bytes(&p),
            used,
            path: p,
        });
    }
    out
}

/// Holds the cargo locks of a target (`.cargo-lock` in it, or in its `debug/` and `release/`), without waiting. `None`: a build holds one.
fn lock_target(dir: &Path) -> Option<Vec<fs::File>> {
    let mut held = Vec::new();
    for sub in ["", "debug", "release"] {
        let lock = dir.join(sub).join(".cargo-lock");
        if !lock.exists() {
            continue;
        }
        let f = fs::OpenOptions::new().write(true).open(&lock).ok()?;
        f.try_lock().ok()?;
        held.push(f);
    }
    Some(held)
}

/// A target with anything written under it in this many seconds is left alone even when no build holds a lock: cargo drops the
/// `.cargo-lock` between compiling and running the test binaries, and a sweep in that window is what GATE-09 lost a live
/// `cargo test` to. Ten minutes — a rebuild of what genuinely went idle costs minutes, a wrong removal costs a whole gate. Past the
/// window a process that still has the target open is found by `pid_using`, not by the clock.
const IN_USE_WINDOW_SECS: u64 = 600;

/// The newest write (mtime) under one candidate target, read **shallowly**: the mtimes of the target's top-level entries and of the
/// `deps/` folders where Cargo and a test run write — the target itself, its `deps/`, and its `debug`/`release` profiles and their
/// `deps/`. Nothing deeper is read. A symlink's own mtime says nothing about what it points at, so symlinks are never followed.
/// `Ok(None)`: readable, and nothing in it. `Err`: the target's own folder, or a probe folder that exists, cannot be read — the
/// caller must not guess, so an unreadable target is skipped, never removed. Never a full walk: a handful of `read_dir` calls.
fn newest_write(dir: &Path) -> Result<Option<SystemTime>, ()> {
    let mut out: Option<SystemTime> = None;
    for (i, probe) in [
        dir.to_path_buf(),
        dir.join("deps"),
        dir.join("debug"),
        dir.join("debug").join("deps"),
        dir.join("release"),
        dir.join("release").join("deps"),
    ]
    .into_iter()
    .enumerate()
    {
        let rd = match fs::read_dir(&probe) {
            Ok(rd) => rd,
            // The folders below the target are optional; the target itself is not.
            Err(e)
                if i > 0
                    && matches!(
                        e.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                    ) =>
            {
                continue;
            }
            Err(_) => return Err(()),
        };
        for e in rd.flatten() {
            let Ok(md) = fs::symlink_metadata(e.path()) else {
                continue;
            };
            if md.file_type().is_symlink() {
                continue;
            }
            if let Ok(m) = md.modified()
                && out.is_none_or(|n| m > n)
            {
                out = Some(m);
            }
        }
    }
    Ok(out)
}

/// Seconds from the newest write to `now` (`0` when a write is timestamped in the future, which is still "just now"). `Ok(None)`:
/// nothing in it. `Err`: unreadable, see `newest_write`.
fn written_ago(dir: &Path, now: SystemTime) -> Result<Option<u64>, ()> {
    newest_write(dir).map(|t| t.map(|t| now.duration_since(t).map_or(0, |d| d.as_secs())))
}

/// One thing a process has, as `pid_using` looks at it.
enum Probe {
    /// It lies inside the target.
    Hit,
    /// It could not be read for want of permission.
    Denied,
    Clear,
}

/// Where a /proc link (`exe`, `cwd`, an `fd/` entry) points, against the target. A process that exits while we look is `Clear`.
fn probe_link(link: &Path, real: &Path) -> Probe {
    match fs::read_link(link) {
        Ok(t) if t.starts_with(real) => Probe::Hit,
        Ok(_) => Probe::Clear,
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => Probe::Denied,
        Err(_) => Probe::Clear,
    }
}

/// The path column of one `/proc/<pid>/maps` line (`addr perms offset dev inode   path`), without a ` (deleted)` mark; `""` for an
/// anonymous mapping.
fn maps_path(line: &str) -> &str {
    let mut rest = line;
    for _ in 0..5 {
        rest = rest.trim_start();
        rest = rest.find(char::is_whitespace).map_or("", |i| &rest[i..]);
    }
    let p = rest.trim();
    p.strip_suffix(" (deleted)").unwrap_or(p)
}

/// Does `arg` name a path inside `real`: the whole argument, or what follows an `=` in it (`--target-dir=/cache/wt-x/...`)?
fn arg_names(arg: &str, real: &Path) -> bool {
    Path::new(arg).starts_with(real)
        || arg
            .match_indices('=')
            .any(|(i, _)| Path::new(&arg[i + 1..]).starts_with(real))
}

/// Does an argument of this `cmdline` (NUL-separated) name a path inside `real` (see `arg_names`)?
fn cmdline_names(cmdline: &Path, real: &Path) -> bool {
    fs::read(cmdline).is_ok_and(|c| {
        c.split(|b| *b == 0)
            .any(|a| !a.is_empty() && arg_names(&String::from_utf8_lossy(a), real))
    })
}

/// The pid of a process that has its executable, its working directory, any open file or any file mapping inside `dir` (the check
/// that does not depend on a clock: a test binary that has run for an hour is still found). Our own pid is not counted (we hold
/// nothing of the target). An entry of another user's process that cannot be read is not "in use"; one of our own uid that cannot
/// be read (there are always some: `systemd --user`) counts only when its command line names a path in the target. `procs` is `/proc`, or a test's own table.
fn pid_using(dir: &Path, procs: &Path) -> Option<u32> {
    let real = fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let me = std::process::id();
    // SAFETY: geteuid has no preconditions and cannot fail.
    let uid = unsafe { libc::geteuid() };
    for p in children(procs) {
        let Some(pid) = p
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == me {
            continue;
        }
        let mut denied = false;
        let mut note = |pr: Probe| match pr {
            Probe::Hit => true,
            Probe::Denied => {
                denied = true;
                false
            }
            Probe::Clear => false,
        };
        if note(probe_link(&p.join("exe"), &real)) || note(probe_link(&p.join("cwd"), &real)) {
            return Some(pid);
        }
        match fs::read_dir(p.join("fd")) {
            Ok(rd) => {
                for e in rd.flatten() {
                    if note(probe_link(&e.path(), &real)) {
                        return Some(pid);
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => denied = true,
            Err(_) => {}
        }
        match fs::read_to_string(p.join("maps")) {
            Ok(maps) => {
                if maps.lines().any(|l| {
                    maps_path(l).starts_with('/') && Path::new(maps_path(l)).starts_with(&real)
                }) {
                    return Some(pid);
                }
            }
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => denied = true,
            Err(_) => {}
        }
        // A process of ours that cannot be read (dumpable=0: `systemd --user`, `sshd-session`, ...) is only ever one of those: it counts
        // as in use when its command line, which anyone can read, names a path inside the target, and not otherwise — counting them all
        // would make every sweep on a host with a login session skip everything.
        if denied
            && fs::metadata(&p).is_ok_and(|m| m.uid() == uid)
            && cmdline_names(&p.join("cmdline"), &real)
        {
            return Some(pid);
        }
    }
    None
}

/// What became of one doomed candidate target.
#[derive(Debug)]
enum Sweep1 {
    /// A cargo build lock is held in it or a profile folder: skipped (lock held).
    Locked,
    /// Something at its top level or in a `deps/` was written `N` seconds ago, inside the in-use window: skipped (written N s ago).
    Recent(u64),
    /// A process has it open (executable, cwd, file or mapping): skipped (in use by pid N).
    InUse(u32),
    /// Its top level could not be read: skipped (unreadable).
    Unreadable,
    /// It is itself a mount point (another filesystem than the root): skipped (mount point).
    MountPoint,
    /// A folder inside it is on another filesystem: skipped whole (contains a mount point), never removed in part.
    ContainsMount,
    /// Removed (a dry run would remove): `bytes` freed and the age of its newest write, for the log.
    Removed { bytes: u64, age: u64 },
    /// The removal failed (possibly part-way): the message, logged as `failed to remove`.
    Failed(String),
}

/// The prefix of a folder a sweep renamed a target to and was deleting; one left behind by a crashed sweep goes on the next run.
const TRASH_PREFIX: &str = ".sweep-trash-";

// ---- removal through a directory fd -------------------------------------------------------------------------------------------
//
// The cache root is opened once, after `check_root`, with O_DIRECTORY|O_NOFOLLOW, and every rename and delete below is relative to
// that fd (renameat, openat with O_NOFOLLOW|O_DIRECTORY, unlinkat). A path string is resolved again at each call, so a symlink swapped
// into it after the check is followed; an fd names the directory that was checked, and what is below it is never reached through a
// link.

/// The canonical cache root, held open. `dev` is its `st_dev`: nothing below is entered if it is on another filesystem.
struct Root {
    fd: OwnedFd,
    path: PathBuf,
    dev: u64,
    /// A test's stand-in for a mount point (a name that counts as foreign), since a unit test cannot mount. Not in a release build.
    #[cfg(test)]
    fake_mount: Option<OsString>,
}

fn cstr(name: &OsStr) -> io::Result<CString> {
    CString::new(name.as_bytes()).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

fn is_dir_mode(st: &libc::stat) -> bool {
    st.st_mode & libc::S_IFMT == libc::S_IFDIR
}

fn is_link_mode(st: &libc::stat) -> bool {
    st.st_mode & libc::S_IFMT == libc::S_IFLNK
}

fn check_fd(fd: libc::c_int) -> io::Result<OwnedFd> {
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: `fd` is a fresh descriptor that nothing else owns.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}

const OPEN_DIR: libc::c_int =
    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;

fn open_dir_path(path: &Path) -> io::Result<OwnedFd> {
    let c = cstr(path.as_os_str())?;
    // SAFETY: `c` is a valid NUL-terminated path.
    check_fd(unsafe { libc::open(c.as_ptr(), OPEN_DIR) })
}

fn open_dir_at(parent: RawFd, name: &OsStr) -> io::Result<OwnedFd> {
    let c = cstr(name)?;
    // SAFETY: `c` is a valid NUL-terminated name; `parent` is a directory fd that outlives the call.
    check_fd(unsafe { libc::openat(parent, c.as_ptr(), OPEN_DIR) })
}

fn stat_at(parent: RawFd, name: &OsStr) -> io::Result<libc::stat> {
    let c = cstr(name)?;
    // SAFETY: an all-zero `stat` is a valid value for fstatat to overwrite.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: valid name, valid out pointer, live parent fd. AT_SYMLINK_NOFOLLOW: a link is stat'ed, never followed.
    let r = unsafe { libc::fstatat(parent, c.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) };
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(st)
    }
}

fn fstat_fd(fd: RawFd) -> io::Result<libc::stat> {
    // SAFETY: an all-zero `stat` is a valid value for fstat to overwrite.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: valid out pointer and a live fd.
    if unsafe { libc::fstat(fd, &mut st) } < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(st)
    }
}

fn unlink_at(parent: RawFd, name: &OsStr, flags: libc::c_int) -> io::Result<()> {
    let c = cstr(name)?;
    // SAFETY: valid name and a live parent fd.
    if unsafe { libc::unlinkat(parent, c.as_ptr(), flags) } < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn rename_at(dir: RawFd, from: &OsStr, to: &OsStr) -> io::Result<()> {
    let (f, t) = (cstr(from)?, cstr(to)?);
    // SAFETY: valid names and a live directory fd for both sides.
    if unsafe { libc::renameat(dir, f.as_ptr(), dir, t.as_ptr()) } < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// The names in the directory `fd` (without `.` and `..`). Reads through a duplicate, so `fd` stays usable.
fn dir_names(fd: RawFd) -> io::Result<Vec<OsString>> {
    // SAFETY: `fd` is a live descriptor; the duplicate is ours.
    let d = unsafe { libc::dup(fd) };
    if d < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `d` is a directory descriptor we own; on success fdopendir owns it until closedir.
    let dirp = unsafe { libc::fdopendir(d) };
    if dirp.is_null() {
        let e = io::Error::last_os_error();
        // SAFETY: fdopendir failed, so `d` is still ours to close.
        unsafe { libc::close(d) };
        return Err(e);
    }
    // SAFETY: `dirp` is a live DIR*; the duplicate shares its offset with `fd`, so start from the top.
    unsafe { libc::rewinddir(dirp) };
    let mut out = Vec::new();
    loop {
        // SAFETY: `dirp` is a live DIR*.
        let ent = unsafe { libc::readdir(dirp) };
        if ent.is_null() {
            break;
        }
        // SAFETY: readdir returned a valid entry whose d_name is NUL-terminated; it is copied before the next call.
        let name = unsafe { CStr::from_ptr((*ent).d_name.as_ptr()) }.to_bytes();
        if name != b"." && name != b".." {
            out.push(OsString::from_vec(name.to_vec()));
        }
    }
    // SAFETY: closes the DIR* and the duplicate.
    unsafe { libc::closedir(dirp) };
    Ok(out)
}

impl Root {
    /// Opens `path` (already canonical) without following a link in its last component.
    fn open(path: &Path) -> io::Result<Root> {
        let fd = open_dir_path(path)?;
        let dev = fstat_fd(fd.as_raw_fd())?.st_dev as u64;
        Ok(Root {
            fd,
            path: path.to_path_buf(),
            dev,
            #[cfg(test)]
            fake_mount: None,
        })
    }

    /// Is a directory with this `st_dev` (and this name) somebody else's filesystem?
    fn foreign(&self, dev: u64, name: &OsStr) -> bool {
        self.fake(name) || crosses_device(self.dev, dev)
    }

    #[cfg(test)]
    fn fake(&self, name: &OsStr) -> bool {
        self.fake_mount.as_deref() == Some(name)
    }

    #[cfg(not(test))]
    fn fake(&self, _name: &OsStr) -> bool {
        false
    }
}

/// How `remove_entry` ended.
#[derive(Debug, PartialEq, Eq)]
enum Removed {
    Done,
    /// A directory on another filesystem was left alone; everything else that could go has gone.
    Mount,
}

/// Removes `name` inside the directory `parent`, recursively, and only through descriptors: a file or a link is `unlinkat`ed (a link
/// is never followed), a directory is opened with O_NOFOLLOW|O_DIRECTORY (one swapped for a link after the `stat` is unlinked as a
/// link), its `st_dev` is read from the open descriptor, and it is entered only when that is the root's. A directory on another
/// filesystem is left whole and reported, and its parents stay.
fn remove_entry(root: &Root, parent: RawFd, name: &OsStr) -> io::Result<Removed> {
    let st = match stat_at(parent, name) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Removed::Done),
        Err(e) => return Err(e),
    };
    if !is_dir_mode(&st) {
        unlink_at(parent, name, 0)?;
        return Ok(Removed::Done);
    }
    let dfd = match open_dir_at(parent, name) {
        Ok(fd) => fd,
        Err(e) if matches!(e.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)) => {
            unlink_at(parent, name, 0)?;
            return Ok(Removed::Done);
        }
        Err(e) => return Err(e),
    };
    if root.foreign(fstat_fd(dfd.as_raw_fd())?.st_dev as u64, name) {
        return Ok(Removed::Mount);
    }
    let mut left = false;
    for child in dir_names(dfd.as_raw_fd())? {
        if remove_entry(root, dfd.as_raw_fd(), &child)? == Removed::Mount {
            left = true;
        }
    }
    if left {
        return Ok(Removed::Mount);
    }
    unlink_at(parent, name, libc::AT_REMOVEDIR)?;
    Ok(Removed::Done)
}

/// `.sweep-trash-<pid>-<name>`, the sibling a candidate is renamed to before it is deleted.
fn trash_name(name: &OsStr) -> OsString {
    let mut v = format!("{TRASH_PREFIX}{}-", std::process::id()).into_bytes();
    v.extend_from_slice(name.as_bytes());
    OsString::from_vec(v)
}

/// Decide one candidate target and, unless `dry`, remove it. Cargo's locks are only probed, without waiting, and let go at once —
/// holding one across a removal made a cargo that arrived wait for the whole deletion and then find its folder gone. The race with
/// a build starting after the probe is closed by the removal, not by a lock: the target is renamed (`renameat`, relative to the
/// held root fd) to a sibling `.sweep-trash-<pid>-<name>` in one step — a cargo that starts afterwards makes a fresh folder at the
/// old name — and only the renamed tree is deleted, through the same fd. The never-used classing has already put this target in
/// `doomed`; here it meets the in-use test: a `debug/` in use is skipped like any other. `path` is the candidate, a direct child of
/// `root.path`; `log` gets the `removing` line before the rename, so a wrong deletion leaves a line.
fn sweep_candidate(
    path: &Path,
    bytes: u64,
    now: SystemTime,
    dry: bool,
    procs: &Path,
    root: &Root,
    log: &mut dyn FnMut(&str),
) -> Sweep1 {
    let name = path.file_name().unwrap_or_default();
    let st = match stat_at(root.fd.as_raw_fd(), name) {
        Ok(st) => st,
        Err(_) => return Sweep1::Unreadable,
    };
    if is_link_mode(&st) {
        // A link is unlinked, never followed: nothing of what it points at is read or touched.
        return remove_candidate(root, path, bytes, 0, dry, log);
    }
    if crosses_device(root.dev, st.st_dev as u64) {
        return Sweep1::MountPoint;
    }
    if contains_foreign_dev(path, root.dev) {
        return Sweep1::ContainsMount;
    }
    let Some(locks) = lock_target(path) else {
        return Sweep1::Locked;
    };
    drop(locks);
    let age = match written_ago(path, now) {
        Err(()) => return Sweep1::Unreadable,
        Ok(a) => a,
    };
    if let Some(a) = age
        && a <= IN_USE_WINDOW_SECS
    {
        return Sweep1::Recent(a);
    }
    if let Some(pid) = pid_using(path, procs) {
        return Sweep1::InUse(pid);
    }
    remove_candidate(root, path, bytes, age.unwrap_or(0), dry, log)
}

/// The removal itself: log `removing <path>` (before anything is touched), `renameat` to the trash name, delete the renamed tree
/// through the root fd. A dry run does none of it.
fn remove_candidate(
    root: &Root,
    path: &Path,
    bytes: u64,
    age: u64,
    dry: bool,
    log: &mut dyn FnMut(&str),
) -> Sweep1 {
    if !dry {
        let name = path.file_name().unwrap_or_default();
        log(&format!("sweep-target: removing {}", path.display()));
        let trash = trash_name(name);
        if let Err(e) = rename_at(root.fd.as_raw_fd(), name, &trash) {
            return Sweep1::Failed(format!(
                "rename to {}: {e}",
                root.path.join(&trash).display()
            ));
        }
        match remove_entry(root, root.fd.as_raw_fd(), &trash) {
            Ok(Removed::Done) => {}
            Ok(Removed::Mount) => return Sweep1::Failed("contains a mount point".into()),
            Err(e) => return Sweep1::Failed(e.to_string()),
        }
    }
    Sweep1::Removed { bytes, age }
}

/// Removes the `.sweep-trash-*` folders a crashed sweep left in the cache root, through the root fd and with the same device check,
/// one `removing` line before and one line after each. A dry run only names them.
fn clean_trash(root: &Root, dry: bool, log: &mut dyn FnMut(&str)) {
    let Ok(names) = dir_names(root.fd.as_raw_fd()) else {
        return;
    };
    for name in names {
        if !name.as_bytes().starts_with(TRASH_PREFIX.as_bytes()) {
            continue;
        }
        let p = root.path.join(&name);
        if dry {
            log(&format!("sweep-target: would remove stale {}", p.display()));
            continue;
        }
        log(&format!("sweep-target: removing {}", p.display()));
        match remove_entry(root, root.fd.as_raw_fd(), &name) {
            Ok(Removed::Done) => log(&format!("sweep-target: removed stale {}", p.display())),
            Ok(Removed::Mount) => log(&format!(
                "sweep-target: failed to remove {}: contains a mount point",
                p.display()
            )),
            Err(e) => log(&format!(
                "sweep-target: failed to remove {}: {e}",
                p.display()
            )),
        }
    }
}
/// The one stderr line for one candidate outcome. Every skip and every removal is exactly one line, naming the folder, its size in
/// GiB and its age in seconds, so the next incident identifies itself in the log; a failed removal says why. A dry run says
/// `would remove`.
fn sweep_log(path: &Path, out: &Sweep1, dry: bool) -> String {
    const GIB: f64 = (1u64 << 30) as f64;
    match out {
        Sweep1::Locked => format!("sweep-target: skipped {} (lock held)", path.display()),
        Sweep1::Recent(a) => {
            format!(
                "sweep-target: skipped {} (written {a} s ago)",
                path.display()
            )
        }
        Sweep1::InUse(pid) => {
            format!(
                "sweep-target: skipped {} (in use by pid {pid})",
                path.display()
            )
        }
        Sweep1::Unreadable => format!("sweep-target: skipped {} (unreadable)", path.display()),
        Sweep1::MountPoint => format!("sweep-target: skipped {} (mount point)", path.display()),
        Sweep1::ContainsMount => format!(
            "sweep-target: skipped {} (contains a mount point)",
            path.display()
        ),
        Sweep1::Removed { bytes, age } => format!(
            "sweep-target: {} {} ({:.1} GiB, last written {age} s)",
            if dry { "would remove" } else { "removed" },
            path.display(),
            *bytes as f64 / GIB
        ),
        Sweep1::Failed(e) => format!("sweep-target: failed to remove {}: {e}", path.display()),
    }
}

/// Is an entry on another filesystem than the cache root? `st_dev` of the entry against `st_dev` of the root. A mount point (or a
/// bind mount) inside the cache is somebody else's data that `remove_dir_all` would walk into and empty.
fn crosses_device(root_dev: u64, entry_dev: u64) -> bool {
    root_dev != entry_dev
}

/// Does a folder inside `p` (at any depth, found with `lstat`, so no symlink is followed) sit on another filesystem than `root_dev`?
/// A candidate holding one is skipped whole: removing part of it would be worse than removing none.
fn contains_foreign_dev(p: &Path, root_dev: u64) -> bool {
    let Ok(rd) = fs::read_dir(p) else {
        return false;
    };
    for e in rd.flatten() {
        let q = e.path();
        let Ok(md) = fs::symlink_metadata(&q) else {
            continue;
        };
        if md.is_dir() && (crosses_device(root_dev, md.dev()) || contains_foreign_dev(&q, root_dev))
        {
            return true;
        }
    }
    false
}

/// What a sweep trusts, given by the caller and never read from the environment inside the sweep: the canonical cache root every
/// `--dir` must be strictly under, the home directory a `--dir` may not be (`None`: no such rule, a test's choice), and the process
/// table (`/proc` in the CLI, a test's own folder in a test).
pub struct Guard<'a> {
    pub allowed: &'a Path,
    pub home: Option<&'a Path>,
    pub procs: &'a Path,
}

/// Checks `--dir` before anything is read or removed, and gives back the canonical root. Refused (an `Err` that names the reason;
/// nothing has been removed) when it is empty or not absolute, has a `..` component, is itself a symlink, canonicalizes to `/` or to
/// the home directory, or canonicalizes to anything that is not strictly under the allowed prefix (the canonical `$HOME/.cache` the
/// CLI passes, a temp root in a test) — and when that prefix is itself `/`, under which every path would be legal. From here on only
/// the canonical path is used.
fn check_root(root: &Path, guard: &Guard) -> Result<PathBuf, String> {
    let refuse = |why: String| {
        Err(format!(
            "sweep-target: refusing --dir {:?}: {why}; nothing was removed",
            root.display().to_string()
        ))
    };
    if root.as_os_str().is_empty() {
        return refuse("it is empty".into());
    }
    if !root.is_absolute() {
        return refuse("it is not an absolute path".into());
    }
    if root.components().any(|c| c == Component::ParentDir) {
        return refuse("it has a `..` component".into());
    }
    let md = match fs::symlink_metadata(root) {
        Ok(md) => md,
        Err(e) => return refuse(format!("it cannot be read: {e}")),
    };
    if md.file_type().is_symlink() {
        return refuse("it is a symlink".into());
    }
    let canon = match fs::canonicalize(root) {
        Ok(c) => c,
        Err(e) => return refuse(format!("it cannot be resolved: {e}")),
    };
    if canon == Path::new("/") {
        return refuse("it is `/`".into());
    }
    if let Some(home) = guard.home
        && fs::canonicalize(home).is_ok_and(|h| h == canon)
    {
        return refuse("it is the home directory".into());
    }
    let allowed = match fs::canonicalize(guard.allowed) {
        Ok(a) => a,
        Err(e) => {
            return refuse(format!(
                "the allowed root {} cannot be resolved: {e}",
                guard.allowed.display()
            ));
        }
    };
    if allowed == Path::new("/") {
        return refuse("the allowed root is `/`, under which every path would be legal".into());
    }
    if canon == allowed || !canon.starts_with(&allowed) {
        return refuse(format!(
            "{} is not strictly under {}",
            canon.display(),
            allowed.display()
        ));
    }
    Ok(canon)
}

/// `sweep-target --dir ROOT --limit-gib N [--to-gib M] [--keep TARGET] [--dry-run]`, root mode, logging to stderr. Exit 0 except a
/// refusal (`Err`, exit 2 in the CLI).
pub fn run(
    root: &Path,
    limit_gib: u64,
    to_gib: u64,
    keep: Option<&Path>,
    dry: bool,
    guard: &Guard,
) -> Result<i32, String> {
    run_with(root, limit_gib, to_gib, keep, dry, guard, &mut |l| {
        eprintln!("{l}")
    })
}

/// `run` with its log line sink given, so a test can see the order of the lines against the removal.
fn run_with(
    root: &Path,
    limit_gib: u64,
    to_gib: u64,
    keep: Option<&Path>,
    dry: bool,
    guard: &Guard,
    log: &mut dyn FnMut(&str),
) -> Result<i32, String> {
    const GIB: u64 = 1 << 30;
    if limit_gib == 0 {
        return Err(
            "sweep-target: refusing --limit-gib 0 (it would empty the cache); nothing was removed"
                .into(),
        );
    }
    let (Some(limit_bytes), Some(to_bytes)) = (limit_gib.checked_mul(GIB), to_gib.checked_mul(GIB))
    else {
        return Err(format!(
            "sweep-target: refusing --limit-gib {limit_gib} / --to-gib {to_gib}: too large to count in bytes; nothing was removed"
        ));
    };
    let canon = check_root(root, guard)?;
    // From here every rename and delete goes through this descriptor, not through the path string.
    let root = Root::open(&canon).map_err(|e| {
        format!(
            "sweep-target: refusing --dir {:?}: cannot open it as a directory: {e}; nothing was removed",
            canon.display().to_string()
        )
    })?;
    let keep = keep.map(|k| fs::canonicalize(k).unwrap_or_else(|_| k.to_path_buf()));
    clean_trash(&root, dry, log);
    let total = tree_bytes(&root.path);
    println!(
        "sweep-target: {} is {:.1} GiB (limit {limit_gib}, sweep down to {to_gib})",
        root.path.display(),
        total as f64 / GIB as f64
    );
    if total <= limit_bytes {
        return Ok(0);
    }
    let now = SystemTime::now();
    let entries = candidates(&root.path, keep.as_deref());
    let doomed = plan(&entries, total, limit_bytes, to_bytes);
    let (mut freed, mut n, mut busy) = (0u64, 0, 0);
    for i in &doomed {
        let e = &entries[*i];
        let out = sweep_candidate(&e.path, e.bytes, now, dry, guard.procs, &root, log);
        log(&sweep_log(&e.path, &out, dry));
        match out {
            Sweep1::Removed { bytes, .. } => {
                freed += bytes;
                n += 1;
            }
            Sweep1::Failed(_) => {}
            _ => busy += 1,
        }
    }
    println!(
        "sweep-target: {} {n} cache folders, {:.1} GiB, least recently used first; {busy} skipped (a build holds a cargo lock, something was just written, a process has it open, it cannot be read, or it is or holds a mount point){}",
        if dry { "would remove" } else { "removed" },
        freed as f64 / GIB as f64,
        if dry { " (dry run)" } else { "" }
    );
    Ok(0)
}

/// The paths a folder holds; none when it cannot be read. (The process table is read this way; no target is.)
fn children(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir).map_or_else(
        |_| Vec::new(),
        |rd| rd.flatten().map(|e| e.path()).collect(),
    )
}

// ---- --single: the target is read and written only through the root fd ------------------------------------------------------
//
// Profiles, their `deps/`, `incremental/` and `build/`, their sizes and their `.cargo-lock` are all reached with `openat`
// (O_NOFOLLOW|O_DIRECTORY) from the descriptor of the checked target, never by joining `root.path`: a path swapped for a link after
// the check, a link planted where a folder or a lock file should be, or a profile that is a mount point on another filesystem, is
// never followed, listed, locked or written through.

/// A profile folder of a Cargo target, found through the root fd: its path (for the log) and the descriptor it was opened with.
struct ProfileFd {
    path: PathBuf,
    fd: OwnedFd,
}

/// What `scan_profiles` found: the real profile folders, and the links that were left alone.
struct Scan {
    profiles: Vec<ProfileFd>,
    symlinks: Vec<PathBuf>,
}

fn stat_follow_at(parent: RawFd, name: &OsStr) -> io::Result<libc::stat> {
    let c = cstr(name)?;
    // SAFETY: an all-zero `stat` is a valid value for fstatat to overwrite.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: valid name, valid out pointer, live parent fd. Used only to decide whether to LOG a link that points at a folder.
    if unsafe { libc::fstatat(parent, c.as_ptr(), &mut st, 0) } < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(st)
    }
}

/// A `stat` number as `u64` without a cast (whose source type differs by platform): a negative one is 0.
fn to_u64<T: TryInto<u64>>(v: T) -> u64 {
    v.try_into().unwrap_or(0)
}

/// The later of mtime and atime, in seconds: a link, a test run or a rebuild reads an artifact without writing it.
fn stat_use(st: &libc::stat) -> u64 {
    to_u64(st.st_mtime).max(to_u64(st.st_atime))
}

/// The bytes under the directory `fd` (files by `lstat` size, folders by their entries; a link counts its own length, never what it
/// points at). The same count `tree_bytes` gives, read through descriptors.
fn bytes_in(fd: RawFd) -> u64 {
    dir_names(fd).map_or(0, |ns| ns.iter().map(|n| bytes_at(fd, n)).sum())
}

fn bytes_at(parent: RawFd, name: &OsStr) -> u64 {
    let Ok(st) = stat_at(parent, name) else {
        return 0;
    };
    if !is_dir_mode(&st) {
        return to_u64(st.st_size);
    }
    open_dir_at(parent, name).map_or(0, |fd| bytes_in(fd.as_raw_fd()))
}

/// A profile folder of a Cargo target, where its artifacts live: `debug` or `release`, directly under the target or under a
/// target-triple folder, or any folder that holds a real `deps/`. `fd` is the open folder.
fn is_profile_at(name: &OsStr, fd: RawFd) -> bool {
    name.as_bytes() == b"debug"
        || name.as_bytes() == b"release"
        || stat_at(fd, OsStr::new("deps")).is_ok_and(|s| is_dir_mode(&s))
}

/// Keeps a profile unless its device is not the root's: its own `fstat` says so, and a foreign one is skipped with a line.
fn add_profile(root: &Root, scan: &mut Scan, path: PathBuf, name: &OsStr, fd: OwnedFd) {
    match fstat_fd(fd.as_raw_fd()) {
        Ok(st) if !root.foreign(to_u64(st.st_dev), name) => {
            scan.profiles.push(ProfileFd { path, fd })
        }
        Ok(_) => eprintln!("sweep-target: skipped {} (mount point)", path.display()),
        Err(e) => eprintln!("sweep-target: skipped {} ({e})", path.display()),
    }
}

/// The profile folders of ONE target: those directly under it and those one level deeper under a target triple, found through the
/// root fd. A link is never a profile and never descended into; one that points at a folder is listed in `symlinks` so the caller can
/// say it was skipped.
fn scan_profiles(root: &Root) -> Scan {
    let mut scan = Scan {
        profiles: Vec::new(),
        symlinks: Vec::new(),
    };
    let rfd = root.fd.as_raw_fd();
    let mut top = dir_names(rfd).unwrap_or_default();
    top.sort();
    for name in top {
        let Ok(st) = stat_at(rfd, &name) else {
            continue;
        };
        let path = root.path.join(&name);
        if is_link_mode(&st) {
            if stat_follow_at(rfd, &name).is_ok_and(|s| is_dir_mode(&s)) {
                scan.symlinks.push(path);
            }
            continue;
        }
        if !is_dir_mode(&st) {
            continue;
        }
        let Ok(fd) = open_dir_at(rfd, &name) else {
            continue;
        };
        if is_profile_at(&name, fd.as_raw_fd()) {
            add_profile(root, &mut scan, path, &name, fd);
            continue;
        }
        // Not a profile: a target-triple folder, whose profiles are one level down.
        let mut inner = dir_names(fd.as_raw_fd()).unwrap_or_default();
        inner.sort();
        for g in inner {
            let Ok(gst) = stat_at(fd.as_raw_fd(), &g) else {
                continue;
            };
            let gpath = path.join(&g);
            if is_link_mode(&gst) {
                if stat_follow_at(fd.as_raw_fd(), &g).is_ok_and(|s| is_dir_mode(&s)) {
                    scan.symlinks.push(gpath);
                }
                continue;
            }
            if !is_dir_mode(&gst) {
                continue;
            }
            if let Ok(gfd) = open_dir_at(fd.as_raw_fd(), &g)
                && is_profile_at(&g, gfd.as_raw_fd())
            {
                add_profile(root, &mut scan, gpath, &g, gfd);
            }
        }
    }
    scan.profiles.sort_by(|a, b| a.path.cmp(&b.path));
    scan
}

/// `sub` of a profile (`deps`, `incremental`, `build`) opened through the profile fd. `None` when it is missing or not a folder, and,
/// with a `skipped` line, when it is a symlink: listing it would follow the link out of the target.
fn open_sub(prof: &ProfileFd, sub: &str) -> Option<OwnedFd> {
    let name = OsStr::new(sub);
    let st = stat_at(prof.fd.as_raw_fd(), name).ok()?;
    if is_link_mode(&st) {
        eprintln!(
            "sweep-target: skipped {} (symlink)",
            prof.path.join(sub).display()
        );
        return None;
    }
    if !is_dir_mode(&st) {
        return None;
    }
    open_dir_at(prof.fd.as_raw_fd(), name).ok()
}

/// What may go from ONE target: the files of each profile's `deps/` and the folders of its `incremental/` and `build/`. Nothing
/// directly in the profile folder (those are the binaries) and no fingerprint. Cargo rebuilds every one of them. Read through the
/// profile descriptors; `deps/`, `incremental/` and `build/` that are links are skipped.
fn single_artifacts(profiles: &[ProfileFd]) -> Vec<Entry> {
    let mut out = Vec::new();
    for prof in profiles {
        if let Some(dfd) = open_sub(prof, "deps") {
            for name in dir_names(dfd.as_raw_fd()).unwrap_or_default() {
                let Ok(st) = stat_at(dfd.as_raw_fd(), &name) else {
                    continue;
                };
                if is_dir_mode(&st) {
                    continue;
                }
                out.push(Entry {
                    bytes: to_u64(st.st_size),
                    used: stat_use(&st),
                    path: prof.path.join("deps").join(&name),
                });
            }
        }
        for sub in ["incremental", "build"] {
            let Some(dfd) = open_sub(prof, sub) else {
                continue;
            };
            for name in dir_names(dfd.as_raw_fd()).unwrap_or_default() {
                let Ok(st) = stat_at(dfd.as_raw_fd(), &name) else {
                    continue;
                };
                if !is_dir_mode(&st) {
                    continue;
                }
                out.push(Entry {
                    bytes: bytes_at(dfd.as_raw_fd(), &name),
                    used: stat_use(&st),
                    path: prof.path.join(sub).join(&name),
                });
            }
        }
    }
    out
}

/// A program name that is part of a Cargo build. A build script or test binary has an arbitrary name, so those are recognised by
/// what they hold open instead.
fn is_builder(comm: &str) -> bool {
    let n = comm.split_whitespace().next().unwrap_or("");
    n == "cargo"
        || n == "rustc"
        || n == "rustdoc"
        || n.starts_with("build-script")
        || n.starts_with("cargo-")
}

/// `cand`, or the path it resolves to, lies inside `real`.
fn inside(real: &Path, cand: &Path) -> bool {
    fs::canonicalize(cand)
        .unwrap_or_else(|_| cand.to_path_buf())
        .starts_with(real)
}

/// Is a process building into this exact target right now? Two ways, and only two:
///
/// - it names this target in `CARGO_TARGET_DIR` and is a build program (cargo, rustc, rustdoc, a build script): that is a build
///   of this target, running now, whatever it has open;
/// - it has its cwd or a file open inside the target, and either names this target (which is how a test binary or a build script,
///   whose name says nothing, is recognised: Cargo's variable is in its environment) or is a build program (a hand-run rustc,
///   whose parent took no lock at all).
///
/// An idle shell that only exports the variable is neither, so it cannot hold the cap open forever. A `CARGO_TARGET_DIR` that is
/// an ancestor or a sibling of this target writes elsewhere, so it is not a reason to skip. Cargo's package lock covers the same
/// ground while a build runs; this also catches a build between two locks. Our own pid does not count: we hold those locks.
fn target_in_use(dir: &Path, procs: &Path) -> Option<String> {
    let real = fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    for p in children(procs) {
        let pid = p
            .file_name()
            .map_or(String::new(), |n| n.to_string_lossy().into_owned());
        if pid == std::process::id().to_string() {
            continue;
        }
        let comm = fs::read_to_string(p.join("comm")).unwrap_or_default();
        let builder = is_builder(&comm);
        let names_us = fs::read(p.join("environ")).is_ok_and(|env| {
            env.split(|b| *b == 0).any(|kv| {
                let kv = String::from_utf8_lossy(kv);
                ["CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR"]
                    .iter()
                    .filter_map(|k| kv.strip_prefix(&format!("{k}=")))
                    .any(|v| inside(&real, Path::new(v)))
            })
        });
        if !builder && !names_us {
            continue;
        }
        if builder && names_us {
            return Some(format!(
                "pid {pid} ({}) builds into it, CARGO_TARGET_DIR names it",
                comm.trim_end()
            ));
        }
        let open = children(&p.join("fd")).into_iter().chain([p.join("cwd")]);
        for f in open {
            let Ok(target) = fs::read_link(&f) else {
                continue;
            };
            if inside(&real, &target) {
                return Some(format!(
                    "pid {pid} ({}) has {} open in it",
                    comm.trim_end(),
                    target.display()
                ));
            }
        }
    }
    None
}

/// Exclusive non-blocking locks on `.cargo-lock` of EVERY profile folder of one target, held while its artifacts go. `None`: a build
/// is running in it (or a lock cannot be had), so nothing must be removed. A missing lock is created first, so a build starting now
/// waits for us; a dry run creates nothing and only checks the locks that exist. Both are `openat` on the profile descriptor with
/// O_NOFOLLOW: a `.cargo-lock` that is a link is never opened (that is a refusal, not a lock), and a lock is never created through one.
fn lock_profiles(profiles: &[ProfileFd], dry: bool) -> Option<Vec<fs::File>> {
    let lock = OsStr::new(".cargo-lock");
    let mut held = Vec::new();
    for prof in profiles {
        let pfd = prof.fd.as_raw_fd();
        if dry && stat_at(pfd, lock).is_err() {
            continue;
        }
        let mut flags = libc::O_RDWR | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        if !dry {
            flags |= libc::O_CREAT;
        }
        let c = cstr(lock).ok()?;
        // SAFETY: valid NUL-terminated name and a live profile fd; the mode is only read with O_CREAT.
        let fd = check_fd(unsafe { libc::openat(pfd, c.as_ptr(), flags, 0o644 as libc::c_uint) })
            .ok()?;
        let f = fs::File::from(fd);
        f.try_lock().ok()?;
        held.push(f);
    }
    Some(held)
}

/// One artifact: a whole folder with what is in it, or a single file, removed relative to the held root fd: each folder on the way
/// is opened O_NOFOLLOW (a link swapped in for one fails the open), the artifact is `unlinkat`ed or taken by `remove_entry` (so a
/// folder on another filesystem is left), and `removing <path>` is logged first. False, with a `failed to remove` line, when it
/// could not be removed.
fn remove_artifact(root: &Root, p: &Path) -> bool {
    // Before the call, so a wrong deletion leaves a line even when the process dies half-way.
    eprintln!("sweep-target: removing {}", p.display());
    let fail = |why: String| {
        eprintln!("sweep-target: failed to remove {}: {why}", p.display());
        false
    };
    let Ok(rel) = p.strip_prefix(&root.path) else {
        return fail("it is not inside the target".into());
    };
    let names: Vec<&OsStr> = rel.components().map(|c| c.as_os_str()).collect();
    let Some((last, parents)) = names.split_last() else {
        return fail("it is the target itself".into());
    };
    let mut held: Option<OwnedFd> = None;
    for name in parents {
        let raw = held.as_ref().map_or(root.fd.as_raw_fd(), |f| f.as_raw_fd());
        match open_dir_at(raw, name) {
            Ok(fd) => held = Some(fd),
            Err(e) => return fail(e.to_string()),
        }
    }
    let raw = held.as_ref().map_or(root.fd.as_raw_fd(), |f| f.as_raw_fd());
    match remove_entry(root, raw, last) {
        Ok(Removed::Done) => true,
        Ok(Removed::Mount) => fail("contains a mount point".into()),
        Err(e) => fail(e.to_string()),
    }
}

/// One target dir, byte limits (so a test needs no GiB of files), byte marks and the process table to consult. Exit 0 as usual,
/// 3 when the target is in use and was left alone, so the caller can log that it skipped; the `Err` paths are the usage errors.
fn sweep_single(
    dir: &Path,
    limit_bytes: u64,
    to_bytes: u64,
    dry: bool,
    procs: &Path,
) -> Result<i32, String> {
    if !dir.is_dir() {
        return Err(format!(
            "sweep-target --single: {} is not a folder",
            dir.display()
        ));
    }
    // Held open from here on: every removal below is relative to this descriptor, not to the path string.
    let root = Root::open(dir).map_err(|e| {
        format!(
            "sweep-target --single: {} cannot be opened as a directory: {e}",
            dir.display()
        )
    })?;
    sweep_single_root(&root, limit_bytes, to_bytes, dry, procs)
}

/// `sweep_single` on a root that is already open.
fn sweep_single_root(
    root: &Root,
    limit_bytes: u64,
    to_bytes: u64,
    dry: bool,
    procs: &Path,
) -> Result<i32, String> {
    const GIB: f64 = (1u64 << 30) as f64;
    let dir = root.path.as_path();
    let total = bytes_in(root.fd.as_raw_fd());
    println!(
        "sweep-target --single: {} is {:.1} GiB (limit {:.0} GiB, sweep down to {:.0} GiB)",
        dir.display(),
        total as f64 / GIB,
        limit_bytes as f64 / GIB,
        to_bytes as f64 / GIB
    );
    if total <= limit_bytes {
        return Ok(0);
    }
    let scan = scan_profiles(root);
    for l in &scan.symlinks {
        eprintln!("sweep-target: skipped {} (symlink profile)", l.display());
    }
    if let Some(why) = target_in_use(dir, procs) {
        println!(
            "sweep-target --single: {} in use: not swept ({why})",
            dir.display()
        );
        return Ok(3);
    }
    let Some(locks) = lock_profiles(&scan.profiles, dry) else {
        println!(
            "sweep-target --single: {} in use: not swept (a build holds a cargo lock in it)",
            dir.display()
        );
        return Ok(3);
    };
    // A dry run removes nothing, so it does not hold a running build off the target while it counts; a real run keeps the locks
    // until the artifacts are gone, and Cargo waits for them.
    if dry {
        drop(locks);
    }
    let entries = single_artifacts(&scan.profiles);
    let doomed = plan(&entries, total, limit_bytes, to_bytes);
    let (mut freed, mut n) = (0u64, 0);
    for i in &doomed {
        let e = &entries[*i];
        if !dry && !remove_artifact(root, &e.path) {
            continue;
        }
        freed += e.bytes;
        n += 1;
    }
    println!(
        "sweep-target --single: {} {n} build artifacts, {:.1} GiB, least recently used first; {:.1} GiB -> {:.1} GiB{}",
        if dry { "would remove" } else { "removed" },
        freed as f64 / GIB,
        total as f64 / GIB,
        (total - freed.min(total)) as f64 / GIB,
        if dry { " (dry run)" } else { "" }
    );
    Ok(0)
}

/// The GiB marks people and the timer give, in the KiB `run_single` takes: one GiB is 1024 KiB of 1024 bytes, so a cap of 40 GiB
/// is 41 943 040 KiB. The same scale as `run` uses for the cache root, and the one `run_single` multiplies back into bytes.
pub fn caps_kib(limit_gib: u64, to_gib: u64) -> (u64, u64) {
    const KIB_IN_GIB: u64 = 1 << 20;
    (limit_gib * KIB_IN_GIB, to_gib * KIB_IN_GIB)
}

/// `caps_kib` for the CLI: `None` when a mark does not fit in bytes (the product `run_single` takes back into bytes included).
pub fn caps_kib_checked(limit_gib: u64, to_gib: u64) -> Option<(u64, u64)> {
    let fits = |g: u64| {
        g.checked_mul(1 << 20)
            .is_some_and(|k| k.checked_mul(1 << 10).is_some())
    };
    (fits(limit_gib) && fits(to_gib)).then(|| caps_kib(limit_gib, to_gib))
}

/// `sweep-target --dir D --single --limit-gib N [--to-gib M] [--limit-kib K] [--to-kib T] [--dry-run]`: cap ONE Cargo target dir
/// (the app and landing caches). The KiB marks exist so a test can run the real thing on a folder of a few KiB.
pub fn run_single(
    dir: &Path,
    limit_kib: u64,
    to_kib: u64,
    dry: bool,
    guard: &Guard,
) -> Result<i32, String> {
    const KIB: u64 = 1 << 10;
    if limit_kib == 0 {
        return Err(
            "sweep-target --single: refusing a cap of 0 (it would empty the target); nothing was removed"
                .into(),
        );
    }
    let (Some(lim), Some(to)) = (limit_kib.checked_mul(KIB), to_kib.checked_mul(KIB)) else {
        return Err(
            "sweep-target --single: the cap is too large to count in bytes; nothing was removed"
                .into(),
        );
    };
    // The same checks as root mode (a symlink `--dir` is refused, never canonicalized into a sweep), and then only the canonical
    // path, so every `removing` line names the folder that really goes.
    let dir = check_root(dir, guard)?;
    sweep_single(&dir, lim, to, dry, guard.procs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(name: &str, bytes: u64, used: u64) -> Entry {
        Entry {
            path: PathBuf::from(name),
            bytes,
            used,
        }
    }

    #[test]
    fn under_the_limit_nothing_goes() {
        let v = [e("a", 10, 1), e("b", 10, 2)];
        assert!(plan(&v, 20, 20, 10).is_empty());
        assert!(plan(&v, 5, 20, 10).is_empty());
    }

    #[test]
    fn the_oldest_go_first_down_to_the_lower_mark() {
        let v = [
            e("new", 10, 300),
            e("old", 10, 100),
            e("mid", 10, 200),
            e("tiny", 1, 50),
        ];
        let gone: Vec<&str> = plan(&v, 31, 25, 15)
            .into_iter()
            .map(|i| v[i].path.to_str().unwrap())
            .collect();
        assert_eq!(gone, ["tiny", "old", "mid"]);
    }

    #[test]
    fn ties_go_in_name_order_and_all_can_go() {
        let v = [e("b", 5, 1), e("a", 5, 1)];
        assert_eq!(plan(&v, 10, 4, 0), [1, 0]);
    }

    #[test]
    fn worktree_targets_go_least_recently_used_first_and_the_one_in_use_stays() {
        let t = std::env::temp_dir().join(format!("lucina-ci-sweep-{}", std::process::id()));
        let _ = fs::remove_dir_all(&t);
        for (name, age) in [("wt-old", 10_000u64), ("wt-new", 10), ("wt-mine", 5_000)] {
            let d = t.join(name);
            fs::create_dir_all(d.join("debug")).unwrap();
            fs::write(d.join("debug/blob"), vec![0u8; 1000]).unwrap();
            let f = fs::File::create(d.join(".last-used")).unwrap();
            let when = SystemTime::now() - std::time::Duration::from_secs(age);
            f.set_times(fs::FileTimes::new().set_modified(when))
                .unwrap();
        }
        // The old shared layout: never used, goes first.
        fs::create_dir_all(t.join("debug")).unwrap();
        fs::write(t.join("debug/blob"), vec![0u8; 1000]).unwrap();
        fs::write(t.join("CACHEDIR.TAG"), "x").unwrap();
        let mine = t.join("wt-mine");
        let entries = candidates(&t, Some(&mine));
        assert_eq!(entries.len(), 3, "{entries:?}");
        let names: Vec<String> = plan(&entries, tree_bytes(&t), 3000, 2200)
            .into_iter()
            .map(|i| {
                entries[i]
                    .path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(names, ["debug", "wt-old"]);
        fs::remove_dir_all(&t).ok();
    }

    #[test]
    fn a_target_with_a_build_running_is_skipped() {
        let t = std::env::temp_dir().join(format!("lucina-ci-sweep-lock-{}", std::process::id()));
        let _ = fs::remove_dir_all(&t);
        fs::create_dir_all(t.join("wt-busy/debug")).unwrap();
        fs::write(t.join("wt-busy/debug/.cargo-lock"), "").unwrap();
        let build = fs::OpenOptions::new()
            .write(true)
            .open(t.join("wt-busy/debug/.cargo-lock"))
            .unwrap();
        build.lock().unwrap();
        assert!(lock_target(&t.join("wt-busy")).is_none());
        drop(build);
        assert!(
            lock_frees_up(&mut || lock_target(&t.join("wt-busy")).is_some()),
            "the target lock we released is lockable again"
        );
        fs::remove_dir_all(&t).ok();
    }

    // ---- root mode (run): the in-use test and its log lines (finding GATE-09) ----
    //
    // `run` only acts past a 30 GiB total, so these tests drive the per-candidate decision `sweep_candidate` directly — the exact
    // function `run`'s loop calls for each doomed folder — on tiny temp dirs. The clock is fixed to a single `now` and every
    // folder's mtime is stamped to `now - ago`, so an age of 60 s reads as exactly 60 s and never drifts with the wall clock.

    /// Removes a folder tree on drop, so a panic half-way still cleans the temp dir (the run-mode tests leave nothing behind).
    struct TempTree(PathBuf);
    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// A process table that does not exist: no process, so the decision rests on the locks and the clock alone.
    fn no_procs() -> &'static Path {
        Path::new("/nonexistent-lucina-proc")
    }

    /// A fresh temp dir under `name`, wrapped so a panic still removes it.
    fn temp_tree(name: &str) -> (TempTree, PathBuf) {
        let p =
            std::env::temp_dir().join(format!("lucina-ci-sweep-run-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        (TempTree(p.clone()), p)
    }

    /// Stamps `p` with `when` (mtime and atime). `File::open` on a directory succeeds on Linux, and `futimens` then stamps the
    /// folder itself — so a directory created during the test does not sneak a real-`now` mtime into `newest_write`'s scan of a
    /// target's top-level entries.
    fn stamp(p: &Path, when: SystemTime) {
        let f = if p.is_dir() {
            // A directory cannot be opened for writing; `File::open` gives a read-only fd, and futimens on a file we own needs
            // no write access to stamp its times.
            fs::File::open(p)
        } else {
            fs::OpenOptions::new().write(true).open(p)
        };
        if let Ok(f) = f {
            let _ = f.set_times(fs::FileTimes::new().set_modified(when).set_accessed(when));
        }
    }

    /// Recursively stamps a folder and everything under it to `when`, so the newest write under a candidate is exactly `when`.
    fn stamp_all(dir: &Path, when: SystemTime) {
        if let Ok(rd) = fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stamp_all(&p, when);
                }
                stamp(&p, when);
            }
        }
        stamp(dir, when);
    }

    /// One artifact of `bytes` at `rel` under `dir`, so `dir` looks like a target with a Cargo `deps/` in it.
    fn mk_dep(dir: &Path, rel: &str, bytes: usize) -> PathBuf {
        let p = dir.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, vec![0u8; bytes]).unwrap();
        p
    }

    #[test]
    fn a_candidate_whose_cargo_lock_is_held_is_skipped_and_logged() {
        let now = SystemTime::now();
        let (_g, t) = temp_tree("lockheld");
        let child = t.join("debug");
        fs::create_dir_all(&child).unwrap();
        let lock = mk_dep(&child, ".cargo-lock", 0);
        stamp_all(&child, now - std::time::Duration::from_secs(700)); // idle, but the lock is what matters
        // Another "process" holds the build lock: an fd of our own with an exclusive lock on the same file.
        let held = fs::OpenOptions::new().write(true).open(&lock).unwrap();
        held.lock().unwrap();
        let out = sweep_candidate(
            &child,
            1234,
            now,
            false,
            no_procs(),
            &root_of(&t),
            &mut |_| {},
        );
        assert!(
            matches!(out, Sweep1::Locked),
            "a held cargo lock must skip: {out:?}"
        );
        assert_eq!(
            sweep_log(&child, &out, false),
            format!("sweep-target: skipped {} (lock held)", child.display())
        );
        assert!(child.exists(), "a folder a build holds is never removed");
        drop(held);
    }

    #[test]
    fn a_target_written_a_minute_ago_is_skipped_and_logged() {
        let now = SystemTime::now();
        let (_g, t) = temp_tree("recent");
        let child = t.join("debug");
        mk_dep(&child, "deps/deals-abc", 1000);
        stamp_all(&child, now - std::time::Duration::from_secs(60)); // newest write 60 s ago, no lock held
        let out = sweep_candidate(
            &child,
            1000,
            now,
            false,
            no_procs(),
            &root_of(&t),
            &mut |_| {},
        );
        assert!(
            matches!(out, Sweep1::Recent(60)),
            "a fresh folder is in use: {out:?}"
        );
        assert_eq!(
            sweep_log(&child, &out, false),
            format!(
                "sweep-target: skipped {} (written 60 s ago)",
                child.display()
            )
        );
        assert!(child.exists(), "a just-written folder is never removed");
    }

    #[test]
    fn an_idle_unlocked_target_past_the_window_is_removed_and_logged() {
        let now = SystemTime::now();
        let (_g, t) = temp_tree("idle");
        let child = t.join("wt-idle");
        mk_dep(&child, "debug/deps/old.rlib", 1000);
        stamp_all(&child, now - std::time::Duration::from_secs(700)); // older than the 600 s window, no lock
        let out = sweep_candidate(
            &child,
            1000,
            now,
            false,
            no_procs(),
            &root_of(&t),
            &mut |_| {},
        );
        assert!(
            matches!(out, Sweep1::Removed { age: 700, .. }),
            "idle and unlocked past the window must go: {out:?}"
        );
        assert_eq!(
            sweep_log(&child, &out, false),
            format!(
                "sweep-target: removed {} (0.0 GiB, last written 700 s)",
                child.display()
            )
        );
        assert!(!child.exists(), "an idle folder is removed");
    }

    #[test]
    fn the_root_debug_child_in_use_is_skipped() {
        // The GATE-09 case: `<cache_root>/debug`, cargo's build lock released after "Finished", test binaries still being run.
        // A sweep must leave it alone — never-used classing sends it first, but the in-use test now comes before the removal.
        let now = SystemTime::now();
        let (_g, t) = temp_tree("gate09");
        let dbg = t.join("debug");
        mk_dep(&dbg, ".cargo-lock", 0); // cargo's lock, present but not held (compile done)
        mk_dep(&dbg, "deps/deals-49dbc", 1000);
        stamp_all(&dbg, now - std::time::Duration::from_secs(30)); // binaries written half a minute ago
        let out = sweep_candidate(
            &dbg,
            1000,
            now,
            false,
            no_procs(),
            &root_of(&t),
            &mut |_| {},
        );
        assert!(
            matches!(out, Sweep1::Recent(30)),
            "a live test's folder is in use: {out:?}"
        );
        assert_eq!(
            sweep_log(&dbg, &out, false),
            format!("sweep-target: skipped {} (written 30 s ago)", dbg.display())
        );
        assert!(
            dbg.exists(),
            "GATE-09: the shared debug/ is never stripped from a running test"
        );
    }

    // ---- root mode: process check, rename-then-delete, unreadable (PF-C040 review fixes) ----

    /// Kills and waits for a child on drop, so a panic leaves no process behind.
    struct Kill(std::process::Child);
    impl Drop for Kill {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// Gives the permissions of folders back on drop, so `TempTree` can remove them after a panic.
    struct Restore(Vec<PathBuf>);
    impl Drop for Restore {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt;
            for p in &self.0 {
                let _ = fs::set_permissions(p, fs::Permissions::from_mode(0o755));
            }
        }
    }

    /// Runs `exe 600` as a child. A binary just written can be "text file busy" while another test thread's fork still holds the
    /// write descriptor for an instant, so the spawn is retried for a second.
    fn spawn_sleeper(exe: &Path) -> std::process::Child {
        for _ in 0..50 {
            match std::process::Command::new(exe)
                .arg("600")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                Ok(c) => return c,
                Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(e) => panic!("cannot run {}: {e}", exe.display()),
            }
        }
        panic!("{} stayed busy", exe.display());
    }

    #[test]
    fn a_target_a_process_runs_a_binary_from_is_kept_however_old_it_is() {
        let now = SystemTime::now();
        let (_g, t) = temp_tree("inuse");
        let cand = t.join("wt-running");
        let deps = cand.join("debug/deps");
        fs::create_dir_all(&deps).unwrap();
        let exe = deps.join("sleep");
        fs::copy("/bin/sleep", &exe).unwrap();
        // Every stamp is 700 s old and no cargo lock exists: only the running binary says this target is in use.
        // (Stamped before the binary runs: a running executable cannot be opened for writing.)
        stamp_all(&cand, now - std::time::Duration::from_secs(700));
        // Named `sleep`: /bin/sleep is a multi-call binary that picks its program from its own name.
        let mut kid = Kill(spawn_sleeper(&exe));
        assert!(
            kid.0.try_wait().unwrap().is_none(),
            "the sleeper exited at once"
        );
        let pid = kid.0.id();
        let out = sweep_candidate(
            &cand,
            1,
            now,
            false,
            Path::new("/proc"),
            &root_of(&t),
            &mut |_| {},
        );
        assert!(
            matches!(out, Sweep1::InUse(p) if p == pid),
            "a running binary of the target must skip it: {out:?}"
        );
        assert_eq!(
            sweep_log(&cand, &out, false),
            format!(
                "sweep-target: skipped {} (in use by pid {pid})",
                cand.display()
            )
        );
        assert!(cand.join("debug/deps/sleep").exists());
    }

    #[test]
    fn an_idle_target_is_removed_through_a_rename_and_leaves_no_trash() {
        let now = SystemTime::now();
        let (_g, t) = temp_tree("trash");
        let cand = t.join("wt-idle");
        mk_dep(&cand, "debug/deps/old.rlib", 1000);
        stamp_all(&cand, now - std::time::Duration::from_secs(700));
        let (mut seen, mut there_at_log) = (Vec::new(), false);
        let cp = cand.clone();
        let out = sweep_candidate(
            &cand,
            1000,
            now,
            false,
            no_procs(),
            &root_of(&t),
            &mut |l| {
                seen.push(l.to_string());
                there_at_log = cp.exists();
            },
        );
        assert!(matches!(out, Sweep1::Removed { age: 700, .. }), "{out:?}");
        assert!(!cand.exists(), "the original name is gone");
        let left: Vec<_> = fs::read_dir(&t).unwrap().flatten().collect();
        assert!(left.is_empty(), "no .sweep-trash-* is left: {left:?}");
        assert_eq!(
            seen,
            [format!("sweep-target: removing {}", cand.display())],
            "one line, before the rename"
        );
        assert!(there_at_log, "the line came while the folder still existed");
    }

    #[test]
    fn a_stale_trash_folder_is_removed_and_logged_and_is_never_a_candidate() {
        let (_g, t) = temp_tree("stale");
        mk_dep(&t, ".sweep-trash-99-wt-crashed/debug/deps/x.rlib", 10);
        mk_dep(&t, "wt-keep/debug/deps/y.rlib", 10);
        let names: Vec<_> = candidates(&t, None)
            .into_iter()
            .map(|e| e.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["wt-keep"], "trash is not a candidate");
        let mut dry = Vec::new();
        clean_trash(&root_of(&t), true, &mut |l| dry.push(l.to_string()));
        assert!(
            t.join(".sweep-trash-99-wt-crashed").exists(),
            "a dry run keeps it"
        );
        assert_eq!(dry.len(), 1, "{dry:?}");
        let mut seen = Vec::new();
        clean_trash(&root_of(&t), false, &mut |l| seen.push(l.to_string()));
        let stale = t.join(".sweep-trash-99-wt-crashed");
        assert!(!stale.exists());
        assert!(t.join("wt-keep").exists(), "only trash goes");
        assert_eq!(
            seen,
            [
                format!("sweep-target: removing {}", stale.display()),
                format!("sweep-target: removed stale {}", stale.display())
            ]
        );
    }

    #[test]
    fn an_unreadable_target_is_skipped_not_removed() {
        use std::os::unix::fs::PermissionsExt;
        let now = SystemTime::now();
        let (_g, t) = temp_tree("unreadable");
        let cand = t.join("wt-locked");
        mk_dep(&cand, "deps/old.rlib", 100);
        stamp_all(&cand, now - std::time::Duration::from_secs(700));
        let _restore = Restore(vec![cand.clone(), cand.join("deps")]);
        for p in [cand.join("deps"), cand.clone()] {
            fs::set_permissions(&p, fs::Permissions::from_mode(0o000)).unwrap();
        }
        let out = sweep_candidate(
            &cand,
            100,
            now,
            false,
            no_procs(),
            &root_of(&t),
            &mut |_| {},
        );
        assert!(matches!(out, Sweep1::Unreadable), "{out:?}");
        assert_eq!(
            sweep_log(&cand, &out, false),
            format!("sweep-target: skipped {} (unreadable)", cand.display())
        );
        assert!(cand.exists(), "an unreadable target is never removed");
    }

    #[test]
    fn the_cargo_locks_are_not_held_during_the_sweep() {
        let now = SystemTime::now();
        // Skipped as recent: the probe lock is gone once the call returns.
        let (_g, t) = temp_tree("lockfree");
        let busy = t.join("wt-recent");
        let lock = mk_dep(&busy, "debug/.cargo-lock", 0);
        mk_dep(&busy, "debug/deps/a", 1);
        stamp_all(&busy, now - std::time::Duration::from_secs(60));
        let out = sweep_candidate(&busy, 1, now, false, no_procs(), &root_of(&t), &mut |_| {});
        assert!(matches!(out, Sweep1::Recent(60)), "{out:?}");
        let f = fs::OpenOptions::new().write(true).open(&lock).unwrap();
        assert!(
            lock_frees_up(&mut || f.try_lock().is_ok()),
            "lock held after a skip"
        );
        drop(f);
        // Being removed: the lock is free at the moment the `removing` line is written, before the folder moves.
        let idle = t.join("wt-idle");
        let lock = mk_dep(&idle, ".cargo-lock", 0);
        stamp_all(&idle, now - std::time::Duration::from_secs(700));
        let mut free_at_log = false;
        let out = sweep_candidate(&idle, 1, now, false, no_procs(), &root_of(&t), &mut |_| {
            let f = fs::OpenOptions::new().write(true).open(&lock).unwrap();
            free_at_log = lock_frees_up(&mut || f.try_lock().is_ok());
        });
        assert!(matches!(out, Sweep1::Removed { .. }), "{out:?}");
        assert!(free_at_log, "the probe lock was held into the removal");
    }

    #[test]
    fn a_failed_removal_says_so_and_a_dry_run_says_would_remove() {
        let p = Path::new("/c/wt-x");
        assert_eq!(
            sweep_log(p, &Sweep1::Failed("boom".into()), false),
            "sweep-target: failed to remove /c/wt-x: boom"
        );
        let r = Sweep1::Removed {
            bytes: 1 << 30,
            age: 9,
        };
        assert_eq!(
            sweep_log(p, &r, true),
            "sweep-target: would remove /c/wt-x (1.0 GiB, last written 9 s)"
        );
        assert_eq!(
            sweep_log(p, &r, false),
            "sweep-target: removed /c/wt-x (1.0 GiB, last written 9 s)"
        );
    }

    #[test]
    fn an_unreadable_process_of_ours_counts_only_when_its_command_line_names_the_target() {
        use std::os::unix::fs::PermissionsExt;
        let (_g, t) = temp_tree("denied");
        let (x, y) = (t.join("wt-x"), t.join("wt-y"));
        mk_dep(&x, "deps/a", 1);
        mk_dep(&y, "deps/b", 1);
        let procs = t.join("procs");
        let mut fds = Vec::new();
        for (pid, arg) in [
            ("4001", format!("{}/deps/a", x.display())),
            ("4002", "/usr/bin/other".to_string()),
        ] {
            fs::create_dir_all(procs.join(pid).join("fd")).unwrap();
            fs::write(procs.join(pid).join("cmdline"), format!("{arg}\0--flag\0")).unwrap();
            fds.push(procs.join(pid).join("fd"));
        }
        let _restore = Restore(fds.clone());
        for fd in &fds {
            fs::set_permissions(fd, fs::Permissions::from_mode(0o000)).unwrap();
        }
        assert_eq!(
            pid_using(&x, &procs),
            Some(4001),
            "its command line names the target"
        );
        assert_eq!(pid_using(&y, &procs), None, "nothing names this one");
    }

    // ---- root mode: the --dir checks, the device check, the limits and the `removing` line (PF-C040 path proof, rules 6-9) ----

    fn dev(p: &Path) -> u64 {
        fs::metadata(p).unwrap().dev()
    }

    /// The directory held open the way a sweep holds the cache root.
    fn root_of(p: &Path) -> Root {
        Root::open(p).unwrap()
    }

    /// The `.cargo-lock` of every profile of a target, taken through a freshly opened root.
    fn lock_all(t: &Path, dry: bool) -> Option<Vec<fs::File>> {
        let root = root_of(t);
        lock_profiles(&scan_profiles(&root).profiles, dry)
    }

    /// A sweep's trust for a test: this allowed root, no home rule, no process table.
    fn guard(allowed: &Path) -> Guard<'_> {
        Guard {
            allowed,
            home: None,
            procs: no_procs(),
        }
    }

    /// A temp root with a canonical `allowed` folder in it and a target root `allowed/x` holding one folder `wt-a`.
    fn fixture(name: &str) -> (TempTree, PathBuf, PathBuf) {
        let (g, t) = temp_tree(name);
        let allowed = fs::canonicalize(&t).unwrap();
        let x = allowed.join("x");
        mk_dep(&x, "wt-a/debug/deps/a.rlib", 10);
        (g, allowed, x)
    }

    /// `run` as a dry run (so a wrongly accepted `--dir` could not remove anything), which must refuse.
    fn refused(dir: &Path, allowed: &Path) -> String {
        match run(dir, 1, 0, None, true, &guard(allowed)) {
            Err(e) => {
                assert!(e.contains("nothing was removed"), "{e}");
                e
            }
            Ok(c) => panic!("--dir {dir:?} was accepted (exit {c})"),
        }
    }

    #[test]
    fn an_empty_dir_is_refused() {
        let (_g, allowed, x) = fixture("rf-empty");
        assert!(refused(Path::new(""), &allowed).contains("empty"));
        assert!(x.join("wt-a").exists());
    }

    #[test]
    fn a_relative_dir_is_refused() {
        let (_g, allowed, x) = fixture("rf-rel");
        assert!(refused(Path::new("x"), &allowed).contains("absolute"));
        assert!(x.join("wt-a").exists());
    }

    #[test]
    fn a_dir_with_a_parent_component_is_refused() {
        let (_g, allowed, x) = fixture("rf-dotdot");
        assert!(refused(&allowed.join("x/../x"), &allowed).contains("`..`"));
        assert!(x.join("wt-a").exists());
    }

    #[test]
    fn a_dir_that_is_a_symlink_is_refused() {
        let (_g, allowed, x) = fixture("rf-link");
        let link = allowed.join("link");
        std::os::unix::fs::symlink(&x, &link).unwrap();
        assert!(refused(&link, &allowed).contains("symlink"));
        assert!(x.join("wt-a").exists());
    }

    #[test]
    fn a_dir_that_resolves_to_slash_is_refused() {
        let (_g, allowed, _x) = fixture("rf-slash");
        assert!(refused(Path::new("/"), &allowed).contains("`/`"));
    }

    #[test]
    fn a_dir_outside_the_allowed_root_is_refused() {
        let (_g, allowed, x) = fixture("rf-outside");
        let (_g2, other) = temp_tree("rf-outside-other");
        mk_dep(&other, "wt-b/deps/b", 10);
        assert!(refused(&other, &allowed).contains("not strictly under"));
        assert!(other.join("wt-b").exists() && x.join("wt-a").exists());
    }

    #[test]
    fn the_allowed_root_itself_is_refused() {
        let (_g, allowed, x) = fixture("rf-prefix");
        assert!(refused(&allowed, &allowed).contains("not strictly under"));
        assert!(x.join("wt-a").exists());
    }

    #[test]
    fn a_dir_that_reaches_out_through_a_symlinked_parent_is_refused() {
        let (_g, allowed, _x) = fixture("rf-esc");
        let (_g2, outside) = temp_tree("rf-esc-out");
        mk_dep(&outside, "x/wt-b/deps/b", 10);
        std::os::unix::fs::symlink(&outside, allowed.join("esc")).unwrap();
        assert!(refused(&allowed.join("esc/x"), &allowed).contains("not strictly under"));
        assert!(outside.join("x/wt-b").exists());
    }

    #[test]
    fn a_limit_of_zero_and_an_overflowing_limit_are_refused() {
        let (_g, allowed, x) = fixture("rf-limit");
        for (lim, to) in [(0, 0), (u64::MAX, 0), (1, u64::MAX), (1 << 34, 0)] {
            let e = run(&x, lim, to, None, true, &guard(&allowed)).unwrap_err();
            assert!(e.contains("nothing was removed"), "{lim} {to}: {e}");
        }
        assert!(x.join("wt-a").exists());
    }

    #[test]
    fn the_device_check_compares_st_dev() {
        assert!(crosses_device(1, 2));
        assert!(!crosses_device(7, 7));
        // A single filesystem cannot hold a mount point, so the walk is tested against a root device that nothing here has: every
        // folder inside then counts as being on another filesystem, and with the real device none does.
        let (_g, t) = temp_tree("devwalk");
        mk_dep(&t, "wt-a/debug/deps/a", 1);
        assert!(contains_foreign_dev(&t.join("wt-a"), dev(&t) ^ 1));
        assert!(!contains_foreign_dev(&t.join("wt-a"), dev(&t)));
        // And a candidate whose own device differs from the root's is skipped whole, and logged.
        let now = SystemTime::now();
        let cand = t.join("wt-a");
        stamp_all(&cand, now - std::time::Duration::from_secs(700));
        let out = sweep_candidate(
            &cand,
            1,
            now,
            false,
            no_procs(),
            &{
                let mut r = root_of(&t);
                r.dev ^= 1;
                r
            },
            &mut |_| {},
        );
        assert!(matches!(out, Sweep1::MountPoint), "{out:?}");
        assert_eq!(
            sweep_log(&cand, &out, false),
            format!("sweep-target: skipped {} (mount point)", cand.display())
        );
        assert!(cand.exists());
        assert_eq!(
            sweep_log(&cand, &Sweep1::ContainsMount, false),
            format!(
                "sweep-target: skipped {} (contains a mount point)",
                cand.display()
            )
        );
    }

    #[test]
    fn a_symlink_candidate_is_unlinked_and_its_target_is_left_whole() {
        let now = SystemTime::now();
        let (_g, t) = temp_tree("linkcand");
        let (_g2, target) = temp_tree("linkcand-target");
        mk_dep(&target, "deps/precious", 100);
        let link = t.join("wt-link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let out = sweep_candidate(&link, 1, now, false, no_procs(), &root_of(&t), &mut |_| {});
        assert!(matches!(out, Sweep1::Removed { .. }), "{out:?}");
        assert!(fs::symlink_metadata(&link).is_err(), "the link is gone");
        assert!(
            target.join("deps/precious").exists(),
            "what it pointed at is not touched"
        );
    }

    #[test]
    fn the_removing_line_comes_before_the_removal_and_names_the_canonical_path() {
        let now = SystemTime::now();
        let (_g, allowed, x) = fixture("rm-line");
        let wt = x.join("wt-old");
        let big = wt.join("debug/deps/big");
        fs::create_dir_all(big.parent().unwrap()).unwrap();
        // A sparse 2 GiB file: over the 1 GiB limit without writing a byte.
        fs::File::create(&big).unwrap().set_len(2 << 30).unwrap();
        stamp_all(&wt, now - std::time::Duration::from_secs(700));
        // `--dir` in a non-canonical but allowed form.
        let given = allowed.join(".").join("x");
        assert!(given.to_string_lossy().contains("/./"));
        let (mut lines, mut there) = (Vec::new(), None);
        let w = wt.clone();
        let rc = run_with(&given, 1, 0, None, false, &guard(&allowed), &mut |l| {
            if l.starts_with("sweep-target: removing ") {
                there = Some(w.exists());
            }
            lines.push(l.to_string());
        });
        assert_eq!(rc, Ok(0));
        assert_eq!(
            lines[0],
            format!("sweep-target: removing {}", wt.display()),
            "{lines:?}"
        );
        assert!(!lines[0].contains("/./"));
        assert_eq!(
            there,
            Some(true),
            "the line was written while the folder still existed"
        );
        assert!(lines[1].starts_with("sweep-target: removed "), "{lines:?}");
        assert!(!wt.exists());
        assert!(x.exists(), "only the candidate went");
    }

    // ---- --single: symlinked profiles and the caps ----

    #[test]
    fn a_symlinked_profile_is_skipped_and_nothing_is_touched_through_it() {
        let t = fake_target("symprof");
        let _g = TempTree(t.clone());
        let (_g2, outside) = temp_tree("symprof-out");
        art(&outside, "deps/keep.rlib", ONE, 100);
        art(&outside, "incremental/some/x", ONE, 100);
        fs::remove_dir_all(t.join("debug")).unwrap();
        std::os::unix::fs::symlink(&outside, t.join("debug")).unwrap();
        let scan = scan_profiles(&root_of(&t));
        assert_eq!(scan.symlinks, [t.join("debug")]);
        assert!(!scan.profiles.iter().any(|p| p.path == t.join("debug")));
        let rc = sweep_single(&t, 0, 0, false, &no_builds("symprof")).unwrap();
        assert_eq!(rc, 0);
        assert!(
            has(&outside, "deps/keep.rlib"),
            "nothing removed through the link"
        );
        assert!(has(&outside, "incremental/some/x"));
        assert!(
            !has(&outside, ".cargo-lock"),
            "no lock created through the link"
        );
        assert!(
            !has(&t, "release/build/some1-aaa"),
            "the real profile was swept"
        );
    }

    // ---- round 2: the root held as a descriptor, the device check inside the deletion, --single and the guard ----

    #[test]
    fn a_symlink_swapped_into_the_root_path_is_never_followed() {
        let (_g, allowed, x) = fixture("swap");
        let (_g2, outside) = temp_tree("swap-out");
        mk_dep(&outside, "wt-a/deps/precious", 10);
        mk_dep(&outside, "wt-b/deps/precious", 10);
        // The root is opened (as `run_with` does after `check_root`) ...
        let root = root_of(&x);
        // ... and then the path is swapped: the real folder moves away and a link to somebody else's tree takes its name.
        let moved = allowed.join("x-real");
        fs::rename(&x, &moved).unwrap();
        std::os::unix::fs::symlink(&outside, &x).unwrap();
        let r = remove_entry(&root, root.fd.as_raw_fd(), OsStr::new("wt-a")).unwrap();
        assert_eq!(r, Removed::Done);
        assert!(
            outside.join("wt-a/deps/precious").exists(),
            "what the swapped-in link points at is untouched"
        );
        assert!(
            !moved.join("wt-a").exists(),
            "the checked folder was the one removed"
        );
        // The same through the candidate removal: the rename and the delete use the descriptor, not the path string, which now
        // leads into somebody else's tree.
        mk_dep(&moved, "wt-c/deps/x", 10);
        let out = remove_candidate(&root, &x.join("wt-c"), 1, 0, false, &mut |_| {});
        assert!(matches!(out, Sweep1::Removed { .. }), "{out:?}");
        assert!(!moved.join("wt-c").exists());
        assert!(outside.join("wt-b/deps/precious").exists());
    }

    #[test]
    fn an_entry_swapped_for_a_symlink_is_unlinked_not_followed() {
        let (_g, _allowed, x) = fixture("swap-entry");
        let (_g2, outside) = temp_tree("swap-entry-out");
        mk_dep(&outside, "deps/precious", 10);
        std::os::unix::fs::symlink(&outside, x.join("wt-link")).unwrap();
        let root = root_of(&x);
        let r = remove_entry(&root, root.fd.as_raw_fd(), OsStr::new("wt-link")).unwrap();
        assert_eq!(r, Removed::Done);
        assert!(
            fs::symlink_metadata(x.join("wt-link")).is_err(),
            "the link is gone"
        );
        assert!(
            outside.join("deps/precious").exists(),
            "its target is not entered"
        );
    }

    #[test]
    fn the_deletion_stops_at_a_directory_on_another_filesystem() {
        let (_g, t) = temp_tree("devrm");
        mk_dep(&t, "wt-a/keep-me/f", 1);
        mk_dep(&t, "wt-a/mnt/theirs", 1);
        mk_dep(&t, "wt-a/sub/mnt/theirs", 1);
        mk_dep(&t, "wt-a/sub/gone", 1);
        mk_dep(&t, "wt-a/z", 1);
        // A unit test cannot mount, so a folder named `mnt` stands in for one: the walk's own predicate says it is foreign.
        let mut root = root_of(&t);
        root.fake_mount = Some(OsString::from("mnt"));
        let r = remove_entry(&root, root.fd.as_raw_fd(), OsStr::new("wt-a")).unwrap();
        assert_eq!(r, Removed::Mount);
        assert!(
            t.join("wt-a/mnt/theirs").exists(),
            "the foreign subtree is left whole"
        );
        assert!(t.join("wt-a/sub/mnt/theirs").exists());
        assert!(!t.join("wt-a/keep-me").exists(), "everything else went");
        assert!(!t.join("wt-a/z").exists());
        assert!(!t.join("wt-a/sub/gone").exists());
        // With the root's device wrong, every directory is foreign: the walk does not enter the first one.
        let (_g2, u) = temp_tree("devrm2");
        mk_dep(&u, "wt-b/deps/f", 1);
        let mut root = root_of(&u);
        root.dev ^= 1;
        assert_eq!(
            remove_entry(&root, root.fd.as_raw_fd(), OsStr::new("wt-b")).unwrap(),
            Removed::Mount
        );
        assert!(u.join("wt-b/deps/f").exists());
    }

    #[test]
    fn a_candidate_holding_a_foreign_directory_fails_with_the_reason_and_leaves_it() {
        let now = SystemTime::now();
        let (_g, t) = temp_tree("devcand");
        let cand = t.join("wt-m");
        mk_dep(&cand, "debug/deps/a", 1);
        mk_dep(&cand, "mnt/theirs", 1);
        stamp_all(&cand, now - std::time::Duration::from_secs(700));
        let mut root = root_of(&t);
        root.fake_mount = Some(OsString::from("mnt"));
        let out = sweep_candidate(&cand, 1, now, false, no_procs(), &root, &mut |_| {});
        assert!(
            matches!(&out, Sweep1::Failed(m) if m == "contains a mount point"),
            "{out:?}"
        );
        assert_eq!(
            sweep_log(&cand, &out, false),
            format!(
                "sweep-target: failed to remove {}: contains a mount point",
                cand.display()
            )
        );
        let trash = trash_name(OsStr::new("wt-m"));
        assert!(
            t.join(&trash).join("mnt/theirs").exists(),
            "left in the trash folder"
        );
        assert!(!t.join(&trash).join("debug").exists());
        // The next run's cleanup uses the same walk and says the same.
        let mut lines = Vec::new();
        clean_trash(&root, false, &mut |l| lines.push(l.to_string()));
        assert_eq!(
            lines.last().unwrap(),
            &format!(
                "sweep-target: failed to remove {}: contains a mount point",
                t.join(&trash).display()
            )
        );
        assert!(t.join(&trash).join("mnt/theirs").exists());
    }

    #[test]
    fn a_cmdline_argument_that_contains_the_target_after_an_equals_sign_names_it() {
        let real = Path::new("/c/wt-x");
        assert!(
            arg_names("/c/wt-x/debug/deps/a", real),
            "the whole argument"
        );
        assert!(arg_names("--target-dir=/c/wt-x/debug", real), "after `=`");
        assert!(arg_names("-C=link-arg=/c/wt-x", real), "after a later `=`");
        assert!(
            !arg_names("/c/wt-xy/debug", real),
            "a sibling with a longer name"
        );
        assert!(!arg_names("--target-dir=/c/other", real));
        assert!(!arg_names("wt-x", real));
        // Through the process scan: an unreadable own-uid process whose only mention is `--target-dir=<path>`.
        use std::os::unix::fs::PermissionsExt;
        let (_g, t) = temp_tree("eqarg");
        let x = t.join("wt-x");
        mk_dep(&x, "deps/a", 1);
        let procs = t.join("procs");
        let fd = procs.join("4003/fd");
        fs::create_dir_all(&fd).unwrap();
        fs::write(
            procs.join("4003/cmdline"),
            format!("rustc\0--target-dir={}/deps\0", x.display()),
        )
        .unwrap();
        let _restore = Restore(vec![fd.clone()]);
        fs::set_permissions(&fd, fs::Permissions::from_mode(0o000)).unwrap();
        assert_eq!(pid_using(&x, &procs), Some(4003));
    }

    #[test]
    fn a_dir_that_is_the_home_directory_is_refused() {
        // A fake HOME, so the rule is always exercised: the prefix is its parent, so only the home rule can refuse it.
        let (_g, allowed, _x) = fixture("rf-home");
        let home = allowed.join("home");
        fs::create_dir_all(home.join(".cache")).unwrap();
        let g = Guard {
            allowed: &allowed,
            home: Some(&home),
            procs: no_procs(),
        };
        let e = run(&home, 1, 0, None, true, &g).unwrap_err();
        assert!(
            e.contains("home directory") && e.contains("nothing was removed"),
            "{e}"
        );
    }

    #[test]
    fn an_allowed_prefix_of_slash_is_refused() {
        let (_g, allowed, x) = fixture("rf-slashprefix");
        let g = Guard {
            allowed: Path::new("/"),
            home: None,
            procs: no_procs(),
        };
        let e = run(&x, 1, 0, None, true, &g).unwrap_err();
        assert!(
            e.contains("allowed root is `/`") && e.contains("nothing was removed"),
            "{e}"
        );
        assert!(x.join("wt-a").exists());
        // The same refusal for --single.
        assert!(run_single(&allowed, 10, 5, true, &g).is_err());
    }

    #[test]
    fn a_single_cap_of_zero_kib_is_refused() {
        let (_g, allowed, x) = fixture("rf-kib0");
        let e = run_single(&x, 0, 0, true, &guard(&allowed)).unwrap_err();
        assert!(
            e.contains("cap of 0") && e.contains("nothing was removed"),
            "{e}"
        );
        assert!(x.join("wt-a").exists());
    }

    #[test]
    fn a_symlink_dir_under_single_is_refused_not_canonicalized_into_a_sweep() {
        let (_g, allowed, x) = fixture("rf-singlelink");
        let link = allowed.join("link");
        std::os::unix::fs::symlink(&x, &link).unwrap();
        let e = run_single(&link, 1, 0, false, &guard(&allowed)).unwrap_err();
        assert!(
            e.contains("symlink") && e.contains("nothing was removed"),
            "{e}"
        );
        assert!(x.join("wt-a/debug/deps/a.rlib").exists());
        // And a real folder outside the allowed root is refused by --single as well.
        let (_g2, other) = temp_tree("rf-singleout");
        mk_dep(&other, "debug/deps/b.rlib", 100);
        assert!(run_single(&other, 1, 0, false, &guard(&allowed)).is_err());
        assert!(other.join("debug/deps/b.rlib").exists());
    }

    #[test]
    fn a_symlinked_deps_incremental_or_build_is_never_listed_under_single() {
        let t = fake_target("symsub");
        let _g = TempTree(t.clone());
        let (_g2, outside) = temp_tree("symsub-out");
        art(&outside, "deps/theirs.rlib", ONE, 100);
        art(&outside, "inc/some/x", ONE, 100);
        art(&outside, "bld/some/y", ONE, 100);
        // debug/deps, release/incremental and release/build become links to folders of somebody else's.
        fs::remove_dir_all(t.join("debug/deps")).unwrap();
        std::os::unix::fs::symlink(outside.join("deps"), t.join("debug/deps")).unwrap();
        fs::remove_dir_all(t.join("release/incremental")).unwrap();
        std::os::unix::fs::symlink(outside.join("inc"), t.join("release/incremental")).unwrap();
        fs::remove_dir_all(t.join("release/build")).unwrap();
        std::os::unix::fs::symlink(outside.join("bld"), t.join("release/build")).unwrap();
        let listed: Vec<_> = single_artifacts(&scan_profiles(&root_of(&t)).profiles)
            .into_iter()
            .map(|e| e.path)
            .collect();
        for p in &listed {
            assert!(
                !p.starts_with(t.join("debug/deps")),
                "listed through a link: {p:?}"
            );
            assert!(!p.starts_with(t.join("release/incremental")), "{p:?}");
            assert!(!p.starts_with(t.join("release/build")), "{p:?}");
        }
        let rc = sweep_single(&t, 0, 0, false, &no_builds("symsub")).unwrap();
        assert_eq!(rc, 0);
        assert!(
            has(&outside, "deps/theirs.rlib"),
            "nothing removed through a deps link"
        );
        assert!(has(&outside, "inc/some/x"));
        assert!(has(&outside, "bld/some/y"));
        assert!(
            !has(&t, "aarch64-fakeos-gnu/debug/deps/triple.rlib"),
            "the real profile was swept"
        );
    }

    // ---- round 3: --single removes through the root fd as well ----

    #[test]
    fn a_single_sweep_leaves_a_directory_on_another_filesystem_and_says_so() {
        let t = fake_target("sdev");
        let _g = TempTree(t.clone());
        // A folder named `some2-bbb` stands in for a mount point (a unit test cannot mount): the walk's own predicate says foreign.
        let mut root = root_of(&t);
        root.fake_mount = Some(OsString::from("some2-bbb"));
        let rc = sweep_single_root(&root, 0, 0, false, &no_builds("sdev")).unwrap();
        assert_eq!(rc, 0);
        assert!(
            has(&t, "release/incremental/some2-bbb/in.lock"),
            "the foreign subtree is left whole"
        );
        assert!(
            !has(&t, "debug/deps/old.rlib"),
            "the rest of the sweep went on"
        );
        assert!(!has(&t, "release/build/some1-aaa"));
        // With the root's device wrong, every profile is on a "foreign" device: none is listed, so nothing is swept (round 4).
        let u = fake_target("sdev2");
        let _g2 = TempTree(u.clone());
        let mut root = root_of(&u);
        root.dev ^= 1;
        sweep_single_root(&root, 0, 0, false, &no_builds("sdev2")).unwrap();
        assert!(has(&u, "release/build/some1-aaa/output"));
        assert!(has(&u, "release/incremental/some2-bbb/in.lock"));
        assert!(
            has(&u, "debug/deps/old.rlib"),
            "a profile on a foreign device is not swept"
        );
    }

    #[test]
    fn a_single_sweep_never_follows_a_link_swapped_into_the_target_path() {
        let t = fake_target("sswap");
        let _g = TempTree(t.clone());
        let o = fake_target("sswap-out");
        let _g2 = TempTree(o.clone());
        // The target is opened (as `sweep_single` does after its checks) ...
        let root = root_of(&t);
        // ... and then its path is swapped for a link to a look-alike tree of somebody else's.
        let moved = PathBuf::from(format!("{}-real", t.display()));
        let _g3 = TempTree(moved.clone());
        fs::rename(&t, &moved).unwrap();
        std::os::unix::fs::symlink(&o, &t).unwrap();
        sweep_single_root(&root, 0, 0, false, &no_builds("sswap")).unwrap();
        for keep in [
            "debug/deps/old.rlib",
            "debug/deps/mid.rlib",
            "release/build/some1-aaa/output",
            "release/incremental/some2-bbb/in.lock",
            "aarch64-fakeos-gnu/debug/deps/triple.rlib",
        ] {
            assert!(has(&o, keep), "{keep} of the swapped-in tree must stay");
        }
        assert!(
            !has(&moved, "aarch64-fakeos-gnu/debug/deps/triple.rlib"),
            "the checked target was the one swept"
        );
        fs::remove_file(&t).unwrap();
    }

    // ---- round 4: --single reads and locks through the root fd ----

    #[test]
    fn a_cargo_lock_that_is_a_symlink_is_never_followed_or_created_through() {
        let t = fake_target("lcklink");
        let _g = TempTree(t.clone());
        let (_g2, outside) = temp_tree("lcklink-out");
        let victim = outside.join("victim");
        fs::write(&victim, "keep").unwrap();
        // debug/.cargo-lock becomes a link to somebody else's file: it is not opened, so there is no lock to take.
        fs::remove_file(t.join("debug/.cargo-lock")).unwrap();
        std::os::unix::fs::symlink(&victim, t.join("debug/.cargo-lock")).unwrap();
        assert!(
            lock_all(&t, false).is_none(),
            "a link is a refusal, not a lock"
        );
        assert!(
            lock_all(&t, true).is_none(),
            "the dry run does not follow it either"
        );
        assert_eq!(fs::read_to_string(&victim).unwrap(), "keep");
        assert_eq!(
            sweep_single(&t, 0, 0, false, &no_builds("lcklink")).unwrap(),
            3,
            "the target is left alone"
        );
        assert!(
            has(&t, "release/build/some1-aaa/output"),
            "nothing was removed"
        );
        // A dangling link where a lock would be created: nothing is created at its target.
        fs::remove_file(t.join("debug/.cargo-lock")).unwrap();
        fs::write(t.join("debug/.cargo-lock"), "").unwrap();
        let created = outside.join("created");
        fs::remove_file(t.join("release/.cargo-lock")).unwrap();
        std::os::unix::fs::symlink(&created, t.join("release/.cargo-lock")).unwrap();
        assert!(lock_all(&t, false).is_none());
        assert!(!created.exists(), "no file was created through the link");
    }

    #[test]
    fn a_profile_on_another_device_is_skipped_not_locked_and_not_swept() {
        let t = fake_target("pdev");
        let _g = TempTree(t.clone());
        // A folder named `release` stands in for a mount point (a unit test cannot mount): its device is "foreign".
        let mut root = root_of(&t);
        root.fake_mount = Some(OsString::from("release"));
        let scan = scan_profiles(&root);
        assert!(!scan.profiles.iter().any(|p| p.path == t.join("release")));
        assert!(scan.profiles.iter().any(|p| p.path == t.join("debug")));
        fs::remove_file(t.join("release/.cargo-lock")).unwrap();
        drop(scan);
        let rc = sweep_single_root(&root, 0, 0, false, &no_builds("pdev")).unwrap();
        assert_eq!(rc, 0);
        assert!(
            has(&t, "release/build/some1-aaa/output"),
            "the foreign profile is not swept"
        );
        assert!(has(&t, "release/incremental/some2-bbb/in.lock"));
        assert!(!has(&t, "release/.cargo-lock"), "no lock is created on it");
        assert!(!has(&t, "debug/deps/old.rlib"), "the real profile was");
        // With the root's own device wrong, every profile is foreign: nothing is listed, locked or removed.
        let u = fake_target("pdev2");
        let _g2 = TempTree(u.clone());
        let mut root = root_of(&u);
        root.dev ^= 1;
        assert!(scan_profiles(&root).profiles.is_empty());
        sweep_single_root(&root, 0, 0, false, &no_builds("pdev2")).unwrap();
        assert!(has(&u, "debug/deps/old.rlib"));
    }

    #[test]
    fn the_caps_are_checked_for_overflow() {
        assert_eq!(caps_kib_checked(40, 32), Some((41_943_040, 33_554_432)));
        assert_eq!(caps_kib_checked(u64::MAX, 1), None);
        assert_eq!(caps_kib_checked(1, u64::MAX), None);
        assert_eq!(
            caps_kib_checked(1 << 34, 1),
            None,
            "the byte product would wrap"
        );
        let (_g, t) = temp_tree("kibcap");
        assert!(run_single(&t, u64::MAX, 0, true, &guard(&t)).is_err());
    }

    // ---- one target dir (--single) ----

    /// `--limit-gib N` reaches `run_single` as the KiB of N GiB, not of N MiB: one 1024 was the unit bug.
    #[test]
    fn a_gib_cap_is_the_kib_of_whole_gib() {
        assert_eq!(caps_kib(40, 32), (41_943_040, 33_554_432));
        // The default `--to-gib` (80% of the limit) converts exactly like a given one.
        let limit = 8;
        assert_eq!(caps_kib(limit, limit * 4 / 5), (8_388_608, 6_291_456));
        // The bytes `run_single` hands `sweep_single` are the cap the flag names, no more and no less.
        let (lim, to) = caps_kib(40, 32);
        assert_eq!((lim * 1024, to * 1024), (40u64 << 30, 32u64 << 30));
    }

    #[test]
    fn a_cap_of_a_thousand_gib_stays_inside_u64() {
        assert_eq!(caps_kib(1000, 800), (1_048_576_000, 838_860_800));
        assert_eq!(caps_kib(1000, 800).0 * 1024, 1000u64 << 30);
    }

    const KIB: usize = 1 << 10;
    /// One artifact of the fake target below: 32 KiB, so a KiB mark is exact.
    const ONE: usize = 32 * KIB;

    fn kib(n: usize) -> u64 {
        (n * KIB) as u64
    }

    /// A file of `bytes` bytes at the target-relative path `rel`, last used `ago` seconds after the epoch. Both its times are set,
    /// so the LRU order here does not depend on the mount options.
    fn art(t: &Path, rel: &str, bytes: usize, ago: u64) {
        let p = t.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, vec![7u8; bytes]).unwrap();
        let when = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(ago);
        let f = fs::OpenOptions::new().write(true).open(&p).unwrap();
        f.set_times(fs::FileTimes::new().set_modified(when).set_accessed(when))
            .unwrap();
    }

    fn has(t: &Path, rel: &str) -> bool {
        t.join(rel).exists()
    }

    /// Keeps trying `take` for up to one second. A lock we released keeps reading as held for a few milliseconds now and then:
    /// other tests in this same test binary fork children, and a child forked while we held the lock carries an inherited copy
    /// of that fd until it execs — the kernel counts the lock as held for the description, and reports it under our pid. The
    /// wait is bounded, so a lock that really stays held still fails the test.
    fn lock_frees_up(take: &mut dyn FnMut() -> bool) -> bool {
        for _ in 0..50 {
            if take() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        false
    }

    /// A process table with nobody in it: the sweep is then down to Cargo's locks alone.
    fn no_builds(name: &str) -> PathBuf {
        let p =
            std::env::temp_dir().join(format!("lucina-sweep-procs-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    /// One process of a fake process table: a folder of its pid with `comm`, its NUL-separated environment, and a link per open
    /// file under `fd/`. Where a link points need not exist: the sweep only asks where it points.
    fn fake_proc(procs: &Path, pid: &str, comm: &str, env: &[&str], open: &[PathBuf]) {
        let p = procs.join(pid);
        fs::create_dir_all(p.join("fd")).unwrap();
        fs::write(p.join("comm"), format!("{comm}\n")).unwrap();
        fs::write(p.join("environ"), env.join("\0")).unwrap();
        for (i, target) in open.iter().enumerate() {
            std::os::unix::fs::symlink(target, p.join("fd").join(i.to_string())).unwrap();
        }
    }

    /// A target as Cargo lays it out: its binaries and Cargo's own lock in each profile folder, the artifacts in `deps/`,
    /// `incremental/` and `build/`, and the same again under a target-triple folder. Eight 32 KiB files: 256 KiB.
    fn fake_target(name: &str) -> PathBuf {
        let t =
            std::env::temp_dir().join(format!("lucina-sweep-single-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&t);
        art(&t, "debug/mybinary", ONE, 500); // a binary, directly in the profile folder
        art(&t, "debug/deps/old.rlib", ONE, 100);
        art(&t, "debug/deps/mid.rlib", ONE, 200);
        art(&t, "debug/deps/new.rlib", ONE, 900);
        art(&t, "debug/deps/note.d", ONE, 300);
        art(&t, "release/build/some1-aaa/output", ONE, 400);
        art(&t, "release/incremental/some2-bbb/in.lock", ONE, 600);
        art(&t, "aarch64-fakeos-gnu/debug/deps/triple.rlib", ONE, 50);
        // Cargo's lock, one per profile folder; the triple folder has none yet, as on a target only built by hand.
        fs::write(t.join("debug/.cargo-lock"), "").unwrap();
        fs::write(t.join("release/.cargo-lock"), "").unwrap();
        t
    }

    #[test]
    fn one_target_sweeps_its_artifacts_least_recently_used_first_down_to_the_mark() {
        let t = fake_target("lru");
        // 256 KiB, over 224 sweep down to 192: exactly the two oldest artifacts go, one of them from the target triple.
        let rc = sweep_single(&t, kib(224), kib(192), false, &no_builds("lru")).unwrap();
        assert_eq!(rc, 0, "nothing was in use");
        for gone in [
            "aarch64-fakeos-gnu/debug/deps/triple.rlib",
            "debug/deps/old.rlib",
        ] {
            assert!(!has(&t, gone), "{gone} should be gone");
        }
        for keep in [
            "debug/deps/mid.rlib",
            "debug/deps/note.d",
            "debug/deps/new.rlib",
            "debug/mybinary",
            "release/build/some1-aaa",
            "release/incremental/some2-bbb",
        ] {
            assert!(has(&t, keep), "{keep} should stay");
        }
        fs::remove_dir_all(&t).ok();
    }

    #[test]
    fn one_target_sweep_keeps_the_binaries_and_touches_nothing_else_in_the_profile_folder() {
        let t = fake_target("bins");
        // Down to nothing: everything removable goes, the rest of the target stays.
        assert_eq!(
            sweep_single(&t, 0, 0, false, &no_builds("bins")).unwrap(),
            0,
            "nothing was in use"
        );
        assert!(has(&t, "debug/mybinary"), "a binary is never removed");
        assert!(
            has(&t, "debug/.cargo-lock"),
            "the lock stays in the profile folder"
        );
        assert!(has(&t, "release/.cargo-lock"));
        assert!(has(&t, "aarch64-fakeos-gnu/debug/deps"), "the folder stays");
        for gone in [
            "debug/deps/old.rlib",
            "debug/deps/new.rlib",
            "release/build/some1-aaa",
            "release/incremental/some2-bbb",
            "aarch64-fakeos-gnu/debug/deps/triple.rlib",
        ] {
            assert!(!has(&t, gone), "{gone} should be gone");
        }
        fs::remove_dir_all(&t).ok();
    }

    #[test]
    fn one_target_sweep_does_nothing_while_a_build_holds_a_lock() {
        let t = fake_target("lock");
        // A lock held in ONE profile folder is enough: half a target cleaned under a running build is worse than a dirty one.
        let held = fs::OpenOptions::new()
            .write(true)
            .open(t.join("release/.cargo-lock"))
            .unwrap();
        held.lock().unwrap();
        assert!(lock_all(&t, false).is_none());
        assert_eq!(
            sweep_single(&t, 0, 0, false, &no_builds("lock")).unwrap(),
            3,
            "in use: not swept"
        );
        for keep in [
            "debug/deps/old.rlib",
            "debug/mybinary",
            "release/build/some1-aaa",
        ] {
            assert!(has(&t, keep), "{keep} should be untouched");
        }
        drop(held);
        assert!(
            lock_frees_up(&mut || lock_all(&t, false).is_some()),
            "the profile locks we released are lockable again"
        );
        fs::remove_dir_all(&t).ok();
    }

    #[test]
    fn a_dry_run_of_one_target_removes_nothing_and_creates_no_lock() {
        let t = fake_target("dry");
        let missing = "aarch64-fakeos-gnu/debug/.cargo-lock";
        assert!(!has(&t, missing), "the fake target has no lock there yet");
        assert_eq!(sweep_single(&t, 0, 0, true, &no_builds("dry")).unwrap(), 0);
        for keep in [
            "debug/deps/old.rlib",
            "debug/deps/mid.rlib",
            "debug/mybinary",
            "release/build/some1-aaa/output",
            "release/incremental/some2-bbb/in.lock",
            "aarch64-fakeos-gnu/debug/deps/triple.rlib",
        ] {
            assert!(has(&t, keep), "{keep} should still be there");
        }
        assert!(!has(&t, missing), "a dry run creates nothing");
        fs::remove_dir_all(&t).ok();
    }

    // ---- a build that takes the target without holding its lock ----

    #[test]
    fn a_build_that_names_the_target_in_cargo_target_dir_stops_the_sweep() {
        let t = fake_target("procenv");
        let procs = no_builds("procenv");
        let real = fs::canonicalize(&t).unwrap();
        fake_proc(
            &procs,
            "4242",
            "cargo",
            &[
                "PATH=/usr/bin",
                &format!("CARGO_TARGET_DIR={}", real.display()),
            ],
            &[],
        );
        assert_eq!(
            sweep_single(&t, 0, 0, false, &procs).unwrap(),
            3,
            "in use: not swept"
        );
        assert!(
            has(&t, "debug/deps/old.rlib"),
            "a running build keeps its artifacts"
        );
        assert!(
            has(&t, "release/incremental/some2-bbb"),
            "and its incremental folder"
        );
        fs::remove_dir_all(&t).ok();
        fs::remove_dir_all(&procs).ok();
    }

    #[test]
    fn a_rustc_with_a_file_open_in_the_target_stops_the_sweep() {
        let t = fake_target("procfd");
        let procs = no_builds("procfd");
        let real = fs::canonicalize(&t).unwrap();
        // A hand-run rustc: no lock taken and nothing in its environment, but its output is open in the target.
        fake_proc(
            &procs,
            "4343",
            "rustc",
            &["PATH=/usr/bin"],
            &[real.join("debug/deps/mid.rlib")],
        );
        assert_eq!(
            sweep_single(&t, 0, 0, false, &procs).unwrap(),
            3,
            "in use: not swept"
        );
        assert!(
            has(&t, "debug/deps/mid.rlib"),
            "a running rustc keeps its objects"
        );
        fs::remove_dir_all(&t).ok();
        fs::remove_dir_all(&procs).ok();
    }

    #[test]
    fn a_shell_that_only_exports_the_variable_does_not_hold_the_cap_open() {
        let t = fake_target("procshell");
        let procs = no_builds("procshell");
        let real = fs::canonicalize(&t).unwrap();
        // Anyone can export CARGO_TARGET_DIR in a shell and leave it there; nothing of ours is open, so this is not a build.
        fake_proc(
            &procs,
            "4444",
            "bash",
            &[
                "PATH=/usr/bin",
                &format!("CARGO_TARGET_DIR={}", real.display()),
            ],
            &[real.parent().unwrap().to_path_buf()],
        );
        assert_eq!(sweep_single(&t, 0, 0, false, &procs).unwrap(), 0);
        assert!(!has(&t, "debug/deps/old.rlib"), "the sweep ran");
        fs::remove_dir_all(&t).ok();
        fs::remove_dir_all(&procs).ok();
    }

    #[test]
    fn a_build_of_another_target_does_not_stop_the_sweep() {
        let t = fake_target("procother");
        let procs = no_builds("procother");
        // The check cache of a sibling worktree: a busy build, but not in this folder.
        let other = no_builds("procother-target");
        fake_proc(
            &procs,
            "4545",
            "cargo",
            &[
                "PATH=/usr/bin",
                &format!("CARGO_TARGET_DIR={}", other.display()),
            ],
            &[other.join("debug/deps/x.rlib")],
        );
        assert_eq!(sweep_single(&t, 0, 0, false, &procs).unwrap(), 0);
        assert!(!has(&t, "debug/deps/old.rlib"), "the sweep ran");
        fs::remove_dir_all(&t).ok();
        fs::remove_dir_all(&procs).ok();
        fs::remove_dir_all(&other).ok();
    }
}
