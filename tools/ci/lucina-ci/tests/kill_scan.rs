//! `lucina-ci kill-scan`: an external kill, pkill or killall program with a negative or variable target is refused (exit 1), the
//! in-process call and the shell builtin pass, an allow file entry with a reason skips a path, one without a reason is an error
//! (exit 2), and a binary file is skipped and said so. The text the scanner looks for is built here from parts, so that scanning
//! this repository does not find it in this file. Each test works in a folder under the temp dir that is removed on drop, so a
//! panic leaves nothing; no process is signalled.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A canonical temp folder, removed on drop.
struct Tmp(PathBuf);
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn tmp(name: &str) -> Tmp {
    let p = std::env::temp_dir().join(format!("lucina-kill-scan-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&p);
    fs::create_dir_all(&p).unwrap();
    Tmp(fs::canonicalize(&p).unwrap())
}

fn put(root: &Path, rel: &str, body: &[u8]) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

/// `lucina-ci kill-scan --root ROOT files...`: exit code and stdout plus stderr.
fn scan(root: &Path, files: &[&str]) -> (Option<i32>, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_lucina-ci"))
        .arg("kill-scan")
        .arg("--root")
        .arg(root)
        .args(files)
        .output()
        .unwrap();
    let msg = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    (o.status.code(), msg)
}

/// The Rust builder for the external program, in text.
fn builder() -> String {
    ["Command", "::", "new(", "\"kill\"", ")"].concat()
}

/// The external program by its path, in text.
fn ext_kill() -> String {
    ["/usr/bin/", "kill"].concat()
}

fn refusal(path: &str, line: usize) -> String {
    format!("kill-scan: {path}:{line}: external kill/pkill with a negative or variable target")
}

#[test]
fn a_format_target_with_a_minus_is_flagged_with_its_line() {
    let t = tmp("a");
    let src = "fn main() {\n    let pid = 1;\n    BUILDER.args([\"-KILL\", &format!(\"-{}\", pid)]).status().ok();\n}\n"
        .replace("BUILDER", &builder());
    put(&t.0, "a.rs", src.as_bytes());
    let (code, out) = scan(&t.0, &["a.rs"]);
    assert_eq!(code, Some(1), "{out}");
    assert!(out.contains(&refusal("a.rs", 3)), "{out}");
}

#[test]
fn a_target_on_a_later_line_of_the_statement_is_flagged() {
    let t = tmp("b");
    let src = concat!(
        "fn k(pid: u32) {\n",
        "    let _ = BUILDER\n",
        "        .args([\"-s\", \"KILL\", \"--\", &format!(\"-{pid}\")])\n",
        "        .status();\n",
        "}\n"
    )
    .replace("BUILDER", &builder());
    put(&t.0, "b.rs", src.as_bytes());
    let (code, out) = scan(&t.0, &["b.rs"]);
    assert_eq!(code, Some(1), "{out}");
    assert!(out.contains(&refusal("b.rs", 2)), "{out}");
}

#[test]
fn the_in_process_call_and_a_positive_pid_pass() {
    let t = tmp("c");
    let src = concat!(
        "fn k(pid: u32) {\n",
        "    // BUILDER.args([\"-KILL\", &format!(\"-{pid}\")]) in a comment is not read\n",
        "    unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };\n",
        "    let _ = BUILDER.args([\"-KILL\", &pid.to_string()]).status();\n",
        "}\n"
    )
    .replace("BUILDER", &builder());
    put(&t.0, "c.rs", src.as_bytes());
    let (code, out) = scan(&t.0, &["c.rs"]);
    assert_eq!(code, Some(0), "{out}");
}

#[test]
fn a_shell_file_flags_the_external_program_not_the_builtin() {
    let t = tmp("d");
    let src = concat!(
        "#!/bin/sh\n",
        "EXT -9 -$pgid\n",
        "kill -9 -$pgid\n",
        "pkill -TERM -12345\n",
        "# EXT -9 -$pgid is a comment\n",
        "EXT -9 4242\n",
    )
    .replace("EXT", &ext_kill());
    put(&t.0, "d.sh", src.as_bytes());
    let (code, out) = scan(&t.0, &["d.sh"]);
    assert_eq!(code, Some(1), "{out}");
    assert!(out.contains(&refusal("d.sh", 2)), "{out}");
    assert!(!out.contains(&refusal("d.sh", 3)), "{out}");
    assert!(out.contains(&refusal("d.sh", 4)), "{out}");
    assert!(!out.contains(&refusal("d.sh", 5)), "{out}");
    assert!(!out.contains(&refusal("d.sh", 6)), "{out}");
}

#[test]
fn a_shebang_script_without_an_extension_is_read() {
    let t = tmp("shebang");
    put(
        &t.0,
        "run-it",
        format!(
            "#!/usr/bin/env bash\nxargs kill -9 -${{g}}\n{} -9 -$x\n",
            ext_kill()
        )
        .as_bytes(),
    );
    let (code, out) = scan(&t.0, &["run-it"]);
    assert_eq!(code, Some(1), "{out}");
    assert!(out.contains(&refusal("run-it", 2)), "{out}");
    assert!(out.contains(&refusal("run-it", 3)), "{out}");
}

#[test]
fn a_workflow_line_is_flagged() {
    let t = tmp("e");
    let src = "jobs:\n  x:\n    steps:\n      - run: EXT -KILL -${pid}\n"
        .replace("EXT", &["/bin/", "kill"].concat());
    put(&t.0, ".github/workflows/w.yml", src.as_bytes());
    let (code, out) = scan(&t.0, &[".github/workflows/w.yml"]);
    assert_eq!(code, Some(1), "{out}");
    assert!(
        out.contains(&refusal(".github/workflows/w.yml", 4)),
        "{out}"
    );
}

#[test]
fn an_allow_entry_with_a_reason_skips_the_path() {
    let t = tmp("f1");
    let src = "#!/bin/sh\nEXT -9 -$g\n".replace("EXT", &ext_kill());
    put(&t.0, "x.sh", src.as_bytes());
    put(&t.0, "y.sh", src.as_bytes());
    put(
        &t.0,
        "tools/ci/kill-scan-allow.txt",
        b"# one path per line\nx.sh\ta fixture that must keep the text\n",
    );
    let (code, out) = scan(&t.0, &["x.sh", "y.sh"]);
    assert_eq!(code, Some(1), "{out}");
    assert!(!out.contains(&refusal("x.sh", 2)), "{out}");
    assert!(out.contains(&refusal("y.sh", 2)), "{out}");
    let (code, out) = scan(&t.0, &["x.sh"]);
    assert_eq!(code, Some(0), "{out}");
}

#[test]
fn an_allow_entry_without_a_reason_is_an_error() {
    let t = tmp("f2");
    put(&t.0, "x.sh", b"#!/bin/sh\necho ok\n");
    for body in [&b"x.sh\n"[..], &b"x.sh\t\n"[..], &b"x.sh\t   \n"[..]] {
        put(&t.0, "tools/ci/kill-scan-allow.txt", body);
        let (code, out) = scan(&t.0, &["x.sh"]);
        assert_eq!(code, Some(2), "{out}");
        assert!(out.contains("needs a reason"), "{out}");
    }
}

#[test]
fn a_binary_file_is_skipped_and_said_so() {
    let t = tmp("g");
    let mut bin = b"\x7fELF\0\0".to_vec();
    bin.extend_from_slice(format!("{} -9 -$g\n", ext_kill()).as_bytes());
    put(&t.0, "tool.sh", &bin);
    let (code, out) = scan(&t.0, &["tool.sh"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(
        out.contains("kill-scan: skipped tool.sh: binary file"),
        "{out}"
    );
}

#[test]
fn no_files_reads_every_tracked_file() {
    let t = tmp("tracked");
    let git = |args: &[&str]| {
        let s = Command::new("git")
            .args(args)
            .current_dir(&t.0)
            .output()
            .unwrap();
        assert!(s.status.success(), "git {args:?}");
    };
    git(&["init", "-q"]);
    put(
        &t.0,
        "bad.sh",
        format!("#!/bin/sh\n{} -9 -$g\n", ext_kill()).as_bytes(),
    );
    put(
        &t.0,
        "untracked.sh",
        format!("#!/bin/sh\n{} -9 -$g\n", ext_kill()).as_bytes(),
    );
    git(&["add", "bad.sh"]);
    let (code, out) = scan(&t.0, &[]);
    assert_eq!(code, Some(1), "{out}");
    assert!(out.contains(&refusal("bad.sh", 2)), "{out}");
    assert!(!out.contains("untracked.sh"), "{out}");
}
