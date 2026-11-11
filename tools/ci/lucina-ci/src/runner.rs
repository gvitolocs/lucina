//! Runs one proof unit in its own cgroup v2 scope. The outer process (outside the cap) starts `systemd-run --scope`; the inner
//! process, inside the scope, checks that the cap really applies, runs the unit and reads the scope's own counters.

use crate::verdict::{self, Facts, Outcome};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

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

fn kill_cgroup_except_self(cg: &Path) {
    let me = std::process::id().to_string();
    for _ in 0..20 {
        let pids: Vec<String> = fs::read_to_string(cg.join("cgroup.procs"))
            .unwrap_or_default()
            .lines()
            .filter(|p| *p != me)
            .map(str::to_string)
            .collect();
        if pids.is_empty() {
            return;
        }
        let _ = Command::new("kill")
            .arg("-9")
            .args(&pids)
            .stderr(Stdio::null())
            .status();
        sleep(Duration::from_millis(100));
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

/// What the inner process needs; passed as `key=value` arguments so that it survives `sudo` and `systemd-run`.
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
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    m.disk_start = disk_free(&cwd).unwrap_or(0);
    m.disk_min = m.disk_start;

    // The cap is checked, not assumed.
    let cg = match own_cgroup() {
        Ok(c) => c,
        Err(e) => return infra(&mut m, e),
    };
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
    // setsid: the unit has its own process group, so a timeout can take all of it at once.
    let mut cmd = Command::new("setsid");
    cmd.args(["--wait", "bash", "-c", &spec.command])
        .env("VERUS", &spec.verus)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log2));
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return infra(&mut m, format!("cannot start: {e}")),
    };
    let mut ticks = 0u64;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) => {}
            Err(_) => break None,
        }
        if let Some(c) = read_trim(&cg.join("memory.current")).and_then(|s| s.parse::<u64>().ok()) {
            m.peak = m.peak.max(c);
        }
        ticks += 1;
        if ticks.is_multiple_of(20)
            && let Some(d) = disk_free(&cwd)
        {
            m.disk_min = m.disk_min.min(d);
            if d < MIN_DISK_FREE && !m.timed_out {
                m.detail = format!("disk nearly full: {d} bytes free");
                kill_cgroup_except_self(&cg);
            }
        }
        if start.elapsed() > deadline && !m.timed_out {
            m.timed_out = true;
            kill_cgroup_except_self(&cg);
        }
        sleep(Duration::from_millis(250));
    };
    kill_cgroup_except_self(&cg);
    m.duration_s = start.elapsed().as_secs_f64();
    m.exit_code = status.and_then(|s| s.code()).unwrap_or(137);
    if let Some(p) = read_trim(&cg.join("memory.peak")).and_then(|s| s.parse::<u64>().ok()) {
        m.peak = m.peak.max(p);
    }
    m.oom = oom_kills(&cg);
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
        let user_bus = Command::new("systemctl")
            .args(["--user", "show-environment"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if user_bus {
            v.extend(["systemd-run".to_string(), "--user".to_string()]);
        } else {
            let gid = capture(Command::new("id").arg("-g"))
                .unwrap_or_default()
                .trim()
                .to_string();
            let sudo = Command::new("sudo")
                .args(["-n", "true"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            if !sudo {
                return Err("no systemd user session and no passwordless sudo: cannot create a memory-capped scope".into());
            }
            v.extend(["sudo", "-n", "-E", "systemd-run"].map(str::to_string));
            v.push(format!("--uid={uid}"));
            v.push(format!("--gid={gid}"));
        }
    }
    v.extend(base.map(str::to_string));
    Ok(v)
}

/// Runs one unit under its own capped scope; the report always exists afterwards. Returns the outcome's exit code.
pub fn run_unit(spec: &Spec) -> Result<i32, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut c = scope_prefix()?;
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
    if let Ok(q) = std::env::var("LUCINA_CI_CPU_QUOTA") {
        c.push(format!("--property=CPUQuota={q}"));
        c.push("--property=TasksMax=256".into());
    }
    c.push("--".into());
    c.push(exe.display().to_string());
    c.push("__inner".into());
    c.extend(spec.to_args());
    let status = Command::new(&c[0])
        .args(&c[1..])
        .status()
        .map_err(|e| format!("cannot start {}: {e}", c[0]))?;
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
            detail: "the launcher inside the scope died before it could report".into(),
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
