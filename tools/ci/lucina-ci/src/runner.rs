//! Runs one proof unit in its own cgroup v2 scope. The outer process (outside the cap) starts `systemd-run --scope`; the inner
//! process, inside the scope, checks that the cap really applies, runs the unit and reads the scope's own counters. A step that
//! cannot get a scope runs as the leader of its own process group instead, and its deadline kill then takes the group and
//! nothing else (GATE-03).

use crate::verdict::{self, Facts, Outcome};
use std::fs;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::thread::sleep;
use std::time::{Duration, Instant};

/// Verus rlimit for one unit by name, and for no other: the unit and the rlimit value it is raised to. Every entry is a
/// raised solver resource limit that `tools/ci/trust-allow.tsv` records with a reason and that `trust` counts as a
/// `--rlimit` site; every other unit gets the Verus default. The value is a number, so no other Verus argument can be
/// written into this table.
pub(crate) const VERUS_ARGS: &[(&str, u32)] = &[
    // `step` in services/lucina-surface-rules/src/rules.rs: path and case splitting over its 13 arms.
    ("verus/juno-surface/rules", 20),
];

/// The extra arguments the verus unit `unit` is run with, as written for the shell: the table's rlimit rendered as the one
/// flag below, and nothing for a unit the table does not name. `--rlimit` is written in `rlimit_arg` and nowhere else, so
/// this is the one place that turns a table entry into an argument.
pub fn verus_args(unit: &str) -> String {
    VERUS_ARGS
        .iter()
        .find(|(name, _)| *name == unit)
        .map_or_else(String::new, |(_, limit)| rlimit_arg(*limit))
}

fn rlimit_arg(limit: u32) -> String {
    format!("--rlimit {limit}")
}

const GIB: u64 = 1 << 30;
/// Left to the system on top of the verification budget.
const RESERVE: u64 = 2 * GIB;
const SMALL_RESERVE: u64 = 3 * GIB / 2;
/// Below this the runner is about to run out of disk, which fails in confusing ways.
const MIN_DISK_FREE: u64 = GIB;

#[derive(Clone, Debug)]
pub struct Budget {
    pub cap_bytes: u64,
    pub class: &'static str,
    pub mem_total: u64,
}

pub fn mem_total() -> Result<u64, String> {
    let t = fs::read_to_string("/proc/meminfo").map_err(|e| e.to_string())?;
    let line = t
        .lines()
        .find(|l| l.starts_with("MemTotal:"))
        .ok_or("no MemTotal")?;
    let kb: u64 = line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or("bad MemTotal")?;
    Ok(kb * 1024)
}

/// The cap for this machine, never above what it has. A runner smaller than the budget is an error with what to do about it,
/// unless a smaller cap was asked for by name; then every report says so.
pub fn budget(cap_gib: u64, small_gib: Option<u64>, total: u64) -> Result<Budget, String> {
    let want = cap_gib * GIB;
    if total >= want + RESERVE {
        return Ok(Budget {
            cap_bytes: want,
            class: "standard",
            mem_total: total,
        });
    }
    if let Some(s) = small_gib {
        let cap = s * GIB;
        if total >= cap + SMALL_RESERVE {
            return Ok(Budget {
                cap_bytes: cap,
                class: "small-runner",
                mem_total: total,
            });
        }
    }
    Err(format!(
        "this runner has {:.1} GiB of RAM; a {cap_gib} GiB cap needs at least {} GiB. Private repositories get 8 GB GitHub runners, public ones 16 GB. \
Either run on a public repository, use a larger runner, or set the repository variable VERUS_SMALL_RUNNER_CAP_GIB to a cap this runner can hold \
(reports then record the smaller cap).",
        total as f64 / GIB as f64,
        cap_gib + 2
    ))
}

pub struct Toolchain {
    pub verus: String,
    pub version: String,
    pub verus_sha: String,
    pub verifier_sha: String,
    pub rust: String,
    pub z3: String,
}

fn capture(cmd: &mut Command) -> Option<String> {
    let o = cmd.stderr(Stdio::piped()).output().ok()?;
    Some(format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    ))
}

fn sha256(p: &str) -> String {
    capture(Command::new("sha256sum").arg(p))
        .and_then(|s| s.split_whitespace().next().map(str::to_string))
        .unwrap_or_default()
}

fn lock_value(lock: &str, key: &str) -> Option<String> {
    let mut in_verus = false;
    for l in lock.lines() {
        let t = l.trim();
        if t.starts_with('[') {
            in_verus = t == "[verus]";
        } else if in_verus && t.starts_with(key) {
            let v = t.split_once('=')?.1.trim();
            return v.strip_prefix('"')?.split('"').next().map(str::to_string);
        }
    }
    None
}

/// The verifier must be the one `references/upstream.lock` pins, launcher and rust_verify both, as in scripts/verify.sh.
pub fn check_toolchain(root: &Path, verus: &str) -> Result<Toolchain, String> {
    let lock =
        fs::read_to_string(root.join("references/upstream.lock")).map_err(|e| e.to_string())?;
    let release = lock_value(&lock, "release").ok_or("upstream.lock: no [verus] release")?;
    let pin_bin = lock_value(&lock, "binary_sha256").ok_or("upstream.lock: no binary_sha256")?;
    let pin_ver =
        lock_value(&lock, "verifier_sha256").ok_or("upstream.lock: no verifier_sha256")?;
    let out = capture(Command::new(verus).arg("--version"))
        .ok_or_else(|| format!("cannot run {verus}"))?;
    let field = |k: &str| {
        out.lines()
            .find_map(|l| l.trim().strip_prefix(k).map(|v| v.trim().to_string()))
            .unwrap_or_default()
    };
    let (version, rust) = (field("Version:"), field("Toolchain:"));
    let dir = Path::new(verus)
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let verus_sha = sha256(verus);
    let verifier_sha = sha256(&format!("{dir}/rust_verify"));
    if version != release || verus_sha != pin_bin || verifier_sha != pin_ver {
        return Err(format!(
            "{verus} is {version} ({verus_sha}, rust_verify {verifier_sha}); upstream.lock pins {release} ({pin_bin}, {pin_ver})"
        ));
    }
    let z3 = capture(Command::new(format!("{dir}/z3")).arg("--version"))
        .unwrap_or_default()
        .trim()
        .to_string();
    Ok(Toolchain {
        verus: verus.to_string(),
        version,
        verus_sha,
        verifier_sha,
        rust,
        z3,
    })
}

fn disk_free(path: &Path) -> Option<u64> {
    let o = Command::new("df")
        .args(["--output=avail", "-B1"])
        .arg(path)
        .output()
        .ok()?;
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .nth(1)?
        .trim()
        .parse()
        .ok()
}

fn read_trim(p: &Path) -> Option<String> {
    fs::read_to_string(p).ok().map(|s| s.trim().to_string())
}

static STEP_EXE: OnceLock<PathBuf> = OnceLock::new();

/// Says which binary the steps of this run start from. `cmd_check_fast` sets it to the copy it made in the run's own
/// directory, and the copy is what a step re-starts: where `current_exe()` is asked instead, a `check-fast` that
/// was started straight off the built binary loses its steps to any rebuild that replaces that file while it runs.
/// Unset leaves `current_exe()`, which is right for every command but `check-fast`.
pub fn install_step_exe(path: PathBuf) {
    let _ = STEP_EXE.set(path);
}

fn step_exe() -> Result<PathBuf, String> {
    STEP_EXE
        .get()
        .cloned()
        .or_else(|| std::env::current_exe().ok())
        .ok_or_else(|| "cannot find this program's own executable".to_string())
}

/// The unit name `run_unit` asks systemd for, so the cgroup whose directory starts with this is one this launcher made
/// for this step and may signal by pid. Anything else is a cgroup the check shares with whatever started it.
const SCOPE_OUR_PREFIX: &str = "lucina-ci-";
/// The grace a step gets to leave on its own after the deadline's SIGTERM, before everything of it is SIGKILLed. One named
/// constant because it is the whole of what the runner ever *waits* beside a limit: the wait ends at
/// `limit + KILL_GRACE_S + KILL_SLACK_S` and never at the step's own length (GATE-03: a limit that does not stop the step
/// is not a limit). The time the gate prints for the step is a little more than that, because the clock the check stamps
/// starts before the scope is asked for and stops after the last round of the kill: `runner_kill_scope.rs` allows 4 s of
/// that, which is what it measures here.
pub const KILL_GRACE_S: u64 = 5;
/// How long the SIGKILL rounds keep coming after the grace before the step is reported anyway. Nothing is waited on past
/// this bound: at it the step is reported as past its limit with whatever of it was still there counted in `detail`.
const KILL_SLACK_S: u64 = 2;
/// How often the wait loop looks at the step, at its counters and at its clock.
const TICK: Duration = Duration::from_millis(250);

fn own_cgroup() -> Result<PathBuf, String> {
    let t = fs::read_to_string("/proc/self/cgroup").map_err(|e| e.to_string())?;
    let line = t
        .lines()
        .find(|l| l.starts_with("0::"))
        .ok_or("no cgroup v2 line in /proc/self/cgroup")?;
    Ok(PathBuf::from(format!("/sys/fs/cgroup{}", &line[3..])))
}

fn oom_kills(cg: &Path) -> u64 {
    let t = fs::read_to_string(cg.join("memory.events")).unwrap_or_default();
    t.lines()
        .filter_map(|l| l.strip_prefix("oom_kill "))
        .filter_map(|n| n.parse::<u64>().ok())
        .sum()
}

/// This process's parent and its own process group, read from `/proc` because forking `ps` is the one thing a step that
/// filled its own `TasksMax` cannot do. The deadline kill never aims at either. Kept after the first read: neither can
/// change while this process lives, and a SIGKILL round screens a pid list of them.
fn own_ids() -> Option<(u32, u32)> {
    static IDS: OnceLock<Option<(u32, u32)>> = OnceLock::new();
    *IDS.get_or_init(|| {
        let t = fs::read_to_string("/proc/self/stat").ok()?;
        // `pid (comm) state ppid pgid ...`: `comm` holds spaces and parentheses, so the fields count from after the last `)`.
        let mut f = t.rsplit(") ").next()?.split_whitespace();
        let _state = f.next()?;
        let ppid = f.next()?.parse().ok()?;
        let pgid = f.next()?.parse().ok()?;
        Some((ppid, pgid))
    })
}

fn own_pgid() -> Option<u32> {
    own_ids().map(|(_, pgid)| pgid)
}

/// The process that started this one — the launcher, or `systemd-run`, which shares this process's group anyway.
fn own_ppid() -> Option<u32> {
    own_ids().map(|(ppid, _)| ppid)
}

/// Whether the deadline kill may aim at `target`. Never: pid 0, which `kill` reads as *this* process's group; pid 1, init;
/// a negative target, which is a whole group and only ever built from a pid that passed here; this process, which has a
/// report to write; this process's group, which holds the check and the other steps (the launcher we were started by is in
/// it too); and this process's parent. Everything else the step's group or cgroup tree names is the step's own.
fn signalable(target: u32) -> bool {
    target > 1
        && target != std::process::id()
        && Some(target) != own_pgid()
        && Some(target) != own_ppid()
}

/// What one round of the deadline kill could not deliver. Every signal of every round goes through here, because a kill
/// whose failure is thrown away is a kill that looks delivered: that is exactly how job_ecf8a642f3c8's 180 s limit became
/// 310.1 s of waiting on a step nothing had stopped (GATE-03, and PF-R024 §6 on the `let _ =` this replaces).
#[derive(Default)]
struct Miss {
    processes: u32,
    why: Option<String>,
    cgroups: u32,
    cgroup_why: Option<String>,
}

impl Miss {
    fn is_empty(&self) -> bool {
        self.processes == 0 && self.cgroups == 0
    }

    fn add_process(&mut self, why: String) {
        self.processes += 1;
        self.why.get_or_insert(why);
    }

    fn add_cgroup(&mut self, n: u32, why: Option<String>) {
        self.cgroups += n;
        if let Some(w) = why {
            self.cgroup_why.get_or_insert(w);
        }
    }

    /// The clause the step's failure line ends with: empty when the round reached everything, which is the normal case and
    /// the only one worth a line of its own when it is not.
    fn line(&self) -> String {
        let mut parts = Vec::new();
        if self.processes > 0 {
            parts.push(format!(
                "the deadline kill could not reach {} processes: {}",
                self.processes,
                self.why.clone().unwrap_or_default()
            ));
        }
        if self.cgroups > 0 {
            parts.push(format!(
                "{} of the step's `cgroup.kill` writes were refused: {}",
                self.cgroups,
                self.cgroup_why.clone().unwrap_or_default()
            ));
        }
        parts.join("; ")
    }
}

/// One `kill(2)`: the signal goes straight to the kernel, so a step that has filled its scope's `TasksMax=256` cannot stop
/// it by leaving no free task slot for `/bin/kill` to be forked into. `Ok` also covers a target that is already gone
/// (`ESRCH`): there is nothing left to reach. Any other errno is a delivery that did not happen and is reported.
fn kill_now(target: i32, sig: i32) -> Result<(), String> {
    debug_assert!(
        signalable(target.unsigned_abs()),
        "the deadline kill never aims at pid 0, 1 or -1, nor at this process, its own group or its parent"
    );
    // SAFETY: `kill(2)` takes two plain integers (a pid, or the negation of a process group id, and a signal number) and
    // touches no memory of the caller, so there is nothing to alias, dereference or unalign. `target` comes from
    // `signalable`, which refuses 0, 1, this pid, this group and this parent, and `sig` is a libc signal constant.
    if unsafe { libc::kill(target, sig) } == 0 {
        return Ok(());
    }
    let e = std::io::Error::last_os_error();
    if e.raw_os_error() == Some(libc::ESRCH) {
        return Ok(());
    }
    Err(e.to_string())
}

/// Every pid in `cg` and in every cgroup under it, without this process. A step's own child can `mkdir` inside its
/// delegated scope and move a grandchild into it (measured on buildhost), which drops that grandchild from the scope's own
/// `cgroup.procs` for good: the walk has to descend.
fn subtree_pids(cg: &Path) -> Vec<u32> {
    let mut out = Vec::new();
    let mut dirs = vec![cg.to_path_buf()];
    while let Some(d) = dirs.pop() {
        out.extend(
            fs::read_to_string(d.join("cgroup.procs"))
                .unwrap_or_default()
                .lines()
                .filter_map(|p| p.trim().parse::<u32>().ok()),
        );
        if let Ok(rd) = fs::read_dir(&d) {
            dirs.extend(rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()));
        }
    }
    out.retain(|p| signalable(*p));
    out
}

/// SIGKILL of a whole subtree, through `cgroup.kill`: no fork, no pid list, nothing to miss, and every thread of every
/// process rather than only the leaders a pid names. It is written only to cgroups UNDER the step's, never to the step's
/// own: that file kills every process of the cgroup, this launcher among them, and the launcher has a report to write.
/// Returns how many of those writes the kernel refused and the first refusal's reason, because a scope that is not
/// delegated to this user refuses them all and the round then reached nothing (PF-R024 §1, last paragraph).
fn kill_subtrees(cg: &Path, miss: &mut Miss) {
    let mut dirs = vec![cg.to_path_buf()];
    while let Some(d) = dirs.pop() {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for p in rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()) {
            let f = p.join("cgroup.kill");
            if let Err(e) = fs::write(&f, b"1\n") {
                miss.add_cgroup(1, Some(format!("{e} ({})", f.display())));
            }
            dirs.push(p);
        }
    }
}

/// The name of the one child cgroup `gather_into_kill` moves the step's processes into. A dot would keep systemd from
/// reading it as a unit of its own, but a hidden name is a name a human never finds in an `ls`, so it is written out.
const KILL_PEN: &str = "killed-at-the-deadline";

/// SIGKILL of the step's own cgroup without forking anything: move every process of it except this one into one child
/// cgroup and write `cgroup.kill` there. This is how the deadline still bites when the step has filled its own `TasksMax`
/// and `/bin/kill` cannot be started at all — measured 2026-11-18: with `TasksMax=256` in force (check-fast sets
/// `LUCINA_CI_CPU_QUOTA`, which asks for it) an 8 s limit recorded 15.9 s, because the old kill discarded the failed fork
/// and the loop then only waited. `Err(reason)` when the machine will not allow it — a scope not delegated to this user, a
/// kernel without `cgroup.kill` — which is reported, not swallowed: those are exactly the cases where the signals by group
/// and by pid are all there is, and an operator reading the failure line should see that the cgroup route was refused.
fn gather_into_kill(cg: &Path) -> Result<(), String> {
    let me = std::process::id().to_string();
    let procs = fs::read_to_string(cg.join("cgroup.procs")).unwrap_or_default();
    let others: Vec<&str> = procs.lines().filter(|p| *p != me).collect();
    let pen = cg.join(KILL_PEN);
    if others.is_empty() && !pen.exists() {
        // Nothing to gather, and no cgroup made on the way: a step that ends inside its deadline leaves the scope exactly
        // as it found it.
        return Ok(());
    }
    // The pen is made once and used by every round after that, so "it is already there" is not a refusal.
    let made = fs::create_dir(&pen).or_else(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            Ok(())
        } else {
            Err(e)
        }
    });
    if let Err(e) = made.and_then(|()| {
        for p in others {
            // `cgroup.kill` of the pen does not reach a process that will not be moved, so one that refuses to move is left
            // in the scope for the pid signal of the caller to take.
            let _ = fs::write(pen.join("cgroup.procs"), format!("{p}\n"));
        }
        fs::write(pen.join("cgroup.kill"), b"1\n")
    }) {
        return Err(format!("{e} ({})", pen.display()));
    }
    Ok(())
}

/// Signal the process group `pgid` and every pid of the step's tree with `sig`, one `kill(2)` each: no fork, so the round
/// lands inside a scope that has no task slot left, and every refusal is counted in `miss` instead of discarded. The group
/// goes first and on its own: it is the only thing that reaches a process the step handed to a nested
/// `systemd-run --scope`, which lands in a SIBLING scope outside this cgroup tree (measured on buildhost) and is therefore
/// invisible to every pid list.
fn signal(pgid: u32, pids: &[u32], sig: i32, miss: &mut Miss) {
    // The group only ever as a negative target, and only after the screen: `kill(0)` is *this* process's group, which
    // is the check itself, and `signalable` is what keeps 0, 1, self, self's group and the parent out.
    if let Ok(g) = i32::try_from(pgid)
        && signalable(pgid)
        && let Err(e) = kill_now(-g, sig)
    {
        miss.add_process(format!("{e} (the group of {pgid})"));
    }
    for p in pids {
        if let Ok(pid) = i32::try_from(*p)
            && signalable(*p)
            && let Err(e) = kill_now(pid, sig)
        {
            miss.add_process(format!("{e} (pid {p})"));
        }
    }
}

/// Where one step's processes are, and the two ways to reach them. `cg` is the step's own scope and `None` when the step
/// got no scope: a scope-less launcher stands in the cgroup of whatever started the check — the session, the agent, the
/// other steps — so the group is then the only thing this step owns and the only thing the kill may aim at (GATE-03
/// Part 1b: the old kill read the launcher's own cgroup, and everything else in it would have been SIGKILLed).
struct Step {
    cg: Option<PathBuf>,
    pgid: u32,
}

impl Step {
    /// The pids of the step that this process may signal — the whole tree, less everything `signalable` refuses.
    fn pids(&self) -> Vec<u32> {
        self.cg.as_ref().map_or_else(Vec::new, |c| subtree_pids(c))
    }

    /// The deadline's first blow: leave the step the chance to clean up after itself.
    fn terminate(&self) -> Miss {
        let mut miss = Miss::default();
        signal(self.pgid, &self.pids(), libc::SIGTERM, &mut miss);
        miss
    }

    /// The deadline's second blow and every round after it: the group, the pids, and the whole cgroup tree of the step.
    /// The group goes first because it is the only one of these that reaches a process the step handed to a nested
    /// `systemd-run --scope`, which lives in a SIBLING cgroup and answers to no signal of ours that walks the tree. None of
    /// it forks: the old kill was `/bin/kill` (a task slot the step had none of) whose failure was discarded (`let _ =`),
    /// which is how an 8 s limit measured 15.9 s here and job_ecf8a642f3c8's 180 s limit measured 310.1 s. What a round
    /// could not deliver comes back in the `Miss` and is reported.
    fn kill(&self) -> Miss {
        let mut miss = Miss::default();
        signal(self.pgid, &self.pids(), libc::SIGKILL, &mut miss);
        if let Some(cg) = &self.cg {
            kill_subtrees(cg, &mut miss);
            if let Err(e) = gather_into_kill(cg) {
                miss.add_cgroup(1, Some(e));
            }
        }
        miss
    }

    /// The scope's counters, read once.
    fn current(&self) -> Option<u64> {
        self.cg
            .as_ref()
            .and_then(|c| read_trim(&c.join("memory.current")))
            .and_then(|s| s.parse().ok())
    }

    fn peak_file(&self) -> Option<u64> {
        self.cg
            .as_ref()
            .and_then(|c| read_trim(&c.join("memory.peak")))
            .and_then(|s| s.parse().ok())
    }

    fn oom_kills(&self) -> u64 {
        self.cg.as_deref().map(oom_kills).unwrap_or_default()
    }
}

pub fn json_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o
}

/// What the inner process needs; passed as `key=value` arguments so that it survives `systemd-run`.
#[derive(Clone, Debug, Default)]
pub struct Spec {
    pub unit: String,
    pub group: String,
    pub kind: String,
    pub target: String,
    pub command: String,
    pub out: String,
    pub cap_bytes: u64,
    pub class: String,
    pub mem_total: u64,
    pub timeout_s: u64,
    pub verus: String,
    pub version: String,
    pub verus_sha: String,
    pub verifier_sha: String,
    pub rust: String,
    pub z3: String,
}

impl Spec {
    fn to_args(&self) -> Vec<String> {
        vec![
            format!("unit={}", self.unit),
            format!("group={}", self.group),
            format!("kind={}", self.kind),
            format!("target={}", self.target),
            format!("command={}", self.command),
            format!("out={}", self.out),
            format!("cap_bytes={}", self.cap_bytes),
            format!("class={}", self.class),
            format!("mem_total={}", self.mem_total),
            format!("timeout_s={}", self.timeout_s),
            format!("verus={}", self.verus),
            format!("version={}", self.version),
            format!("verus_sha={}", self.verus_sha),
            format!("verifier_sha={}", self.verifier_sha),
            format!("rust={}", self.rust),
            format!("z3={}", self.z3),
        ]
    }

    fn from_args(args: &[String]) -> Result<Spec, String> {
        let mut s = Spec::default();
        for a in args {
            let (k, v) = a
                .split_once('=')
                .ok_or_else(|| format!("bad argument {a}"))?;
            let v = v.to_string();
            match k {
                "unit" => s.unit = v,
                "group" => s.group = v,
                "kind" => s.kind = v,
                "target" => s.target = v,
                "command" => s.command = v,
                "out" => s.out = v,
                "cap_bytes" => s.cap_bytes = v.parse().map_err(|_| "cap_bytes")?,
                "class" => s.class = v,
                "mem_total" => s.mem_total = v.parse().map_err(|_| "mem_total")?,
                "timeout_s" => s.timeout_s = v.parse().map_err(|_| "timeout_s")?,
                "verus" => s.verus = v,
                "version" => s.version = v,
                "verus_sha" => s.verus_sha = v,
                "verifier_sha" => s.verifier_sha = v,
                "rust" => s.rust = v,
                "z3" => s.z3 = v,
                _ => return Err(format!("unknown argument {k}")),
            }
        }
        Ok(s)
    }

    fn file_stem(&self) -> String {
        self.unit.replace('/', "__")
    }
}

#[derive(Default)]
struct Measured {
    peak: u64,
    duration_s: f64,
    exit_code: i32,
    oom: u64,
    timed_out: bool,
    verified: Option<u64>,
    errors: Option<u64>,
    disk_start: u64,
    disk_min: u64,
    detail: String,
}

fn env_or(k: &str) -> String {
    std::env::var(k).unwrap_or_default()
}

fn write_report(spec: &Spec, m: &Measured, outcome: Outcome) -> Result<(), String> {
    let e = |s: &str| json_escape(s);
    let opt = |v: Option<u64>| v.map(|n| n.to_string()).unwrap_or_else(|| "null".into());
    let commit = if env_or("GITHUB_SHA").is_empty() {
        capture(Command::new("git").args(["rev-parse", "HEAD"]))
            .unwrap_or_default()
            .trim()
            .to_string()
    } else {
        env_or("GITHUB_SHA")
    };
    let s = format!(
        "{{\"unit\":\"{}\",\"group\":\"{}\",\"kind\":\"{}\",\"target\":\"{}\",\"outcome\":\"{}\",\"exit_code\":{},\"commit\":\"{}\",\"ref\":\"{}\",\"run_id\":\"{}\",\"job\":\"{}\",\"runner_name\":\"{}\",\"runner_class\":\"{}\",\"mem_total_bytes\":{},\"cap_bytes\":{},\"peak_bytes\":{},\"oom_kills\":{},\"timed_out\":{},\"timeout_s\":{},\"duration_s\":{:.1},\"verified\":{},\"errors\":{},\"disk_free_start_bytes\":{},\"disk_free_min_bytes\":{},\"verus_version\":\"{}\",\"verus_sha256\":\"{}\",\"rust_verify_sha256\":\"{}\",\"rust_toolchain\":\"{}\",\"z3\":\"{}\",\"detail\":\"{}\"}}\n",
        e(&spec.unit),
        e(&spec.group),
        e(&spec.kind),
        e(&spec.target),
        outcome.name(),
        m.exit_code,
        e(&commit),
        e(&env_or("GITHUB_REF")),
        e(&env_or("GITHUB_RUN_ID")),
        e(&env_or("GITHUB_JOB")),
        e(&env_or("RUNNER_NAME")),
        e(&spec.class),
        spec.mem_total,
        spec.cap_bytes,
        m.peak,
        m.oom,
        m.timed_out,
        spec.timeout_s,
        m.duration_s,
        opt(m.verified),
        opt(m.errors),
        m.disk_start,
        m.disk_min,
        e(&spec.version),
        e(&spec.verus_sha),
        e(&spec.verifier_sha),
        e(&spec.rust),
        e(&spec.z3),
        e(&m.detail)
    );
    fs::create_dir_all(&spec.out).map_err(|e| e.to_string())?;
    fs::write(
        Path::new(&spec.out).join(format!("{}.json", spec.file_stem())),
        s,
    )
    .map_err(|e| e.to_string())
}

/// Inside the scope. Exit code: the outcome's.
pub fn inner(args: &[String]) -> i32 {
    let spec = match Spec::from_args(args) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("lucina-ci inner: {e}");
            return 2;
        }
    };
    let infra = |m: &mut Measured, why: String| -> i32 {
        eprintln!("lucina-ci: {}: {why}", spec.unit);
        m.detail = why;
        let _ = write_report(&spec, m, Outcome::Infra);
        Outcome::Infra.exit_code()
    };
    let mut m = Measured::default();
    let note = |m: &mut Measured, s: String| {
        m.detail = if m.detail.is_empty() {
            s
        } else {
            format!("{}; {s}", m.detail)
        };
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    m.disk_start = disk_free(&cwd).unwrap_or(0);
    m.disk_min = m.disk_start;

    // The cap is checked, not assumed, and what the check proves is that this process stands in the scope `run_unit`
    // asked systemd for: `cgroup.kill` and a kill by pid reach every process in that cgroup, so it may only be aimed at a
    // cgroup that holds nothing but this step. A step that got no scope never reaches this point: `run_unit` fails the
    // check for it, so there is no uncapped branch.
    let step_cg = {
        let cg = match own_cgroup() {
            Ok(c) => c,
            Err(e) => return infra(&mut m, e),
        };
        if !cg
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with(SCOPE_OUR_PREFIX))
        {
            return infra(
                &mut m,
                format!("not a scope of this runner: {}", cg.display()),
            );
        }
        let max = read_trim(&cg.join("memory.max")).unwrap_or_default();
        if max != spec.cap_bytes.to_string() {
            return infra(
                &mut m,
                format!(
                    "cap not applied: memory.max is {max:?}, wanted {} in {}",
                    spec.cap_bytes,
                    cg.display()
                ),
            );
        }
        let swap = read_trim(&cg.join("memory.swap.max")).unwrap_or_default();
        if !swap.is_empty() && swap != "0" {
            return infra(
                &mut m,
                format!("swap not capped: memory.swap.max is {swap:?}"),
            );
        }
        Some(cg)
    };

    fs::create_dir_all(&spec.out).ok();
    let log_path = Path::new(&spec.out).join(format!("{}.log", spec.file_stem()));
    let (Ok(log), Ok(log2)) = (
        fs::File::create(&log_path),
        fs::File::create(&log_path).and_then(|f| f.try_clone()),
    ) else {
        return infra(&mut m, format!("cannot write {}", log_path.display()));
    };
    let start = Instant::now();
    let deadline = Duration::from_secs(spec.timeout_s);
    let bound = deadline + Duration::from_secs(KILL_GRACE_S + KILL_SLACK_S);
    // The step leads a process group of its own, which is what the `setsid --wait` wrapper was here for: the runner now
    // holds that group id itself instead of hiding the step behind a wrapper it cannot signal through.
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(&spec.command)
        .env("VERUS", &spec.verus)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log2))
        .process_group(0);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return infra(&mut m, format!("cannot start: {e}")),
    };
    let step = Step {
        cg: step_cg,
        pgid: child.id(),
    };
    let mut ticks = 0u64;
    // What the LAST round could not deliver. Every round repeats the whole kill, so what matters to an operator is the
    // state at the moment the runner gives up, not the count of rounds that failed on the way to it.
    let mut miss = Miss::default();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) => {}
            Err(_) => break None,
        }
        if let Some(c) = step.current() {
            m.peak = m.peak.max(c);
        }
        ticks += 1;
        if ticks.is_multiple_of(20)
            && let Some(d) = disk_free(&cwd)
        {
            m.disk_min = m.disk_min.min(d);
            if d < MIN_DISK_FREE && !m.timed_out {
                // The one detail that stays first: `outcome != verified` reads this line to tell a full disk from a
                // failure of the tree, so it is written as it always was, not appended to anything.
                m.detail = format!("disk nearly full: {d} bytes free");
                miss = step.kill();
            }
        }
        let e = start.elapsed();
        if e > deadline + Duration::from_secs(KILL_GRACE_S) {
            // Past the grace the step is killed, and killed again every round until nothing of it is left or the bound
            // passes. The old kill fired once, on a list it read once, and the loop then simply went on waiting.
            m.timed_out = true; // a step past the grace was past its deadline first
            miss = step.kill();
            if e > bound {
                let left = step.pids().len();
                if left > 0 {
                    note(
                        &mut m,
                        format!("{left} of the step's processes were still there at the bound"),
                    );
                }
                break None;
            }
        } else if e > deadline && !m.timed_out {
            m.timed_out = true;
            miss = step.terminate();
        }
        sleep(TICK);
    };
    let final_miss = step.kill();
    if !final_miss.is_empty() {
        miss = final_miss;
    }
    if !miss.is_empty() {
        // A kill that did not reach its target is said out loud, in the log the gate prints for a step that failed, and in
        // the report's detail. Silence here is how GATE-03 read for as long as it did: the step looked stopped because
        // nothing was ever said about the kill that had not stopped it.
        let line = format!("lucina-ci: {}: {}\n", spec.unit, miss.line());
        note(&mut m, miss.line());
        if let Ok(mut f) = fs::OpenOptions::new().append(true).open(&log_path) {
            let _ = f.write_all(line.as_bytes());
        }
    }
    m.duration_s = start.elapsed().as_secs_f64();
    m.exit_code = status.and_then(|s| s.code()).unwrap_or(137);
    if let Some(p) = step.peak_file() {
        m.peak = m.peak.max(p);
    }
    m.oom = step.oom_kills();
    if let Some(cg) = &step.cg {
        // The pen is this launcher's own to tidy, and an empty child cgroup left in the scope is one systemd has to chase
        // when the scope is collected. It fails while something of the step is still in there, which the report already says.
        let _ = fs::remove_dir(cg.join(KILL_PEN));
    }
    let text = fs::read_to_string(&log_path).unwrap_or_default();
    (m.verified, m.errors) = verdict::parse_results(&text);
    let mut outcome = verdict::classify(&Facts {
        kind: &spec.kind,
        exit_code: m.exit_code,
        oom_kills: m.oom,
        timed_out: m.timed_out,
        verified: m.verified,
        errors: m.errors,
    });
    if outcome == Outcome::Verified {
        let c = verdict::check_output(&spec.kind, &text);
        if let Some(w) = c.weak {
            m.detail = format!("{} weak refusals (no verification run)", w);
        }
        if let Some(p) = c.problem {
            // A mutants log without its baseline is a script that cannot be trusted (infra); the others passed on the exit code alone.
            outcome = if spec.kind == "mutants" {
                Outcome::Infra
            } else {
                Outcome::Vacuous
            };
            m.detail = p;
        }
    }
    if outcome != Outcome::Verified && m.detail.starts_with("disk nearly full") {
        outcome = Outcome::Infra;
    }
    if let Err(e) = write_report(&spec, &m, outcome) {
        eprintln!("lucina-ci: {}: cannot write the report: {e}", spec.unit);
        return Outcome::Infra.exit_code();
    }
    outcome.exit_code()
}

/// How long the probe for a systemd user manager may take. `systemctl --user` waits on a bus that may never answer
/// (a runner with no user session), and a probe that waits there stops the whole check with it.
const PROBE_LIMIT_S: u64 = 5;

/// Runs `cmd` and says whether it ended with success within `secs` seconds. One still running then is killed.
fn succeeds_within(cmd: &mut Command, secs: u64) -> bool {
    let Ok(mut child) = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let end = Instant::now() + Duration::from_secs(secs);
    loop {
        match child.try_wait() {
            Ok(Some(s)) => return s.success(),
            Ok(None) if Instant::now() < end => sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

/// How to get a scope with a memory cap on this machine.
fn scope_prefix() -> Result<Vec<String>, String> {
    let base = ["--scope", "--quiet", "--collect"];
    let uid = capture(Command::new("id").arg("-u"))
        .unwrap_or_default()
        .trim()
        .to_string();
    let mut v: Vec<String> = Vec::new();
    if uid == "0" {
        v.push("systemd-run".into());
    } else {
        let user_bus = succeeds_within(
            Command::new("systemctl").args(["--user", "show-environment"]),
            PROBE_LIMIT_S,
        );
        if user_bus {
            v.extend(["systemd-run".to_string(), "--user".to_string()]);
        } else {
            return Err(
                "no systemd user manager (`systemctl --user show-environment` fails or does not answer in 5 s)".into(),
            );
        }
    }
    v.extend(base.map(str::to_string));
    Ok(v)
}

/// The whole `systemd-run` argv of one step's scope. A step's scope is put in `build.slice`, always: `systemd-run
/// --user --scope` otherwise lands it in `app.slice`, outside the slice the build queue's jobs are held to, and 80 of them
/// from several concurrent checks stacked up there until the host ran out of memory. Every other property is as it was.
fn scope_args(
    prefix: Vec<String>,
    spec: &Spec,
    exe: &Path,
    cpu_quota: Option<&str>,
) -> Vec<String> {
    let mut c = prefix;
    c.push("--slice=build.slice".into());
    c.push(format!(
        "--unit=lucina-ci-{}-{}",
        std::process::id(),
        spec.file_stem().replace(['_', '.'], "-")
    ));
    c.push("--property=MemoryAccounting=yes".into());
    c.push(format!("--property=MemoryMax={}", spec.cap_bytes));
    c.push("--property=MemorySwapMax=0".into());
    // Otherwise the kernel kills the whole scope, this launcher included, and the report loses its numbers.
    c.push("--property=OOMPolicy=continue".into());
    if let Some(q) = cpu_quota {
        c.push(format!("--property=CPUQuota={q}"));
        c.push("--property=TasksMax=256".into());
    }
    c.push("--".into());
    c.push(exe.display().to_string());
    c.push("__inner".into());
    c.extend(spec.to_args());
    c
}

/// Runs one unit under its own capped scope; the report always exists afterwards. Returns the outcome's exit code.
///
/// Every step gets its own capped scope. Where it cannot get one (no systemd user manager) the check FAILS with one
/// message that names the step and the reason; no step is ever run uncapped, and `lucina-ci` never runs `sudo`
/// A check-fast step used to fall back to an uncapped process group, and `LUCINA_CI_NO_SCOPE` forced it; that is gone.
pub fn run_unit(spec: &Spec) -> Result<i32, String> {
    let exe = step_exe()?;
    let prefix = scope_prefix().map_err(|why| {
        format!(
            "step {}: cannot get a memory-capped scope: {why}",
            spec.unit
        )
    })?;
    let c = scope_args(
        prefix,
        spec,
        &exe,
        std::env::var("LUCINA_CI_CPU_QUOTA").ok().as_deref(),
    );
    let mut cmd = Command::new(&c[0]);
    cmd.args(&c[1..]);
    let launcher = c[0].clone();
    let status = cmd
        .status()
        .map_err(|e| format!("cannot start {launcher}: {e}"))?;
    let code = status.code().unwrap_or(137);
    let report = Path::new(&spec.out).join(format!("{}.json", spec.file_stem()));
    if !report.exists() {
        // The inner process died before reporting: killed by the OOM killer, or it could not start.
        let outcome = if code == 137 {
            Outcome::Oom
        } else {
            Outcome::Infra
        };
        let m = Measured {
            exit_code: code,
            detail: "the launcher of the step died before it could report".into(),
            ..Default::default()
        };
        write_report(spec, &m, outcome)?;
        return Ok(outcome.exit_code());
    }
    Ok(code)
}

/// Field of a report line. Reports are written by this program, one object per file, with escaped strings.
pub fn report_field(json: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\":\"");
    let i = json.find(&pat)? + pat.len();
    let rest = &json[i..];
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => {
                out.push(chars.next()?);
            }
            c => out.push(c),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    /// The probe for a user manager is bounded: a command that does not answer is a "no", and does not stop the check.
    #[test]
    fn a_probe_that_does_not_answer_is_a_no_inside_its_limit() {
        let t = Instant::now();
        assert!(!succeeds_within(Command::new("sleep").arg("30"), 1));
        assert!(
            t.elapsed() < Duration::from_secs(10),
            "the probe waited {:?} for a command that never answers",
            t.elapsed()
        );
        assert!(succeeds_within(&mut Command::new("true"), 5));
        assert!(!succeeds_within(&mut Command::new("false"), 5));
        assert!(!succeeds_within(&mut Command::new("/nonexistent/probe"), 5));
    }

    fn test_spec() -> Spec {
        Spec {
            unit: "u".into(),
            group: "fast".into(),
            kind: "k".into(),
            target: "t".into(),
            command: "true".into(),
            out: "/nonexistent".into(),
            cap_bytes: 1024,
            class: "c".into(),
            mem_total: 4096,
            timeout_s: 5,
            verus: String::new(),
            version: String::new(),
            verus_sha: String::new(),
            verifier_sha: String::new(),
            rust: String::new(),
            z3: String::new(),
        }
    }

    #[test]
    fn a_step_scope_is_started_in_build_slice_and_nothing_else_changes() {
        // Exactly one slice argument, spelled exactly so, ahead of the unit and the properties.
        let prefix: Vec<String> = ["systemd-run", "--user", "--scope", "--quiet", "--collect"]
            .map(str::to_string)
            .to_vec();
        let argv = scope_args(
            prefix,
            &test_spec(),
            Path::new("/x/lucina-ci"),
            Some("400%"),
        );
        assert_eq!(
            argv.iter().filter(|a| a.starts_with("--slice")).count(),
            1,
            "{argv:?}"
        );
        assert!(argv.iter().any(|a| a == "--slice=build.slice"), "{argv:?}");
        for want in [
            "--property=MemoryAccounting=yes",
            "--property=MemoryMax=1024",
            "--property=MemorySwapMax=0",
            "--property=OOMPolicy=continue",
            "--property=CPUQuota=400%",
            "--property=TasksMax=256",
            "--",
            "/x/lucina-ci",
            "__inner",
        ] {
            assert!(argv.iter().any(|a| a == want), "{want} missing: {argv:?}");
        }
        let no_quota = scope_args(
            vec!["systemd-run".into()],
            &test_spec(),
            Path::new("/x/l"),
            None,
        );
        assert!(no_quota.iter().any(|a| a == "--slice=build.slice"));
        assert!(
            !no_quota
                .iter()
                .any(|a| a.starts_with("--property=CPUQuota"))
        );
    }

    #[test]
    fn the_deadline_bounds_the_time_a_step_may_record() {
        // `tests/runner_kill_scope.rs` judges a step's recorded time against `limit + grace + slack` and cannot name
        // these (an integration test sees the binary, not the module), so the numbers are written there. Pinned here,
        // where they live: a change to either is a change that test has to be told about.
        assert_eq!(KILL_GRACE_S, 5);
        assert_eq!(KILL_SLACK_S, 2);
        // The two moments the wait loop compares the step's clock against, built as it builds them: a grace of zero
        // would leave a step nothing to clean up with, and a bound that is not later than the end of the grace would
        // report a step before its kill rounds had a chance.
        let deadline = Duration::from_secs(4);
        let grace_end = deadline + Duration::from_secs(KILL_GRACE_S);
        let bound = deadline + Duration::from_secs(KILL_GRACE_S + KILL_SLACK_S);
        assert!(
            grace_end > deadline,
            "no grace leaves a step nothing to clean up with"
        );
        assert!(
            bound > grace_end,
            "the bound should not be looser than the grace it is counted from"
        );
    }

    #[test]
    fn the_group_of_this_process_is_the_one_the_kill_never_aims_at() {
        // `signal` refuses a group signal aimed at this process's own group: the deadline must not kill the check that
        // enforces it. The number comes out of `/proc/self/stat`, where the `comm` field can hold spaces and
        // parentheses, so it is compared with what the kernel tells a shell about the very same process.
        let o = Command::new("sh")
            .args(["-c", "ps -o pgid= -p $$"])
            .output()
            .unwrap();
        let said: u32 = String::from_utf8_lossy(&o.stdout)
            .trim()
            .parse()
            .unwrap_or_else(|e| panic!("`ps -o pgid=` said {:?}: {e}", o.stdout));
        assert_eq!(own_pgid(), Some(said));
    }

    /// A duration no other process on the machine carries, for the tests below that signal real processes.
    fn marker(k: u32) -> String {
        format!("{}99{}", std::process::id(), k)
    }

    struct Sweep(String);

    impl Drop for Sweep {
        fn drop(&mut self) {
            let _ = Command::new("pkill")
                .args(["-KILL", "-f", &format!("sleep {}$", self.0)])
                .stderr(Stdio::null())
                .status();
        }
    }

    #[test]
    fn the_screen_refuses_every_target_that_would_kill_the_check_itself() {
        // GATE-03's one hard rule about the killer: pid 0 is *this* process's group, pid 1 is init, a negative target is a
        // whole group, and this pid, this group and this parent are the check, its launcher and whatever started that.
        // The screen is what `kill_now`'s debug assertion runs on, so these are not style points.
        assert!(!signalable(0), "pid 0 is this process's own group");
        assert!(!signalable(1), "pid 1 is init");
        assert!(
            !signalable(std::process::id()),
            "this pid is the report writer"
        );
        assert!(
            !signalable(own_pgid().expect("this process has a group")),
            "this group holds the check and the other steps"
        );
        assert!(
            !signalable(own_ppid().expect("this process has a parent")),
            "the launcher that started this one is never the step's"
        );
        // And it is a screen, not a refusal of everything: no kernel pid reaches this high (`/proc/sys/kernel/pid_max` is
        // 4194304 on buildhost, the ceiling the kernel allows), so nothing real is being hidden behind it.
        assert!(signalable(2_000_000_000));
    }

    #[test]
    fn kill_2_reaches_a_live_process_and_a_gone_one_is_no_failure() {
        // The syscall route, proven on a real target: no `/bin/kill` is forked, the signal lands, and a pid that is
        // already gone answers `ESRCH`, which the deadline reads as delivered — there is nothing left to reach.
        let m = marker(1);
        let _sweep = Sweep(m.clone());
        assert!(
            kill_now(i32::MAX, libc::SIGKILL).is_ok(),
            "a target that does not exist is not a failed delivery"
        );
        let mut child = Command::new("sleep")
            .arg(&m)
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        let pid = i32::try_from(child.id()).unwrap();
        assert!(
            kill_now(pid, libc::SIGKILL).is_ok(),
            "a signal to a process of this user must be delivered"
        );
        let s = child.wait().unwrap();
        assert_eq!(
            s.signal(),
            Some(9),
            "the step's process was not SIGKILLed: {s:?}"
        );
    }

    #[test]
    fn a_refused_signal_is_a_failed_delivery_and_says_why() {
        // The arm `Miss` exists for: a `kill(2)` that did not deliver answers with a reason, and the reason reaches the
        // report instead of being discarded with the status of a forked program. A signal number the kernel does not
        // know is the refusal this test can cause on purpose, on a target of its own, without touching another user's
        // process — where a target that is gone answers `ESRCH`, which is NOT a failed delivery.
        let m = marker(3);
        let _sweep = Sweep(m.clone());
        let mut child = Command::new("sleep")
            .arg(&m)
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        let pid = i32::try_from(child.id()).unwrap();
        let e = kill_now(pid, 999)
            .expect_err("a signal number outside the kernel's table cannot be delivered, and must not look delivered");
        assert!(e.contains("Invalid argument"), "{e}");
        assert!(
            kill_now(i32::MAX, libc::SIGTERM).is_ok(),
            "nothing there to reach is a delivered deadline, not a refused one"
        );
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn one_signal_to_the_group_takes_everything_the_group_holds() {
        // The route GATE-03 is really about: work the step hands to a nested `systemd-run --scope` stands in a SIBLING
        // cgroup, so no walk of this step's tree names it and only a group signal reaches it. Here the same shape without
        // systemd: a shell in a group of its own, holding a child that inherited that group.
        let m = marker(2);
        let _sweep = Sweep(m.clone());
        let mut child = Command::new("bash")
            .args(["-c", &format!("sleep {m} & wait")])
            .process_group(0)
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        let pgid = i32::try_from(child.id()).unwrap();
        let mut seen: u32 = 0;
        for _ in 0..40 {
            let o = Command::new("pgrep")
                .args(["-fc", &format!("sleep {m}$")])
                .output()
                .unwrap();
            seen = String::from_utf8_lossy(&o.stdout)
                .trim()
                .parse()
                .unwrap_or(0);
            if seen >= 1 {
                break;
            }
            sleep(Duration::from_millis(100));
        }
        assert_eq!(
            seen, 1,
            "the shell never got its backgrounded sleeper up: {m}"
        );
        assert!(
            kill_now(-pgid, libc::SIGKILL).is_ok(),
            "the group signal is the one the deadline relies on"
        );
        let s = child.wait().unwrap();
        assert_eq!(
            s.signal(),
            Some(9),
            "the group leader never took the SIGKILL the group was sent"
        );
        for _ in 0..40 {
            if survivors(&format!("sleep {m}$")).is_empty() {
                return;
            }
            sleep(Duration::from_millis(100));
        }
        panic!(
            "one SIGKILL to the process group left {} of its members standing",
            survivors(&format!("sleep {m}$")).len()
        );
    }

    fn survivors(pat: &str) -> Vec<u32> {
        let o = Command::new("pgrep").args(["-f", pat]).output().unwrap();
        String::from_utf8_lossy(&o.stdout)
            .lines()
            .filter_map(|l| l.trim().parse().ok())
            .filter(|p: &u32| *p != std::process::id())
            .collect()
    }

    #[test]
    fn the_rlimit_table_selects_by_unit_name() {
        // One entry, a number, and no other unit gets anything. The exact string the surface unit is run with is pinned by
        // `inventory::tests::only_the_named_verus_unit_gets_extra_arguments`; writing that string here as well would give
        // this scanned file a second `--rlimit` site for the one table entry.
        let (unit, limit) = VERUS_ARGS.first().copied().expect("the table has no entry");
        assert_eq!((unit, limit), ("verus/juno-surface/rules", 20));
        assert_eq!(VERUS_ARGS.len(), 1);
        assert_eq!(verus_args(unit), rlimit_arg(limit));
        assert!(verus_args("verus/juno-charge/rules").is_empty());
        assert!(verus_args("mutants/juno-surface").is_empty());
        assert!(verus_args("").is_empty());
    }

    #[test]
    fn the_trust_scanner_counts_this_table_and_nothing_else() {
        // One `--rlimit` site per entry, with the value the entry is rendered with: a second entry needs a second allow
        // count in `tools/ci/trust-allow.tsv`, and a raise needs its max_value raised there.
        let scan = crate::trust::scan_flags(crate::trust::RUNNER_SOURCE, "", false);
        let counted: Vec<Option<u64>> = scan
            .markers
            .iter()
            .filter(|m| m.class == crate::trust::Class::Rlimit)
            .map(|m| m.value)
            .collect();
        let rendered: Vec<Option<u64>> = VERUS_ARGS
            .iter()
            .map(|(_, limit)| {
                crate::trust::parse_value(rlimit_arg(*limit).rsplit_once(' ').unwrap().1)
            })
            .collect();
        assert_eq!(counted, rendered);
        assert_eq!(counted.len(), VERUS_ARGS.len());
        assert_eq!(
            scan.markers.len(),
            VERUS_ARGS.len(),
            "no other flag is passed"
        );
    }

    #[test]
    fn budget_on_a_16_gb_runner_is_12_gib() {
        let b = budget(12, None, 15_600_000_000).unwrap();
        assert_eq!((b.cap_bytes, b.class), (12 * GIB, "standard"));
    }

    #[test]
    fn budget_refuses_an_8_gb_runner_with_what_to_do() {
        let e = budget(12, None, 7_800_000_000).unwrap_err();
        assert!(e.contains("VERUS_SMALL_RUNNER_CAP_GIB"), "{e}");
    }

    #[test]
    fn small_cap_is_recorded_and_still_needs_room() {
        let b = budget(12, Some(6), 8 * GIB - GIB / 8).unwrap();
        assert_eq!((b.cap_bytes, b.class), (6 * GIB, "small-runner"));
        assert!(budget(12, Some(7), 8 * GIB - GIB / 8).is_err());
    }

    #[test]
    fn report_fields_read_back_escaped() {
        let j = format!(
            "{{\"unit\":\"{}\",\"outcome\":\"oom\"}}",
            json_escape("a\"b\\c")
        );
        assert_eq!(report_field(&j, "unit").as_deref(), Some("a\"b\\c"));
        assert_eq!(report_field(&j, "outcome").as_deref(), Some("oom"));
        assert_eq!(report_field(&j, "absent"), None);
    }

    #[test]
    fn upstream_lock_values() {
        let l = "[other]\nrelease = \"x\"\n[verus]\nrelease = \"0.1\"  # c\nbinary_sha256 = \"ab\"\n[next]\n";
        assert_eq!(lock_value(l, "release").as_deref(), Some("0.1"));
        assert_eq!(lock_value(l, "binary_sha256").as_deref(), Some("ab"));
        assert_eq!(lock_value(l, "absent"), None);
    }
}
