//! `lucina-ci kill-scan [--root DIR] [FILE...]`: refuses an external `kill`, `pkill` or `killall` program whose target is a
//! negative process or group id.
//!
//! Why. On some hosts the external `kill` program (procps-ng 4.0.4) reads a negative id from its first digit only, so a request
//! for the group `-10027` becomes a request for `-1`, which with a real signal is every process the user owns, the CI runner's own
//! worker included. On a developer host the same call is right, so the mistake hides until a hosted run. The shell builtin
//! `kill` is right; only the external program is wrong. The fix is an in-process `libc::kill`, which has no parser in between.
//!
//! What it reads. With no FILE, every git-tracked file under the root; text files only (a NUL in the first 8 KiB, or a size over
//! 4 MiB, skips a file and the skip is printed). Rust files: a `Command` built from one of the kill programs whose statement
//! carries a string literal that starts with a minus and a brace, or a minus and three or more digits. Shell and workflow text
//! (`*.sh`, `*.yml`, `*.yaml`, a `#!` shell line): a line that names the external program (by path, or after `env`, `command`,
//! `xargs`, `exec`, `sudo`, or as `pkill`/`killall`) followed on the same line by a target that starts with a minus and three or
//! more digits, or a minus and a dollar sign. A plain `kill` word in a shell script is the builtin and is not read. Comments are
//! not read. A path in `tools/ci/kill-scan-allow.txt` (`path<TAB>reason`, a reason is required) is skipped.
//!
//! The patterns below are built from halves so that scanning this repository does not find them in this file.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::Command;

/// The allow file, relative to the root.
pub const ALLOW_FILE: &str = "tools/ci/kill-scan-allow.txt";

const MAX_BYTES: u64 = 4 * 1024 * 1024;
const NUL_WINDOW: usize = 8 * 1024;
const MAX_STATEMENT_LINES: usize = 12;

/// The refusal text; the path and line are filled in.
const REFUSAL: &str = "external kill/pkill with a negative or variable target";

/// The program names a Rust statement can start. Compared with the whole first string literal.
fn rust_programs() -> [&'static str; 5] {
    ["kill", "pkill", "killall", "/usr/bin/kill", "/bin/kill"]
}

/// What starts a Rust process builder.
fn builder_needle() -> &'static str {
    concat!("Command", "::", "new", "(")
}

/// The shell triggers: a word that names the external program. (`kill` alone is the builtin and is not here.)
fn shell_triggers() -> [&'static str; 9] {
    [
        "/usr/bin/kill",
        "/bin/kill",
        "env kill",
        "command kill",
        "xargs kill",
        "exec kill",
        "sudo kill",
        "pkill",
        "killall",
    ]
}

/// One finding.
#[derive(Debug, PartialEq, Eq)]
pub struct Hit {
    pub path: String,
    pub line: usize,
}

/// The string literals of `text` in order, escapes decoded just enough to compare (`\"` and `\\`).
fn literals(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_str = false;
    let mut esc = false;
    for c in text.chars() {
        if in_str {
            if esc {
                cur.push(c);
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
                out.push(std::mem::take(&mut cur));
            } else {
                cur.push(c);
            }
        } else if c == '"' {
            in_str = true;
        }
    }
    out
}

/// `line` without its trailing `//` comment, string literals and character literals respected.
fn strip_rust_comment(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    let mut in_str = false;
    while i < chars.len() {
        let c = chars[i];
        if in_str {
            out.push(c);
            if c == '\\' && i + 1 < chars.len() {
                out.push(chars[i + 1]);
                i += 2;
                continue;
            }
            if c == '"' {
                in_str = false;
            }
        } else if c == '"' {
            in_str = true;
            out.push(c);
        } else if c == '\'' {
            // A character literal such as '"' or '\'' must not open a string; a lifetime has no closing quote nearby.
            let simple = i + 2 < chars.len() && chars[i + 2] == '\'' && chars[i + 1] != '\\';
            let escaped = i + 3 < chars.len() && chars[i + 1] == '\\' && chars[i + 3] == '\'';
            let n = if simple {
                3
            } else if escaped {
                4
            } else {
                1
            };
            for k in 0..n {
                out.push(chars[i + k]);
            }
            i += n;
            continue;
        } else if c == '/' && i + 1 < chars.len() && chars[i + 1] == '/' {
            break;
        } else {
            out.push(c);
        }
        i += 1;
    }
    out
}

/// Is `lit` a negative target: a minus then a brace, or a minus then three or more digits?
fn negative_literal(lit: &str) -> bool {
    if lit.contains("-{") {
        return true;
    }
    let b = lit.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'-' {
            let digits = b[i + 1..].iter().take_while(|d| d.is_ascii_digit()).count();
            if digits >= 3 {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// The 1-based lines of a Rust file that start an external kill statement with a negative or variable target.
pub fn scan_rust(text: &str) -> Vec<usize> {
    let lines: Vec<String> = text.lines().map(strip_rust_comment).collect();
    let needle = builder_needle();
    let mut hits = Vec::new();
    for (n, line) in lines.iter().enumerate() {
        let mut from = 0;
        while let Some(off) = line[from..].find(needle) {
            let start = from + off + needle.len();
            from = start;
            // The statement: from the builder to the terminating semicolon, at most 12 lines.
            let mut stmt = String::from(&line[start..]);
            let mut k = n;
            while !stmt.contains(';') && k + 1 < lines.len() && k + 1 - n < MAX_STATEMENT_LINES {
                k += 1;
                stmt.push('\n');
                stmt.push_str(&lines[k]);
            }
            if let Some(end) = stmt.find(';') {
                stmt.truncate(end);
            }
            let lits = literals(&stmt);
            let Some(prog) = lits.first() else { continue };
            if !rust_programs().contains(&prog.as_str()) {
                continue;
            }
            if lits[1..].iter().any(|l| negative_literal(l)) {
                hits.push(n + 1);
            }
        }
    }
    hits.dedup();
    hits
}

/// Is `b` a byte that can be part of a command word?
fn word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'/')
}

/// `line` without a comment: a whole-line `#` comment is dropped, a trailing ` #` comment is cut.
fn strip_shell_comment(line: &str) -> &str {
    let t = line.trim_start();
    if t.starts_with('#') {
        return "";
    }
    match line.find(" #") {
        Some(i) => &line[..i],
        None => line,
    }
}

/// Does `rest` (the text after a trigger) hold a target: a minus then three or more digits, or a minus then a dollar sign?
fn shell_target(rest: &str) -> bool {
    rest.split_whitespace().any(|tok| {
        let t = tok.trim_start_matches(['"', '\'', '(']);
        let Some(r) = t.strip_prefix('-') else {
            return false;
        };
        r.starts_with('$') || r.bytes().take_while(|d| d.is_ascii_digit()).count() >= 3
    })
}

/// The 1-based lines of shell or workflow text that name the external program with a negative or variable target.
pub fn scan_shell(text: &str) -> Vec<usize> {
    let mut hits = Vec::new();
    for (n, raw) in text.lines().enumerate() {
        let line = strip_shell_comment(raw);
        let b = line.as_bytes();
        'line: for trig in shell_triggers() {
            let mut from = 0;
            while let Some(off) = line[from..].find(trig) {
                let s = from + off;
                let e = s + trig.len();
                from = e;
                let before_ok = s == 0 || !word_byte(b[s - 1]);
                let after_ok = e >= b.len() || !word_byte(b[e]);
                if before_ok && after_ok && shell_target(&line[e..]) {
                    hits.push(n + 1);
                    break 'line;
                }
            }
        }
    }
    hits
}

/// The allowed paths, or a message when a line has no reason.
pub fn load_allow(root: &Path) -> Result<BTreeSet<String>, String> {
    let p = root.join(ALLOW_FILE);
    let Ok(text) = fs::read_to_string(&p) else {
        return Ok(BTreeSet::new());
    };
    let mut set = BTreeSet::new();
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let mut parts = line.splitn(2, '\t');
        let path = parts.next().unwrap_or("").trim();
        let reason = parts.next().unwrap_or("").trim();
        if path.is_empty() || reason.is_empty() {
            return Err(format!(
                "{}:{}: an allowed path needs a reason: path, a tab, the reason",
                ALLOW_FILE,
                n + 1
            ));
        }
        set.insert(path.to_string());
    }
    Ok(set)
}

fn tracked_files(root: &Path) -> Result<Vec<String>, String> {
    let out = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(root)
        .output()
        .map_err(|e| format!("git ls-files: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git ls-files failed in {}: {}",
            root.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out
        .stdout
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect())
}

/// What kind of text a file is.
enum Kind {
    Rust,
    Shell,
    Other,
}

fn kind_of(path: &str, text: &str) -> Kind {
    if path.ends_with(".rs") {
        Kind::Rust
    } else if path.ends_with(".sh") || path.ends_with(".yml") || path.ends_with(".yaml") {
        Kind::Shell
    } else if let Some(first) = text.lines().next()
        && first.starts_with("#!")
        && ["sh", "bash", "zsh", "dash", "ksh"]
            .iter()
            .any(|s| first.split(['/', ' ']).any(|w| w == *s))
    {
        Kind::Shell
    } else {
        Kind::Other
    }
}

/// Runs the scan. Returns the exit code: 0 clean, 1 a hit, 2 an error.
pub fn run(root: &Path, files: &[String]) -> i32 {
    let allow = match load_allow(root) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("kill-scan: {e}");
            return 2;
        }
    };
    let list = if files.is_empty() {
        match tracked_files(root) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("kill-scan: {e}");
                return 2;
            }
        }
    } else {
        files
            .iter()
            .map(|f| f.trim_start_matches("./").to_string())
            .collect()
    };
    let (mut scanned, mut skipped, mut allowed) = (0usize, 0usize, 0usize);
    let mut hits: Vec<Hit> = Vec::new();
    for rel in &list {
        if allow.contains(rel) {
            allowed += 1;
            continue;
        }
        let full = root.join(rel);
        let Ok(meta) = fs::metadata(&full) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        if meta.len() > MAX_BYTES {
            println!("kill-scan: skipped {rel}: larger than 4 MiB");
            skipped += 1;
            continue;
        }
        let Ok(bytes) = fs::read(&full) else {
            continue;
        };
        if bytes[..bytes.len().min(NUL_WINDOW)].contains(&0) {
            println!("kill-scan: skipped {rel}: binary file");
            skipped += 1;
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        scanned += 1;
        let lines = match kind_of(rel, &text) {
            Kind::Rust => scan_rust(&text),
            Kind::Shell => scan_shell(&text),
            Kind::Other => Vec::new(),
        };
        for line in lines {
            hits.push(Hit {
                path: rel.clone(),
                line,
            });
        }
    }
    for h in &hits {
        println!("kill-scan: {}:{}: {REFUSAL}", h.path, h.line);
    }
    println!(
        "kill-scan: {scanned} files read, {skipped} skipped, {allowed} allowed, {} refused",
        hits.len()
    );
    if hits.is_empty() { 0 } else { 1 }
}

/// `kill-scan [--root DIR] [FILE...]`.
pub fn cmd(args: &[String]) -> Result<i32, String> {
    let mut root: Option<String> = None;
    let mut files = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--root" {
            root = Some(it.next().ok_or("--root needs a directory")?.clone());
        } else if a.starts_with("--") {
            return Err(format!("kill-scan: unknown option {a}"));
        } else {
            files.push(a.clone());
        }
    }
    let root = match root {
        Some(r) => std::path::PathBuf::from(r),
        None => std::env::current_dir().map_err(|e| e.to_string())?,
    };
    Ok(run(&root, &files))
}
