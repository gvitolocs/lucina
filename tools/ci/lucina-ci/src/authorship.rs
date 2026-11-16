//! Authorship by content: which model shaped a unit's code and which shaped its statement and proof text.
//!
//! One rule (`CodeAuthors(U) ∩ ProofAuthors(U) = ∅`), decided by ONE walk of `LB..HEAD` that compares the two token streams of every
//! file version (docs/ci/PROOF-ROLES.md). Registration, formatting, comments, doc comments and pure moves touch no stream and are
//! therefore neutral by construction; what rustc compiles is the code side and what only the verifier reads is the proof side.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Instant;

use crate::roles;
use crate::spec::fnv1a;

/// Only the entries of this repository decide authorship.
pub const LEDGER_REPO: &str = "LucinaOS";
/// The environment variable naming the jsonl ledger that `lagent` appends to, one line per commit.
pub const LEDGER_ENV: &str = "LAGENT_COMMIT_LEDGER";
/// From which commit id every commit of `base..HEAD` must have a ledger entry. `None` while the lanes do not stamp them all:
/// the trailers still decide, and PROOF-ROLES.md names the open gap this leaves.
pub const LEDGER_REQUIRED_FROM: Option<&str> = None;

const ZERO: &str = "0000000000000000000000000000000000000000";

/// The two streams of one file version, as 64-bit hashes. `ok` is false for a version that cannot be classified: the comparison
/// then counts on BOTH sides (fail closed: nobody can show what the commit moved).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Streams {
    pub code: u64,
    pub proof: u64,
    pub ok: bool,
}

/// An absent file: empty streams.
const ABSENT: Streams = Streams {
    code: 0,
    proof: 0,
    ok: true,
};

impl Streams {
    fn diff(&self, b: &Streams) -> (bool, bool) {
        if !self.ok || !b.ok {
            (true, true)
        } else {
            (self.code != b.code, self.proof != b.proof)
        }
    }
}

/// The streams of one file version. A file of a model unit is proof alone: the whole normalised text, as the freeze fingerprints it.
pub fn streams(path: &str, src: &str, model: bool) -> Streams {
    if model {
        let t = roles::model_text(path, src);
        return Streams {
            code: 0,
            proof: if t.trim().is_empty() { 0 } else { fnv1a(&t) },
            ok: true,
        };
    }
    let c = roles::classify(path, src);
    if !c.errors.is_empty() {
        return Streams {
            code: !0,
            proof: !0,
            ok: false,
        };
    }
    // an empty side hashes to 0, the hash of an absent file: creating a file empty authors nothing
    Streams {
        code: if c.author_code.is_empty() {
            0
        } else {
            fnv1a(&c.author_code.join(" "))
        },
        proof: if c.author_proof.trim().is_empty() {
            0
        } else {
            fnv1a(&c.author_proof)
        },
        ok: true,
    }
}

/// Where `models(C)` came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Ledger,
    Attribution,
    Trailer,
    None,
}

impl Source {
    pub fn label(&self) -> &'static str {
        match self {
            Source::Ledger => "Ledger",
            Source::Attribution => "Attribution",
            Source::Trailer => "Trailer",
            Source::None => "no ledger entry, attribution line or trailer",
        }
    }
}

/// The first ten characters of a commit id, as every message names it (roles 1.1).
pub fn short(id: &str) -> String {
    id.chars().take(10).collect()
}

/// One commit that shaped a unit.
#[derive(Clone, Debug)]
pub struct Authored {
    pub commit: String,
    /// The models of the commit; empty when nothing names one (a trailer-less commit).
    pub models: Vec<String>,
    pub source: Source,
    pub code: bool,
    pub proof: bool,
    /// The unit paths the commit moved (for the message).
    pub paths: Vec<String>,
    /// In `base..HEAD` (the range under review) rather than in the history below it.
    pub in_range: bool,
}

/// Who shaped one unit since the lower bound.
#[derive(Clone, Debug, Default)]
pub struct UnitAuthors {
    pub commits: Vec<Authored>,
    /// The implementer of a covering `pre-rule` record: it stands for the exempt code (roles 1.5).
    pub pre_rule_impl: Option<String>,
    /// The `started_before` of that record: the commit the exempt past ends at, which is what the implementer wrote code UP TO
    /// (review 4, message nit - the exempt work sits between the lower bound and this commit, not below the lower bound).
    pub pre_rule_start: Option<String>,
    /// The `code_author` of the exemption line that dropped this unit's past: it stands for the exempt code the same way, so an
    /// approval of a unit's past cannot hand a later proof edit of the same model a clean sheet (review 2, New 2). `not
    /// established` in the line names nobody and is read as nothing.
    pub exempt_code: Option<String>,
}

/// The whole walk of one run, plus what it cost.
#[derive(Debug, Default)]
pub struct Walk {
    /// The lower bound the walk started from (line 1 of the cutoff list, else the constant).
    pub lb: String,
    /// The lower bound is not an object of this repository (a shallow clone): the walk could not run.
    pub lb_missing: bool,
    pub units: BTreeMap<String, UnitAuthors>,
    pub notes: Vec<String>,
    /// The ledger chain did not verify: the reason. Refused at the merge gate, a note elsewhere.
    pub ledger_broken: Option<String>,
    /// A commit the ledger and the attribution file name differently: (commit, why).
    pub contradictions: Vec<(String, String)>,
    /// A merge whose authorship could not be read at all: neither the automatic merge nor a comparison with any parent.
    pub merge_unreadable: Vec<String>,
    pub commits: usize,
    pub blobs: usize,
    pub secs: f64,
}

impl Walk {
    pub fn authors(&self, unit: &str) -> &UnitAuthors {
        static EMPTY: std::sync::OnceLock<UnitAuthors> = std::sync::OnceLock::new();
        self.units
            .get(unit)
            .unwrap_or_else(|| EMPTY.get_or_init(UnitAuthors::default))
    }
}

/// The models that shaped the code since the lower bound: model -> the commits it was seen in.
pub fn code_authors(u: &UnitAuthors) -> BTreeMap<String, Vec<String>> {
    authored_by(u, true)
}

/// The models that shaped the statement or proof text since the lower bound.
pub fn proof_authors(u: &UnitAuthors) -> BTreeMap<String, Vec<String>> {
    authored_by(u, false)
}

fn authored_by(u: &UnitAuthors, code: bool) -> BTreeMap<String, Vec<String>> {
    let mut m: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for a in &u.commits {
        if (code && a.code) || (!code && a.proof) {
            if a.models.is_empty() {
                m.entry(String::new()).or_default().push(short(&a.commit));
                continue;
            }
            for x in &a.models {
                m.entry(x.clone()).or_default().push(short(&a.commit));
            }
        }
    }
    if let Some(pi) = u.pre_rule_impl.as_ref().filter(|_| code) {
        m.entry(pi.clone()).or_default();
    }
    for x in u.exempt_code.iter().filter(|_| code) {
        m.entry(x.clone()).or_default();
    }
    for v in m.values_mut() {
        v.sort();
        v.dedup();
    }
    m
}

/// What the per-commit rules of `check` read: the same classification, restricted to `base..HEAD` (roles 1.6).
pub fn range_change(u: &UnitAuthors) -> roles::ChangeInfo {
    let mut info = roles::ChangeInfo::default();
    for a in u.commits.iter().filter(|a| a.in_range) {
        if a.code {
            info.exec.push(a.models.clone());
        }
        if a.proof {
            info.proof.push((short(&a.commit), a.models.clone()));
        }
    }
    info
}

/// Who the model of a commit is: the ledger, then the attribution file of the base, then the trailers (roles 1.3).
#[derive(Clone, Default)]
pub struct ModelResolver {
    pub ledger: BTreeMap<String, String>,
    /// The chain did not verify: the reason, with the line.
    pub ledger_broken: Option<String>,
    pub attrib: BTreeMap<String, String>,
    pub ledger_lines: usize,
}

impl ModelResolver {
    /// `ledger` is the file named by `LAGENT_COMMIT_LEDGER` (`None` when unset), `attrib_base` the attribution file as it is at BASE.
    pub fn new(ledger: Option<&Path>, attrib_base: Option<&str>) -> ModelResolver {
        let mut r = ModelResolver {
            attrib: roles::attribution_map(attrib_base),
            ..Default::default()
        };
        let Some(p) = ledger else { return r };
        match std::fs::read(p) {
            Err(e) => r.ledger_broken = Some(format!("{}: {e}", p.display())),
            Ok(b) => match verify_ledger(&b) {
                Err(e) => r.ledger_broken = Some(format!("{}: {e}", p.display())),
                Ok((m, lines)) => {
                    r.ledger = m;
                    r.ledger_lines = lines;
                }
            },
        }
        r
    }

    /// The model of a commit and where it came from; the third field names a contradiction with the attribution file.
    pub fn models(&self, commit: &str, msg: &str) -> (Vec<String>, Source, Option<String>) {
        let l = self.ledger.get(commit);
        let a = self.attrib.get(commit);
        let contra = match (l, a) {
            (Some(x), Some(y)) if x != y => Some(format!(
                "the ledger names {x} and the attribution file names {y} for the same commit"
            )),
            _ => None,
        };
        if let Some(m) = l {
            return (vec![m.clone()], Source::Ledger, contra);
        }
        if let Some(m) = a {
            return (vec![m.clone()], Source::Attribution, contra);
        }
        let t = roles::trailer_models(msg);
        let s = if t.is_empty() {
            Source::None
        } else {
            Source::Trailer
        };
        (t, s, contra)
    }
}

/// The jsonl ledger: every line's `prev` is the lowercase hex SHA-256 of the previous line as stored (its newline is not
/// hashed), 64 zeros for the first, and the file ends with a newline. Returns the entries of `LEDGER_REPO` (commit -> normalised model) and the number of lines.
pub fn verify_ledger(b: &[u8]) -> Result<(BTreeMap<String, String>, usize), String> {
    if b.is_empty() {
        return Ok((BTreeMap::new(), 0));
    }
    if !b.ends_with(b"\n") {
        return Err("the last line has no trailing newline".into());
    }
    let mut lines: Vec<&[u8]> = b.split(|&c| c == b'\n').collect();
    lines.pop();
    let mut prev = "0".repeat(64);
    let mut m: BTreeMap<String, String> = BTreeMap::new();
    for (i, raw) in lines.iter().enumerate() {
        let n = i + 1;
        if raw.is_empty() {
            return Err(format!(
                "line {n} is empty: the ledger holds one object per line"
            ));
        }
        let line = String::from_utf8_lossy(raw);
        if !line.trim_start().starts_with('{') {
            return Err(format!("line {n} is not a JSON object"));
        }
        let get = |k: &str| json_field(&line, k);
        let Some(v) = get("v") else {
            return Err(format!("line {n} has no `v`"));
        };
        if v != "1" {
            return Err(format!("line {n}: `v` is {v}, expected 1"));
        }
        for k in ["commit", "repo", "model", "prev"] {
            if get(k).is_none() {
                return Err(format!("line {n} has no `{k}`"));
            }
        }
        let pr = get("prev").unwrap_or_default();
        if pr != prev {
            return Err(format!(
                "line {n}: `prev` is {pr}, expected the SHA-256 of line {} ({prev})",
                i
            ));
        }
        let repo = get("repo").unwrap_or_default();
        if repo == LEDGER_REPO {
            let commit = get("commit").unwrap_or_default().to_lowercase();
            let model = roles::model_norm(&get("model").unwrap_or_default());
            m.insert(commit, model);
        }
        prev = sha256_hex(raw);
    }
    Ok((m, lines.len()))
}

/// The value of a key of a one-line JSON object, as text (a string without its quotes, a number as written). `None` when the object
/// does not hold the key at its top level. Plain std: the tool has no dependencies to spend on a JSON parser.
fn json_field(o: &str, key: &str) -> Option<String> {
    let c: Vec<char> = o.chars().collect();
    let want: Vec<char> = key.chars().collect();
    let mut depth = 0i32;
    let mut i = 0usize;
    let mut quote = false;
    let mut esc = false;
    while i < c.len() {
        let x = c[i];
        if esc {
            esc = false;
            i += 1;
            continue;
        }
        if quote {
            match x {
                '\\' => esc = true,
                '"' => quote = false,
                _ => {}
            }
            i += 1;
            continue;
        }
        match x {
            '{' | '[' => depth += 1,
            '}' | ']' => depth -= 1,
            '"' => {
                // a string: is it the key, at the top level of this object?
                let is_key = depth == 1
                    && c[i + 1..].starts_with(&want[..])
                    && c.get(i + 1 + want.len()) == Some(&'"')
                    && c[i + 2 + want.len()..]
                        .iter()
                        .position(|y| !y.is_whitespace())
                        .is_some_and(|p| c[i + 2 + want.len() + p] == ':');
                if is_key {
                    let mut j = i + 2 + want.len();
                    while j < c.len() && (c[j].is_whitespace() || c[j] == ':') {
                        j += 1;
                    }
                    return if c[j] == '"' {
                        let mut out = String::new();
                        j += 1;
                        while j < c.len() && c[j] != '"' {
                            if c[j] == '\\' && j + 1 < c.len() {
                                j += 1;
                                out.push(match c[j] {
                                    'n' => '\n',
                                    't' => '\t',
                                    'r' => '\r',
                                    other => other,
                                });
                            } else {
                                out.push(c[j]);
                            }
                            j += 1;
                        }
                        Some(out)
                    } else {
                        let e = c[j..]
                            .iter()
                            .position(|y| matches!(y, ',' | '}' | ']') || y.is_whitespace())
                            .map_or(c.len(), |p| j + p);
                        Some(c[j..e].iter().collect())
                    };
                }
                quote = true;
            }
            _ => {}
        }
        i += 1;
    }
    None
}

const SHA_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// FIPS 180-4 SHA-256 as lowercase hex, plain std (the ledger chain is verified here and nowhere else).
pub fn sha256_hex(msg: &[u8]) -> String {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut data: Vec<u8> = Vec::with_capacity(msg.len() + 72);
    data.extend_from_slice(msg);
    data.push(0x80);
    while data.len() % 64 != 56 {
        data.push(0);
    }
    data.extend_from_slice(&((msg.len() as u64) * 8).to_be_bytes());
    for ch in data.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([ch[4 * i], ch[4 * i + 1], ch[4 * i + 2], ch[4 * i + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(SHA_K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *x = x.wrapping_add(y);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

// ---------------------------------------------------------------------------------------------------- the walk

#[derive(Clone, Debug)]
struct Delta {
    /// The path after the commit.
    path: String,
    /// The source of a rename or copy.
    src: Option<String>,
    /// The blob the lineage path held at the parent (`None`: absent). For a rename, the source at the parent.
    before: Option<String>,
    /// The blob at the commit (`None`: deleted).
    after: Option<String>,
    /// The merge conflicted in this path.
    conflicted: bool,
}

impl Delta {
    /// The path the pre-image is classified under (the old name of a rename).
    fn old_path(&self) -> &str {
        self.src.as_deref().unwrap_or(&self.path)
    }
}

#[derive(Clone, Debug)]
struct Cmt {
    id: String,
    parents: Vec<String>,
    msg: String,
    deltas: Vec<Delta>,
    /// The tree of the computed automatic merge, the version a merge delta is compared against. `None` for a non-merge and for a
    /// merge whose automatic merge could not be computed (every one of its paths is then read as conflicted).
    auto: Option<String>,
}

/// One git call: its exit code, its stdout and its stderr.
fn git2(root: &Path, args: &[&str]) -> (i32, String, String) {
    match Command::new("git").args(args).current_dir(root).output() {
        Ok(o) => (
            o.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&o.stdout).into_owned(),
            String::from_utf8_lossy(&o.stderr).into_owned(),
        ),
        Err(e) => (-1, String::new(), e.to_string()),
    }
}

fn git_ok(root: &Path, args: &[&str]) -> Option<String> {
    let (st, out, _) = git2(root, args);
    (st == 0).then_some(out)
}

/// `%x01`-separated commit blocks of `git log --raw -M -C --no-abbrev --format=%x01%H %P%x00%B%x00`.
fn parse_log(out: &str) -> Vec<Cmt> {
    let mut v: Vec<Cmt> = Vec::new();
    for chunk in out.split('\u{1}') {
        if chunk.trim().is_empty() {
            continue;
        }
        let Some((hdr, rest)) = chunk.split_once('\0') else {
            continue;
        };
        let ws: Vec<&str> = hdr.split_whitespace().collect();
        if ws.first().is_none_or(|s| s.len() != 40) {
            continue;
        }
        let (msg, diff) = rest.split_once('\0').unwrap_or((rest, ""));
        v.push(Cmt {
            id: ws[0].to_string(),
            parents: ws[1..].iter().map(|s| s.to_string()).collect(),
            msg: msg.to_string(),
            deltas: parse_raw(diff),
            auto: None,
        });
    }
    v
}

/// The `--raw` records of one commit: `:<modes> <old> <new> <status>\t<path>` and, for a rename or copy, `<src>\t<dst>`.
fn parse_raw(diff: &str) -> Vec<Delta> {
    let nz = |s: &str| (s != ZERO).then(|| s.to_string());
    let mut out: Vec<Delta> = Vec::new();
    for l in diff.lines().filter(|x| x.starts_with(':')) {
        let Some((head, paths)) = l.split_once('\t') else {
            continue;
        };
        let f: Vec<&str> = head.split_whitespace().collect();
        if f.len() < 5 {
            continue;
        }
        let p: Vec<String> = paths.split('\t').map(str::to_string).collect();
        let st = f[4].chars().next().unwrap_or('M');
        let ren = st == 'R' || st == 'C';
        let path = match &p[..] {
            [_, dst] if ren => dst.clone(),
            [first, ..] => first.clone(),
            [] => continue,
        };
        out.push(Delta {
            src: ren.then(|| p[0].clone()),
            path,
            before: nz(f[2]),
            after: nz(f[3]),
            conflicted: false,
        });
    }
    out
}

/// One `git cat-file --batch` process: the content of every blob version the walk needs, read once.
struct Batch {
    child: Child,
    stdin: std::process::ChildStdin,
    rd: BufReader<std::process::ChildStdout>,
    pub n: usize,
}

impl Batch {
    fn new(root: &Path) -> Result<Batch, String> {
        let mut c = Command::new("git")
            .arg("cat-file")
            .arg("--batch")
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("git cat-file --batch: {e}"))?;
        let stdin = c.stdin.take().ok_or("git cat-file --batch: no stdin")?;
        let rd = BufReader::new(c.stdout.take().ok_or("git cat-file --batch: no stdout")?);
        Ok(Batch {
            child: c,
            stdin,
            rd,
            n: 0,
        })
    }

    fn content(&mut self, oid: &str) -> Option<String> {
        writeln!(self.stdin, "{oid}").ok()?;
        self.stdin.flush().ok()?;
        let mut head = String::new();
        if self.rd.read_line(&mut head).ok()? == 0 {
            return None;
        }
        if head.trim_end().ends_with("missing") {
            return None;
        }
        let size: usize = head.split_whitespace().nth(2)?.parse().ok()?;
        let mut buf = vec![0u8; size];
        self.rd.read_exact(&mut buf).ok()?;
        let mut eol = [0u8; 1];
        let _ = self.rd.read(&mut eol);
        self.n += 1;
        Some(String::from_utf8_lossy(&buf).into_owned())
    }
}

impl Drop for Batch {
    fn drop(&mut self) {
        // this process is ours: close its input and reap it
        let _ = self.child.stdin.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The streams of the file versions the walk reads, with the persistent cache of `check-fast` (never read at the merge gate).
struct Streams_ {
    mem: HashMap<(String, String, bool), Streams>,
    dirty: bool,
    path: Option<PathBuf>,
    /// A path whose blob the object store does not have: (path, oid). A missing object is not an absent file (review 8).
    missing: Vec<(String, String)>,
}

impl Streams_ {
    fn open(cache: Option<&Path>) -> Streams_ {
        let mut s = Streams_ {
            mem: HashMap::new(),
            dirty: false,
            path: cache.map(PathBuf::from),
            missing: Vec::new(),
        };
        if let Some(p) = &s.path
            && let Ok(text) = std::fs::read_to_string(p)
        {
            for l in text.lines() {
                let f: Vec<&str> = l.split('\t').collect();
                if f.len() != 5 {
                    continue;
                }
                let Some(code) = u64::from_str_radix(f[2], 16).ok() else {
                    continue;
                };
                let Some(proof) = u64::from_str_radix(f[3], 16).ok() else {
                    continue;
                };
                s.mem.insert(
                    (f[0].to_string(), f[1].to_string(), f[4] == "m"),
                    Streams {
                        code,
                        proof,
                        ok: f[2] != "error",
                    },
                );
            }
        }
        s
    }

    fn flush(&mut self) {
        if !self.dirty {
            return;
        }
        let Some(p) = &self.path else { return };
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let mut out = String::new();
        for ((path, oid, model), s) in self.mem.iter() {
            let (code, proof) = if s.ok {
                (format!("{:016x}", s.code), format!("{:016x}", s.proof))
            } else {
                ("error".to_string(), "error".to_string())
            };
            out.push_str(&format!(
                "{path}\t{oid}\t{code}\t{proof}\t{}\n",
                if *model { "m" } else { "r" }
            ));
        }
        let _ = std::fs::write(p, out);
        self.dirty = false;
    }

    fn of(
        &mut self,
        path: &str,
        oid: Option<&str>,
        model: bool,
        bd: &mut Batch,
    ) -> (Streams, Option<String>) {
        let Some(oid) = oid else {
            return (ABSENT, None);
        };
        let key = (path.to_string(), oid.to_string(), model);
        if let Some(s) = self.mem.get(&key) {
            return (*s, None);
        }
        let src = bd.content(oid);
        let s = match &src {
            Some(t) => streams(path, t, model),
            None => {
                // a blob the object store does not have (a shallow or grafted clone) is an UNKNOWN version, never the
                // empty file: it counts as a change on both sides, and the walk says so (review 8).
                if !self.missing.contains(&(path.to_string(), oid.to_string())) {
                    self.missing.push((path.to_string(), oid.to_string()));
                }
                Streams {
                    code: !0,
                    proof: !0,
                    ok: false,
                }
            }
        };
        self.mem.insert(key, s);
        self.dirty = true;
        (s, src)
    }
}

fn revset(root: &Path, spec: &str) -> HashSet<String> {
    git_ok(root, &["rev-list", spec])
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// The persistent stream cache of `check-fast`: `<git-common-dir>/lucina-ci/authors-<src id>.tsv`. The id is baked in at build time
/// from the extractor's own sources, so a new binary ignores an older cache and every worktree shares one file (blobs are immutable).
/// The merge gate never reads it: any agent may write under `.git`.
pub fn cache_path(root: &Path) -> Option<PathBuf> {
    let dir = git_ok(root, &["rev-parse", "--git-common-dir"])?
        .trim()
        .to_string();
    let d = Path::new(&dir);
    let d = if d.is_absolute() {
        d.to_path_buf()
    } else {
        root.join(d)
    };
    Some(
        d.join("lucina-ci")
            .join(format!("authors-{}.tsv", env!("LUCINA_CI_SRC_ID"))),
    )
}

/// The automatic merge of a merge commit: its tree and the paths that conflicted.
fn auto_merge(root: &Path, parents: &[String]) -> Option<(String, BTreeSet<String>)> {
    if parents.len() < 2 {
        return None;
    }
    let (st, out, _) = git2(
        root,
        &[
            "merge-tree",
            "--write-tree",
            "--name-only",
            &parents[0],
            &parents[1],
        ],
    );
    if st != 0 && st != 1 {
        return None;
    }
    let mut lines = out.lines();
    let tree = lines.next()?.trim().to_string();
    if !hex(&tree) {
        return None;
    }
    Some((tree, lines.map(str::to_string).collect()))
}

fn hex(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn blob_of(root: &Path, rev: &str, path: &str) -> Option<String> {
    let (st, out, _) = git2(root, &["rev-parse", &format!("{rev}:{path}")]);
    if st != 0 {
        return None;
    }
    let o = out.trim().to_string();
    (o != ZERO).then_some(o)
}

/// A copy of a file the commit leaves alone: `-C` does not look at unchanged sources, so the same blob under another name at the
/// parent is that copy (roles 1.4). Only a source of the same kind, or a coincidental twin would drag an unrelated file into the
/// lineage.
fn same_blob_at(root: &Path, rev: &str, path: &str, oid: &str) -> Option<String> {
    if oid.len() != 40 || roles::full_id(path) {
        return None;
    }
    let ext = path.rsplit('.').next().unwrap_or("");
    let out = git_ok(root, &["ls-tree", "-r", rev])?;
    let mut best: Option<String> = None;
    for l in out.lines() {
        // "<mode> <type> <oid>\t<path>"
        let Some((meta, p)) = l.split_once('\t') else {
            continue;
        };
        if p == path || p.rsplit('.').next().unwrap_or("") != ext {
            continue;
        }
        let m: Vec<&str> = meta.split(' ').collect();
        if m.len() == 3
            && m[1] == "blob"
            && m[2] == oid
            && best.as_ref().is_none_or(|b| p < b.as_str())
        {
            best = Some(p.to_string());
        }
    }
    best
}

/// The path this commit deletes and hands its content to `path`: `git rm f` and `git mv g f` in one commit is reported as a
/// modification of `f` and the deletion of `g` with no rename, so the blob is the only thing that ties them (review K1).
/// Only a source of the same extension counts, as in `same_blob_at`.
fn deleted_blob_of(deltas: &[Delta], path: &str, oid: &str) -> Option<String> {
    if oid.len() != 40 || roles::full_id(path) {
        return None;
    }
    let ext = path.rsplit('.').next().unwrap_or("");
    deltas
        .iter()
        .filter(|d| d.path != path && d.after.is_none() && d.src.is_none())
        .find(|d| {
            d.before.as_deref() == Some(oid) && d.path.rsplit('.').next().unwrap_or("") == ext
        })
        .map(|d| d.path.clone())
}

/// Every version the walked log shows, by blob: a path and the index of the commit that carried it, newest commit first. It is
/// built once per walk from the deltas already parsed, so it costs no git call.
type Seen = BTreeMap<String, Vec<(String, usize)>>;

/// What the occurrence map is read against: the walked commits, with the git answer memoised per (occurrence, parent).
struct Matcher<'a> {
    root: &'a Path,
    commits: &'a [Cmt],
    seen: &'a Seen,
    past: BTreeMap<(String, String), bool>,
}

impl Matcher<'_> {
    /// Whether the commit of the occurrence at index `j` is in the past of the commit judged at index `at`, whose parents are
    /// `parents`: one of its parents' ancestors, or the very commit (the source of a `cp` that stays in the same commit, K1c).
    ///
    /// Topological order does not say so on its own. `git log --topo-order` lists a commit of a PARALLEL branch BELOW the judged
    /// one, where the walk reads it as older, so `j >= at` alone accepts a copy of a blob nobody merged from: a Sonnet commit on
    /// another branch that recreates Opus's body launders the authorship (review 4, K1par), and a test fixture committed on this
    /// branch reads as the source of a proof commit on the branch coming in through a merge (the `slide_rules` regression).
    /// Ancestry is the fact the copy matching needs, and a merge counts every one of its parents.
    fn in_past(&mut self, j: usize, at: usize, parents: &[String]) -> bool {
        if j == at {
            return true;
        }
        let x = self.commits[j].id.clone();
        for p in parents {
            let key = (x.clone(), p.clone());
            let hit = match self.past.get(&key) {
                Some(v) => *v,
                None => {
                    let v = roles::is_ancestor_of(self.root, &x, p);
                    self.past.insert(key, v);
                    v
                }
            };
            if hit {
                return true;
            }
        }
        false
    }

    /// A version of this blob at ANOTHER path in this commit or an older one of the walked history: the content of `path` came from
    /// there, so the commit that put it here authored nothing of it, and that path joins the lineage (roles 1.4, review K1c/K1d/K1e).
    /// `deleted_blob_of` and `same_blob_at` only see what the commit itself did: a `cp staged.rs rules.rs` that leaves the source
    /// alone, or rewrites it, is a plain modification of the unit file with no deletion and no rename, and without this the copier is
    /// credited with the code the stager wrote.
    ///
    /// No extension is required: the match is on the blob, and an identical blob IS identical content, whatever the source was named
    /// (`cp staged.txt rules.rs` is the same laundering as `cp staged.rs rules.rs`, review K1e). The name still travels with the
    /// content into the lineage, where the streams are read from it; what the name does NOT decide is whether the version counts,
    /// which is decided by ancestry (`in_past`), never by the order the log happened to be printed in.
    fn predecessor(
        &mut self,
        at: usize,
        parents: &[String],
        path: &str,
        oid: &str,
    ) -> Option<String> {
        if oid.len() != 40 || roles::full_id(path) {
            return None;
        }
        let mut cands: Vec<(usize, String)> = self
            .seen
            .get(oid)?
            .iter()
            .filter(|(p, j)| *j >= at && p != path)
            .map(|(p, j)| (*j, p.clone()))
            .collect();
        cands.sort_by_key(|(j, _)| *j);
        for (j, p) in cands {
            if self.in_past(j, at, parents) {
                return Some(p);
            }
        }
        None
    }
}

/// The authorship walk of one run: `LB..HEAD` in topological order, every unit at once (roles 2).
///
/// `pre` maps a unit to the `started_before` of its covering `pre-rule` record; the cutoff list is read only together with the
/// exemption line the coordinator named for the very commit the list fixes (`roles::exempt_past`): the list alone exempts nobody.
#[allow(clippy::too_many_arguments)]
pub fn walk(
    root: &Path,
    base: &str,
    units: &[roles::UnitFp],
    cutoff: &roles::Cutoff,
    pre: &BTreeMap<String, String>,
    r: &ModelResolver,
    cache: Option<&Path>,
) -> Result<Walk, String> {
    let t0 = Instant::now();
    let lb = if roles::full_id(&cutoff.commit) {
        cutoff.commit.clone()
    } else {
        roles::HISTORY_LOWER_BOUND.to_string()
    };
    let mut w = Walk {
        lb,
        ..Default::default()
    };
    if !roles::commit_exists(root, &w.lb) {
        // the caller decides: a refusal at the merge gate, a note elsewhere
        w.lb_missing = true;
        return Ok(w);
    }
    // The attribution file corrects commits that were already on main at the base; a branch cannot re-attribute its own range.
    let base_anc = revset(root, base);
    let mut res = r.clone();
    res.attrib.retain(|c, _| base_anc.contains(c));
    w.ledger_broken = res.ledger_broken.clone();

    // 1. the commits, non-merges with their rename- and copy-aware deltas
    let (st, out, err) = git2(
        root,
        &[
            "log",
            "--topo-order",
            "-M",
            "-C",
            "--raw",
            "--no-abbrev",
            "--diff-merges=off",
            "--format=%x01%H %P%x00%B%x00",
            &format!("{}..HEAD", w.lb),
        ],
    );
    if st != 0 {
        let why = err.trim().replace('\n', " ");
        return Err(format!(
            "git log {}..HEAD failed ({st}): {}",
            short(&w.lb),
            if why.is_empty() { "no message" } else { &why }
        ));
    }
    // newest first, as git lists them: the lineage of a unit grows backwards over renames and copies (roles 1.4)
    let mut commits = parse_log(&out);
    // 2. merges: what the automatic merge would have been, and what the commit made of it
    for c in &mut commits {
        if c.parents.len() < 2 {
            continue;
        }
        // The automatic merge is the version a merge is compared against. Where it cannot be computed (unrelated histories,
        // a git too old for `merge-tree --write-tree`) the merge is read against every parent, exactly as a conflicted path
        // is: it authors a stream only where its result differs from all of its parents (spec 54, review I3b).
        let (auto, conf) = match auto_merge(root, &c.parents) {
            Some(x) => x,
            None => {
                w.notes.push(format!(
                    "{}: the automatic merge of its parents could not be computed; every path it changed is read against every parent",
                    short(&c.id)
                ));
                (String::new(), BTreeSet::new())
            }
        };
        c.deltas = if auto.is_empty() {
            let mut by_path: BTreeMap<String, Delta> = BTreeMap::new();
            let mut read = false;
            for p in &c.parents {
                let (st, out, _) = git2(
                    root,
                    &["diff-tree", "-r", "--raw", "-M", "--no-abbrev", p, &c.id],
                );
                if st != 0 {
                    continue;
                }
                read = true;
                for d in parse_raw(&out) {
                    by_path
                        .entry(format!("{}\u{1}{}", d.path, d.src.as_deref().unwrap_or("")))
                        .or_insert(d);
                }
            }
            if !read {
                // not even a parent comparison: such a merge is nobody's work only on paper
                w.merge_unreadable.push(short(&c.id));
            }
            by_path.into_values().collect()
        } else {
            parse_raw(
                &git_ok(
                    root,
                    &[
                        "diff-tree",
                        "-r",
                        "--raw",
                        "-M",
                        "--no-abbrev",
                        &auto,
                        &c.id,
                    ],
                )
                .unwrap_or_default(),
            )
        };
        let many = c.parents.len() != 2;
        let every_parent = auto.is_empty();
        c.auto = (!auto.is_empty()).then_some(auto.clone());
        for d in &mut c.deltas {
            d.conflicted = many
                || every_parent
                || conf.contains(&d.path)
                || d.src.as_ref().is_some_and(|s| conf.contains(s));
        }
    }
    w.commits = commits.len();
    // every version of every blob the log shows, newest commit first: what a copy or a move can have come from (roles 1.4)
    let mut seen: Seen = BTreeMap::new();
    for (i, c) in commits.iter().enumerate() {
        for d in &c.deltas {
            if let Some(o) = &d.before {
                seen.entry(o.clone())
                    .or_default()
                    .push((d.old_path().to_string(), i));
            }
            if let Some(o) = &d.after {
                seen.entry(o.clone()).or_default().push((d.path.clone(), i));
            }
        }
    }
    let range = revset(root, &format!("{base}..HEAD"));
    // 3. streams and lineage
    let mut m = Matcher {
        root,
        commits: &commits,
        seen: &seen,
        past: BTreeMap::new(),
    };
    let mut bd = Batch::new(root)?;
    let mut sc = Streams_::open(cache);
    let mut anc: BTreeMap<String, HashSet<String>> = BTreeMap::new();
    let listed: BTreeMap<String, String> = cutoff
        .units
        .iter()
        .map(|(u, c, _)| (u.clone(), c.clone()))
        .collect();
    for u in units
        .iter()
        .filter(|x| x.fp.is_some() && roles::tracked(&x.kind))
    {
        let model = matches!(u.kind.as_str(), "tla" | "models");
        // roles 1.5: the commits of a unit's exempt past are dropped from both sets. The past of a unit is exempt only through its
        // valid `pre-rule` record, or through an exemption the coordinator named for its cutoff line (review 5, N), whose range
        // starts at `from_commit` exclusive. The cutoff list alone, with neither, exempts nothing.
        let since = roles::exempt_past(&u.unit, pre, &listed, &cutoff.exempt);
        // An exemption line carries what its author fields say about the range it approves. The `code_author` stands for the
        // exempt code, exactly as a `pre-rule` record's implementer does, so approving a unit's past cannot hand the same model's
        // later proof edit a clean sheet (review 2, New 2). `not established` names nobody and counts as nothing.
        let exempt_code: Option<String> = match since {
            Some((to, Some(_))) => cutoff
                .exempt
                .iter()
                .find(|e| e.unit == u.unit && e.to == to)
                .filter(|e| e.code_author != roles::NOT_ESTABLISHED)
                .map(|e| roles::model_norm(&e.code_author)),
            _ => None,
        };
        let ids: Vec<String> = match since {
            Some((to, Some(f))) => vec![to.to_string(), f.to_string()],
            Some((to, None)) => vec![to.to_string()],
            None => Vec::new(),
        };
        for k in &ids {
            anc.entry(k.clone()).or_insert_with_key(|k| revset(root, k));
        }
        let to_set: Option<&HashSet<String>> = ids.first().map(|k| &anc[k]);
        let from_set: Option<&HashSet<String>> = ids.get(1).map(|k| &anc[k]);
        // the lineage paths of the unit, growing backwards over renames and copies (roles 1.4)
        let mut live: HashSet<String> = u.files.iter().cloned().collect();
        let mut rec: Vec<Authored> = Vec::new();
        for (ci, c) in commits.iter().enumerate() {
            if c.deltas.is_empty() {
                continue;
            }
            if to_set
                .is_some_and(|a| a.contains(&c.id) && !from_set.is_some_and(|f| f.contains(&c.id)))
            {
                continue;
            }
            let mut touched: Vec<&Delta> = Vec::new();
            for d in &c.deltas {
                if !live.contains(&d.path) && !d.src.as_ref().is_some_and(|s| live.contains(s)) {
                    continue;
                }
                touched.push(d);
                if let Some(s) = &d.src
                    && !model
                {
                    live.insert(s.clone());
                }
            }
            if touched.is_empty() {
                continue;
            }
            // the tree the deltas of this commit are compared against: the parent of a non-merge, the automatic merge of a merge
            let cmp_tree: Option<&String> = if c.parents.len() == 1 {
                c.parents.first()
            } else {
                c.auto.as_ref()
            };
            let (mut code, mut proof) = (false, false);
            // code a copy match took off this commit: it stays with a STRONG copier (review 5, B4/B4r), see below
            let mut copied_code = false;
            let mut paths: Vec<String> = Vec::new();
            for d in touched {
                let after = sc.of(&d.path, d.after.as_deref(), model, &mut bd).0;
                // the versions the commit is compared against: the parent for a non-merge, the automatic merge for a merge,
                // every parent when the path conflicted
                let mut olds: Vec<Streams> = Vec::new();
                if d.conflicted {
                    for p in &c.parents {
                        let old = blob_of(root, p, d.old_path());
                        olds.push(sc.of(d.old_path(), old.as_deref(), model, &mut bd).0);
                    }
                } else if d.src.is_none()
                    && d.after.as_deref().is_some_and(|o| {
                        // the versions a move leaves no trace of: a copy of a file the commit leaves alone (no `before`, so
                        // `-C` sees no source), or a file moved or copied OVER this one in the same commit, which git reports
                        // as a modification and nothing else (review K1, K1c, K1d, K1m). The tree the delta is compared
                        // against is the parent of a non-merge and the automatic merge of a merge: a `cp staged.rs rules.rs`
                        // done INSIDE the merge commit is the same laundering as one done in a commit of its own.
                        let twin = if d.before.is_none() {
                            cmp_tree.and_then(|t| same_blob_at(root, t, &d.path, o))
                        } else {
                            deleted_blob_of(&c.deltas, &d.path, o)
                        };
                        if let Some(x) = twin.or_else(|| m.predecessor(ci, &c.parents, &d.path, o))
                        {
                            if !model {
                                live.insert(x);
                            }
                            true
                        } else {
                            false
                        }
                    })
                {
                    // the content came from elsewhere: the source is lineage, and this commit authors nothing of it
                    let own = sc.of(d.old_path(), d.before.as_deref(), model, &mut bd).0;
                    if after.diff(&own).0 {
                        copied_code = true;
                    }
                    olds.push(after);
                } else {
                    olds.push(sc.of(d.old_path(), d.before.as_deref(), model, &mut bd).0);
                }
                if olds.is_empty() {
                    olds.push(ABSENT);
                }
                let (dc, dp) = olds
                    .iter()
                    .map(|x| after.diff(x))
                    .fold((true, true), |a, x| (a.0 && x.0, a.1 && x.1));
                code |= dc;
                proof |= dp;
                paths.push(d.path.clone());
            }
            let (models, source, contra) = res.models(&c.id, &c.msg);
            // A copy match may ADD the source's author to the unit's code credit; it never REMOVES the credit of the commit judged
            // where that commit is a STRONG model's (review 5, B4/B4r): cherry-pick and rebase re-parent a strong model's commit
            // onto a builder's scratch copy of its own bytes, so the match would otherwise hand the code to the builder. A builder
            // that copies still authors nothing (K1), so only strong copiers keep the credit.
            if copied_code && !code && models.iter().any(|m| roles::class(m) == "strong") {
                code = true;
            }
            if !code && !proof {
                continue;
            }
            paths.sort();
            if let Some(why) = contra {
                let e = (short(&c.id), why);
                if !w.contradictions.contains(&e) {
                    w.contradictions.push(e);
                }
            }
            rec.push(Authored {
                commit: c.id.clone(),
                models,
                source,
                code,
                proof,
                paths,
                in_range: range.contains(&c.id),
            });
        }
        // the story of the unit, oldest first: what the messages and the range rules read
        rec.reverse();
        // A unit with no walked commit at all still enters the walk when an exemption line names who wrote the past it dropped:
        // the author of exempt code is a fact about the unit, not a leftover of the range (review 2, New 2).
        if !rec.is_empty() || exempt_code.is_some() {
            w.units.insert(
                u.unit.clone(),
                UnitAuthors {
                    commits: rec,
                    pre_rule_impl: None,
                    pre_rule_start: None,
                    exempt_code,
                },
            );
        }
    }
    for (p, o) in std::mem::take(&mut sc.missing) {
        w.notes
            .push(format!("`{p}`: the object {} is not in this repository, so its version is read as a change on both sides of every model that touched it", short(&o)));
    }
    sc.flush();
    w.blobs = bd.n;
    w.secs = t0.elapsed().as_secs_f64();
    Ok(w)
}
