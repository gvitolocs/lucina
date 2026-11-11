//! The binary end to end: make, sign (test key through the environment), verify.
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_lucina-release");
const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

fn run(args: &[&str], key: Option<&str>) -> (bool, String, String) {
    let mut c = Command::new(BIN);
    c.args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_remove("LUCINA_RELEASE_KEY");
    if let Some(k) = key {
        c.env("LUCINA_RELEASE_KEY", k);
    }
    let o = c.output().unwrap();
    (
        o.status.success(),
        String::from_utf8_lossy(&o.stdout).into(),
        String::from_utf8_lossy(&o.stderr).into(),
    )
}

#[test]
fn make_sign_verify() {
    let dir = std::env::temp_dir().join(format!("lucina-release-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = |n: &str| dir.join(n).to_str().unwrap().to_string();
    std::fs::write(p("img"), vec![0xabu8; 70000]).unwrap();

    let (ok, kg, _) = run(&["keygen-test"], None);
    assert!(ok);
    let field = |name: &str| {
        kg.lines()
            .find_map(|l| l.strip_prefix(name))
            .unwrap()
            .trim()
            .to_string()
    };
    let (seed, public) = (field("seed "), field("public "));
    std::fs::write(p("keys"), format!("{public}\n")).unwrap();

    let (ok, unsigned, e) = run(
        &[
            "make",
            "--board",
            "juno-sc7180",
            "--commit",
            COMMIT,
            "--date",
            "2026-11-11T11:39:59Z",
            "--seq",
            "3",
            "--chunk",
            "65536",
            "--write",
            "lun0:root-a",
            &p("img"),
        ],
        None,
    );
    assert!(ok, "{e}");
    std::fs::write(p("m.unsigned"), &unsigned).unwrap();

    let (ok, _, e) = run(&["sign", &p("m.unsigned")], None);
    assert!(!ok && e.contains("LUCINA_RELEASE_KEY is not set"));
    let (ok, signed, e) = run(&["sign", &p("m.unsigned")], Some(&seed));
    assert!(ok, "{e}");
    assert!(!signed.contains(&seed) && !e.contains(&seed));
    std::fs::write(p("m"), &signed).unwrap();

    let (ok, out, e) = run(&["verify", "--keys", &p("keys"), &p("m")], None);
    assert!(ok, "{e}");
    assert!(
        out.contains("board juno-sc7180")
            && out.contains("seq 3")
            && out.contains("size 70000")
            && out.contains("chunks 2")
    );

    std::fs::write(p("bad"), signed.replace("seq 3", "seq 4")).unwrap();
    let (ok, _, e) = run(&["verify", "--keys", &p("keys"), &p("bad")], None);
    assert!(!ok && !e.is_empty());
    std::fs::remove_dir_all(&dir).unwrap();
}
