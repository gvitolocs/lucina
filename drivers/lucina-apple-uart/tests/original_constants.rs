// SPDX-License-Identifier: GPL-2.0-only
//! Differential test of the Apple S5L UART register constants: ours in `src/regs.rs` against the same constants in the
//! original they were ported from, AsahiLinux/linux (branch `asahi`) at the pinned revision below.
//!
//! Run it on a machine that holds a clone of the original (the path is in `README.md`, "Provenance"):
//!
//! ```text
//! LUCINA_ASAHI_LINUX=/home/user/Projects/ReverseEngineering/asahi-m4/linux \
//!   cargo test --locked -p lucina-apple-uart --test original_constants
//! ```
//!
//! With `LUCINA_ASAHI_LINUX` unset the comparisons are skipped and the test passes, since CI and the other hosts have no
//! clone. The classification of our constants is checked either way; it needs only this repository.
//!
//! Nothing of the original is kept here: the tables name theirs and ours, and the original text is read at test time with
//! `git show` from the object store. The originals are GPL-2.0 (`samsung_tty.c`) and GPL-2.0+ (`serial_s3c.h`); reading them
//! and comparing two numbers takes no code into this tree.
//!
//! # How the original's value is obtained
//!
//! `#define NAME value` lines are read out of `include/linux/serial_s3c.h`; the right-hand side is expanded the way the C
//! preprocessor would and evaluated with C's precedence. The forms these two files use are hex and decimal literals,
//! `<< >> & | + - * / ~ !`, parentheses, and the kernel's `BIT`/`GENMASK`. Anything else is a hard error naming the text it
//! choked on, never a silent pass.
//!
//! Two kernel macros are given their meaning rather than their body, because their body is compile-time argument checking
//! around that meaning:
//!
//! - `GENMASK(h, l)`: the bits `h..l` set. `include/linux/bits.h:46-51` computes exactly that for `unsigned long`
//!   (`type_max(t) << (l) & type_max(t) >> (BITS_PER_TYPE(t) - 1 - (h))`, plus `GENMASK_INPUT_CHECK`, which contributes 0); the
//!   only use compared here is `APPLE_S5L_UTRSTAT_ALL_FLAGS`, whose bits are 3 to 9, far from any width difference.
//! - `BIT(n)`: `1 << n`. Where `BIT()` is defined at this revision was not found in the files read here
//!   (`include/linux/bits.h` defines the `BIT_U*` variants and `include/uapi/linux/const.h:28` only `_BITUL(x)`), so this is
//!   the meaning the kernel gives it, taken from its uses and not from its body (not read).
//!
//! # The constants that are not a `#define`
//!
//! The original states some of ours in another form, and each has its own table below and is still read from the original and
//! compared: in the initializer of `s5l_serial_drv_data` (a number, or the choice of another macro's name), as a string
//! literal, or as a literal inside a statement. One of our 39 has nothing in the original to compare against at all,
//! `REG_WIDTH_BYTES`; it is in `OURS_ONLY`.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use lucina_apple_uart::regs;

/// The revision `src/regs.rs` is ported from and names in its header: AsahiLinux/linux branch `asahi`, commit
/// `6ba0637d1ed9a2e3639dd3fc9eca6cd0483a6434`, dated 2026-11-05. Pinned, so the comparison cannot move under us.
const ORIGINAL_REV: &str = "6ba0637d1ed9";

/// The environment variable naming a git clone of the original. Unset means: skip.
const CLONE_VAR: &str = "LUCINA_ASAHI_LINUX";

/// Our file, as the messages name it: a mismatch points at both sides and both lines.
const OURS_PATH: &str = "drivers/lucina-apple-uart/src/regs.rs";

/// The C structure that carries the S5L's parameters (`samsung_tty.c:2564-2587`), bound to `apple,s5l-uart` by the match
/// table at `samsung_tty.c:2667-2668`.
const DRV_DATA: &str = "s5l_serial_drv_data";

/// An original file this test reads at the pinned revision.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Orig {
    /// `include/linux/serial_s3c.h`: the register offsets and the bit defines, S3C and Apple alike.
    S3c,
    /// `drivers/tty/serial/samsung_tty.c`: the driver, which is where the S5L is bound to them.
    Tty,
}

impl Orig {
    fn path(self) -> &'static str {
        match self {
            Orig::S3c => "include/linux/serial_s3c.h",
            Orig::Tty => "drivers/tty/serial/samsung_tty.c",
        }
    }

    /// Read in this order: the header that defines the constants, then the driver that chooses them, so a name defined in
    /// both would be taken from the header.
    const ALL: &'static [Orig] = &[Orig::S3c, Orig::Tty];
}

// ---------------------------------------------------------------------------
// The tables: every constant of ours, classified.
// ---------------------------------------------------------------------------

/// Our constant, the original file, the original's `#define`. The two values must be equal. Every one of ours renames: the C
/// carries the family in the name (`S3C2410_ULCON`, `APPLE_S5L_UCON_TXTHRESH_ENA_MSK`) and this crate is only ever the S5L,
/// so ours drops the prefix and the `_MSK`.
const SCALARS: &[(&str, Orig, &str)] = &[
    // The register offsets, `serial_s3c.h:19-29`.
    ("ULCON", Orig::S3c, "S3C2410_ULCON"),
    ("UCON", Orig::S3c, "S3C2410_UCON"),
    ("UFCON", Orig::S3c, "S3C2410_UFCON"),
    ("UMCON", Orig::S3c, "S3C2410_UMCON"),
    ("UTRSTAT", Orig::S3c, "S3C2410_UTRSTAT"),
    ("UERSTAT", Orig::S3c, "S3C2410_UERSTAT"),
    ("UFSTAT", Orig::S3c, "S3C2410_UFSTAT"),
    ("UMSTAT", Orig::S3c, "S3C2410_UMSTAT"),
    ("UTXH", Orig::S3c, "S3C2410_UTXH"),
    ("URXH", Orig::S3c, "S3C2410_URXH"),
    ("UBRDIV", Orig::S3c, "S3C2410_UBRDIV"),
    // UFCON: `serial_s3c.h:119` and `:136-137`.
    ("UFCON_FIFOMODE", Orig::S3c, "S3C2410_UFCON_FIFOMODE"),
    ("UFCON_RESETRX", Orig::S3c, "S3C2410_UFCON_RESETRX"),
    ("UFCON_RESETTX", Orig::S3c, "S3C2410_UFCON_RESETTX"),
    // UFSTAT: `serial_s3c.h:155-160`, the S3C2410 layout, which is the family `s5l_serial_drv_data` picks (DRV_DATA_LAYOUT).
    ("UFSTAT_TXFULL", Orig::S3c, "S3C2410_UFSTAT_TXFULL"),
    ("UFSTAT_RXFULL", Orig::S3c, "S3C2410_UFSTAT_RXFULL"),
    ("UFSTAT_TXMASK", Orig::S3c, "S3C2410_UFSTAT_TXMASK"),
    ("UFSTAT_TXSHIFT", Orig::S3c, "S3C2410_UFSTAT_TXSHIFT"),
    ("UFSTAT_RXMASK", Orig::S3c, "S3C2410_UFSTAT_RXMASK"),
    ("UFSTAT_RXSHIFT", Orig::S3c, "S3C2410_UFSTAT_RXSHIFT"),
    // UTRSTAT: the S3C bits at `serial_s3c.h:170-173` ...
    ("UTRSTAT_RXDR", Orig::S3c, "S3C2410_UTRSTAT_RXDR"),
    ("UTRSTAT_TXFE", Orig::S3c, "S3C2410_UTRSTAT_TXFE"),
    ("UTRSTAT_TXE", Orig::S3c, "S3C2410_UTRSTAT_TXE"),
    // ... and the Apple ones at `serial_s3c.h:266-270`.
    (
        "UTRSTAT_RXTO_LEGACY",
        Orig::S3c,
        "APPLE_S5L_UTRSTAT_RXTO_LEGACY",
    ),
    ("UTRSTAT_RXTHRESH", Orig::S3c, "APPLE_S5L_UTRSTAT_RXTHRESH"),
    ("UTRSTAT_TXTHRESH", Orig::S3c, "APPLE_S5L_UTRSTAT_TXTHRESH"),
    ("UTRSTAT_RXTO", Orig::S3c, "APPLE_S5L_UTRSTAT_RXTO"),
    (
        "UTRSTAT_ALL_FLAGS",
        Orig::S3c,
        "APPLE_S5L_UTRSTAT_ALL_FLAGS",
    ),
    // UCON: `serial_s3c.h:114` and `:117` for the mode fields, `:249-256` and `:261-264` for the Apple interrupt enables.
    ("UCON_RXMODE_MASK", Orig::S3c, "S3C64XX_UCON_RXMODE_MASK"),
    ("UCON_TXMODE_MASK", Orig::S3c, "S3C64XX_UCON_TXMODE_MASK"),
    ("UCON_RXTO_ENA", Orig::S3c, "APPLE_S5L_UCON_RXTO_ENA_MSK"),
    (
        "UCON_RXTO_LEGACY_ENA",
        Orig::S3c,
        "APPLE_S5L_UCON_RXTO_LEGACY_ENA_MSK",
    ),
    (
        "UCON_RXTHRESH_ENA",
        Orig::S3c,
        "APPLE_S5L_UCON_RXTHRESH_ENA_MSK",
    ),
    (
        "UCON_TXTHRESH_ENA",
        Orig::S3c,
        "APPLE_S5L_UCON_TXTHRESH_ENA_MSK",
    ),
    ("UCON_IRQ_MASK", Orig::S3c, "APPLE_S5L_UCON_MASK"),
];

/// Our constant and the field of `s5l_serial_drv_data` whose initializer is the number. The C gives the S5L these not as a
/// `#define` but in the driver data bound to `apple,s5l-uart`, so the number is read out of that structure.
const DRV_DATA_NUMBERS: &[(&str, &str)] = &[
    // `.fifosize = 16` (`samsung_tty.c:2570`), which our `FIFO_SIZE` carries.
    ("FIFO_SIZE", "fifosize"),
];

/// Our constant, the field of `s5l_serial_drv_data`, and the original macro that field must name. Different S3C families
/// spell the same field differently and the numbers are not the same (`S3C2440_UFSTAT_RXMASK` is 63 where
/// `S3C2410_UFSTAT_RXMASK` is 15), so comparing our number against the C's would not show that we followed the family the S5L
/// follows. This checks the choice, at the same line the number is read from.
const DRV_DATA_LAYOUT: &[(&str, &str, &str)] = &[
    ("UFSTAT_RXMASK", "rx_fifomask", "S3C2410_UFSTAT_RXMASK"),
    ("UFSTAT_RXSHIFT", "rx_fifoshift", "S3C2410_UFSTAT_RXSHIFT"),
    ("UFSTAT_RXFULL", "rx_fifofull", "S3C2410_UFSTAT_RXFULL"),
    ("UFSTAT_TXFULL", "tx_fifofull", "S3C2410_UFSTAT_TXFULL"),
    ("UFSTAT_TXMASK", "tx_fifomask", "S3C2410_UFSTAT_TXMASK"),
    ("UFSTAT_TXSHIFT", "tx_fifoshift", "S3C2410_UFSTAT_TXSHIFT"),
    ("UCON_IRQ_MASK", "ucon_mask", "APPLE_S5L_UCON_MASK"),
    // The register width: the C states it as this symbol, we state it as a number. See `OURS_ONLY`.
    ("REG_WIDTH_BYTES", "iotype", "UPIO_MEM32"),
];

/// Our constant holding text, and a marker for each place in `samsung_tty.c` where the original writes it. Some line
/// containing the marker must carry our exact string, and the line found is reported.
const STRING_LITERALS: &[(&str, &[&str])] = &[(
    "COMPATIBLE",
    &[
        // `{ .compatible = "apple,s5l-uart", .data = S5L_SERIAL_DRV_DATA }`: the match table (`samsung_tty.c:2667-2668`).
        ".compatible",
        // `OF_EARLYCON_DECLARE(s5l, "apple,s5l-uart", apple_s5l_early_console_setup)`: the early console (`:2878`).
        "OF_EARLYCON_DECLARE(s5l,",
    ],
)];

/// Our constant, the C function, and why the constant is that function's mask. The original states it as a literal inside a
/// statement, with no name to compare to; the test reads the `parameter & MASK` out of the function body and compares it.
const STATEMENT_MASKS: &[(&str, &str, &str)] = &[(
    "UCON_CONFIGURED_MASK",
    "s3c24xx_port_configured",
    "the C writes `return (ucon & 0xf) != 0;` at samsung_tty.c:2231, with no name on the mask, so ours carries the number \
     and this checks it against the statement it comes from",
)];

/// Every constant of ours that the original states no number for, and why. Everything else above is compared.
const OURS_ONLY: &[(&str, &str)] = &[
    // The C never writes a width: it chooses `.iotype = UPIO_MEM32` (`samsung_tty.c:2569`) and says why in a comment ("Apple
    // A7-A11 requires MMIO32 register accesses", `samsung_tty.c:2864`). 4 is our spelling of that choice; the kernel turns
    // `UPIO_MEM32` into 32-bit accessors in `serial_core`, not the S5L code. `DRV_DATA_LAYOUT` checks the field still names
    // `UPIO_MEM32`, so the choice is compared even though the number is ours.
    (
        "REG_WIDTH_BYTES",
        "our number for the C's `.iotype = UPIO_MEM32`; the C states no width as a number",
    ),
];

// ---------------------------------------------------------------------------
// Our side: the declarations and the values, both read from `src/regs.rs`.
// ---------------------------------------------------------------------------

/// What `src/regs.rs` declares: name -> line.
fn ours_declared() -> BTreeMap<String, usize> {
    // cargo runs a test with the crate directory as its working directory, so the path is built from the manifest dir.
    let path = format!("{}/src/regs.rs", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {path}: {e}"));
    let mut out = BTreeMap::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim_start();
        let rest = match line
            .strip_prefix("pub const fn ")
            .map(|r| (r, true))
            .or_else(|| line.strip_prefix("pub const ").map(|r| (r, false)))
        {
            Some((r, _)) => r,
            None => continue,
        };
        let name = rest
            .split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .next()
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| panic!("{OURS_PATH}:{} declares a constant with no name", i + 1));
        out.insert(name.to_string(), i + 1);
    }
    assert!(
        !out.is_empty(),
        "{OURS_PATH} declares no `pub const`: the parse in this test is wrong"
    );
    out
}

/// The values, taken from the crate rather than retyped here, so the test compares what we ship.
macro_rules! our_int_values {
    ($($n:ident),* $(,)?) => {{
        let mut m: BTreeMap<&'static str, u64> = BTreeMap::new();
        $(m.insert(stringify!($n), regs::$n as u64);)*
        m
    }};
}

fn our_value(name: &str) -> Option<u64> {
    our_int_values![
        REG_WIDTH_BYTES,
        ULCON,
        UCON,
        UFCON,
        UMCON,
        UTRSTAT,
        UERSTAT,
        UFSTAT,
        UMSTAT,
        UTXH,
        URXH,
        UBRDIV,
        FIFO_SIZE,
        UFCON_FIFOMODE,
        UFCON_RESETRX,
        UFCON_RESETTX,
        UFSTAT_TXFULL,
        UFSTAT_RXFULL,
        UFSTAT_TXMASK,
        UFSTAT_TXSHIFT,
        UFSTAT_RXMASK,
        UFSTAT_RXSHIFT,
        UTRSTAT_RXDR,
        UTRSTAT_TXFE,
        UTRSTAT_TXE,
        UTRSTAT_RXTO_LEGACY,
        UTRSTAT_RXTHRESH,
        UTRSTAT_TXTHRESH,
        UTRSTAT_RXTO,
        UTRSTAT_ALL_FLAGS,
        UCON_RXMODE_MASK,
        UCON_TXMODE_MASK,
        UCON_RXTO_ENA,
        UCON_RXTO_LEGACY_ENA,
        UCON_RXTHRESH_ENA,
        UCON_TXTHRESH_ENA,
        UCON_IRQ_MASK,
        UCON_CONFIGURED_MASK,
    ]
    .get(name)
    .copied()
}

fn our_text(name: &str) -> Option<String> {
    match name {
        "COMPATIBLE" => Some(regs::COMPATIBLE.to_string()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// The original side: reading the clone, and the little C evaluator.
// ---------------------------------------------------------------------------

/// The clone, or `None` when the variable is unset or empty: the test then skips.
fn clone_dir() -> Option<std::path::PathBuf> {
    let raw = std::env::var(CLONE_VAR).ok()?;
    if raw.trim().is_empty() {
        return None;
    }
    let p = std::path::PathBuf::from(raw.trim());
    assert!(p.is_dir(), "{CLONE_VAR}={}: not a directory", p.display());
    assert!(
        p.join(".git").exists(),
        "{CLONE_VAR}={}: not a git clone (no .git), so `{ORIGINAL_REV}:{}` cannot be read from it",
        p.display(),
        Orig::S3c.path()
    );
    Some(p)
}

/// The original text of one file at the pinned revision, from the object store: nothing of it is written anywhere.
fn original(clone: &Path, file: Orig) -> String {
    let spec = format!("{ORIGINAL_REV}:{}", file.path());
    let out = Command::new("git")
        .arg("-C")
        .arg(clone)
        .arg("show")
        .arg(&spec)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "cannot run git to read {spec} from {}: {e}",
                clone.display()
            )
        });
    assert!(
        out.status.success(),
        "`git -C {} show {spec}` failed ({}): {}\nIs the clone at that revision? Try `git -C {} cat-file -e {ORIGINAL_REV}`.",
        clone.display(),
        out.status,
        String::from_utf8_lossy(&out.stderr).trim(),
        clone.display()
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Blank `/* */` and `//` comments, keeping every newline, so a line number stays the line number of the original file.
fn strip_comments(text: &str) -> String {
    #[derive(PartialEq)]
    enum S {
        Code,
        Block,
        Line,
        Str,
        Chr,
    }
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut s = S::Code;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let n = chars.get(i + 1).copied();
        match s {
            S::Code if c == '/' && n == Some('*') => {
                s = S::Block;
                out.push_str("  ");
                i += 2;
                continue;
            }
            S::Code if c == '/' && n == Some('/') => {
                s = S::Line;
                out.push_str("  ");
                i += 2;
                continue;
            }
            S::Code => {
                if c == '"' {
                    s = S::Str;
                } else if c == '\'' {
                    s = S::Chr;
                }
                out.push(c);
            }
            S::Block => {
                if c == '*' && n == Some('/') {
                    s = S::Code;
                    out.push_str("  ");
                    i += 2;
                    continue;
                }
                out.push(if c == '\n' { '\n' } else { ' ' });
            }
            S::Line => {
                if c == '\n' {
                    s = S::Code;
                    out.push('\n');
                } else {
                    out.push(' ');
                }
            }
            S::Str | S::Chr => {
                let stop = if s == S::Str { '"' } else { '\'' };
                out.push(c);
                if c == '\\' {
                    if let Some(x) = n {
                        out.push(x);
                        i += 2;
                        continue;
                    }
                } else if c == stop {
                    s = S::Code;
                }
            }
        }
        i += 1;
    }
    out
}

/// The file as logical lines: comments gone, a backslash continuation joined into the line it continues. Each is numbered
/// with the first physical line it came from.
fn logical_lines(text: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut start: Option<usize> = None;
    for (i, raw) in strip_comments(text).lines().enumerate() {
        let line = raw.trim_end();
        if start.is_none() {
            start = Some(i + 1);
        }
        match line.strip_suffix('\\') {
            Some(head) => {
                cur.push_str(head);
                cur.push(' ');
            }
            None => {
                cur.push_str(line);
                out.push((start.take().unwrap(), std::mem::take(&mut cur)));
            }
        }
    }
    if let Some(s) = start {
        out.push((s, cur));
    }
    out
}

#[derive(Clone, PartialEq, Debug)]
enum Tok {
    Num(u64),
    Id(String),
    /// A string literal, kept as a marker with no text: the evaluator refuses it, and nothing of the original's wording
    /// reaches this repository or its output.
    Str,
    P(&'static str),
}

/// Kernel macros this evaluator knows by meaning; see the module documentation.
const BUILTINS: &[&str] = &[
    "BIT",
    "BIT_ULL",
    "GENMASK",
    "GENMASK_ULL",
    "GENMASK_U32",
    "UL",
    "ULL",
    "_UL",
    "_ULL",
    "_BITUL",
];

/// Two-character operators first, so `<<` is not read as two `<` and `==` not as `=` twice. `=` is here to be read in an
/// initializer, not to be evaluated.
const PUNCT: &[&str] = &[
    "<<", ">>", "<=", ">=", "==", "!=", "&&", "||", "(", ")", ",", "|", "&", "^", "~", "!", "+",
    "-", "*", "/", "%", "?", ":", "<", ">", "=", "{", "}", "[", "]", ";", ".",
];

/// Split C text into literals, names and punctuation. Octal literals are refused: none appears in what is compared here, and
/// reading `013` as thirteen would be a silent wrong answer.
fn tokenize(text: &str) -> Result<Vec<Tok>, String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '"' {
            // A driver block sets `.name = "…"`, so the lexer has to step over a string to reach the fields after it.
            i += 1;
            while i < chars.len() && chars[i] != '"' {
                if chars[i] == '\\' {
                    i += 1;
                }
                i += 1;
            }
            if i >= chars.len() {
                return Err("a string literal is not closed".to_string());
            }
            i += 1;
            out.push(Tok::Str);
            continue;
        }
        if c.is_ascii_digit() {
            let s = i;
            let hex = c == '0' && matches!(chars.get(i + 1), Some('x') | Some('X'));
            i += if hex { 2 } else { 1 };
            while i < chars.len() && (chars[i].is_ascii_hexdigit() || chars[i] == '_') {
                i += 1;
            }
            let digits_end = i;
            while i < chars.len() && matches!(chars[i].to_ascii_uppercase(), 'U' | 'L') {
                i += 1;
            }
            let all: String = chars[s..i].iter().collect();
            let raw: String = chars[s..digits_end].iter().filter(|x| **x != '_').collect();
            let digits = raw
                .strip_prefix("0x")
                .or_else(|| raw.strip_prefix("0X"))
                .unwrap_or(&raw)
                .to_string();
            if digits.is_empty() {
                return Err(format!("`{all}` is not a number"));
            }
            if !hex && raw.starts_with('0') && raw.len() > 1 {
                return Err(format!(
                    "octal literal `{raw}` is not something this evaluator reads"
                ));
            }
            let v = u64::from_str_radix(&digits, if hex { 16 } else { 10 })
                .map_err(|e| format!("`{all}` is not a literal: {e}"))?;
            out.push(Tok::Num(v));
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' {
            let s = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            out.push(Tok::Id(chars[s..i].iter().collect()));
            continue;
        }
        if let Some(p) = PUNCT.iter().find(|p| {
            let pc: Vec<char> = p.chars().collect();
            chars.len() >= i + pc.len() && chars[i..i + pc.len()] == pc[..]
        }) {
            out.push(Tok::P(p));
            i += p.chars().count();
            continue;
        }
        return Err(format!("`{c}` is not something this evaluator reads"));
    }
    Ok(out)
}

/// A `#define NAME body`, as tokens, with where it was read from.
#[derive(Clone, Debug)]
struct Sym {
    file: Orig,
    line: usize,
    text: String,
    toks: Result<Vec<Tok>, String>,
}

struct Model {
    defines: BTreeMap<String, Sym>,
    text: BTreeMap<Orig, Vec<(usize, String)>>,
}

/// The word of a `#define` / `#undef` directive, and its operand.
fn directive(line: &str) -> Option<(&'static str, &str)> {
    let rest = line.trim_start().strip_prefix('#')?.trim_start();
    for name in ["define", "undef"] {
        if let Some(r) = rest.strip_prefix(name)
            && r.chars()
                .next()
                .is_none_or(|c| !(c.is_alphanumeric() || c == '_'))
        {
            return Some((name, r.trim_start()));
        }
    }
    None
}

/// The identifier at the head of a string.
fn head_word(s: &str) -> String {
    s.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .next()
        .unwrap_or("")
        .to_string()
}

impl Model {
    fn load(clone: &Path) -> Model {
        let mut m = Model {
            defines: BTreeMap::new(),
            text: BTreeMap::new(),
        };
        for f in Orig::ALL {
            let lines = logical_lines(&original(clone, *f));
            m.parse_defines(*f, &lines);
            m.text.insert(*f, lines);
        }
        m
    }

    /// `#define` and `#undef`, in file order. The first definition of a name wins. A function-like macro is not recorded:
    /// nothing compared here is one, and guessing at its parameters would be worse than an error saying so.
    fn parse_defines(&mut self, file: Orig, lines: &[(usize, String)]) {
        for (no, raw) in lines {
            let Some((kind, operand)) = directive(raw) else {
                continue;
            };
            if kind == "undef" {
                let name = head_word(operand);
                if !name.is_empty() {
                    self.defines.remove(&name);
                }
                continue;
            }
            let end = operand
                .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                .unwrap_or(operand.len());
            let name = &operand[..end];
            // A function-like macro has its parenthesis hard against the name; an object-like one may well have a body that
            // starts with a parenthesis, and every register offset in `serial_s3c.h` does.
            let after = &operand[end..];
            if name.is_empty() || after.starts_with('(') || self.defines.contains_key(name) {
                continue;
            }
            let body = after.trim();
            if body.is_empty() {
                continue;
            }
            let toks = tokenize(body);
            self.defines.insert(
                name.to_string(),
                Sym {
                    file,
                    line: *no,
                    text: body.to_string(),
                    toks,
                },
            );
        }
    }

    fn lookup(&self, name: &str) -> Option<&Sym> {
        self.defines.get(name)
    }

    /// Where a name came from, for a message: file and line of the definition.
    fn describe(&self, name: &str) -> String {
        match self.lookup(name) {
            Some(s) => format!("{}:{}", s.file.path(), s.line),
            None => "not a #define in the files this test reads".to_string(),
        }
    }

    fn lines(&self, file: Orig) -> &[(usize, String)] {
        &self.text[&file]
    }

    /// The value of a `#define`.
    fn value(&self, name: &str) -> Result<u64, String> {
        let sym = self
            .lookup(name)
            .ok_or_else(|| format!("`{name}` is not defined in the files this test reads"))?;
        let toks = sym.toks.as_ref().map_err(|e| {
            format!(
                "its text `{}` cannot be read by this evaluator: {e}",
                sym.text
            )
        })?;
        let mut stack = vec![format!("{name} at {}:{}", sym.file.path(), sym.line)];
        let flat = self.expand(toks, &mut stack)?;
        self.eval(&flat)
            .map_err(|e| format!("{e} (in `{name}`, whose text is `{}`)", sym.text))
    }

    /// The value of tokens read out of a statement or an initializer, where there is no name to report.
    fn value_of(&self, toks: &[Tok], what: &str) -> Result<u64, String> {
        let mut stack = vec![what.to_string()];
        let flat = self.expand(toks, &mut stack)?;
        self.eval(&flat).map_err(|e| format!("{e} (in `{what}`)"))
    }

    /// Replace every name that is defined by its tokens, recursively. `BIT`/`GENMASK` stay: the evaluator reads the call, and
    /// the names inside its arguments are expanded like any other.
    fn expand(&self, toks: &[Tok], stack: &mut Vec<String>) -> Result<Vec<Tok>, String> {
        let mut out = Vec::with_capacity(toks.len());
        for t in toks {
            let Tok::Id(name) = t else {
                out.push(t.clone());
                continue;
            };
            if BUILTINS.contains(&name.as_str()) {
                out.push(t.clone());
                continue;
            }
            match self.lookup(name) {
                Some(s) => {
                    let trail = format!("{name} at {}:{}", s.file.path(), s.line);
                    if stack.contains(&trail) {
                        stack.push(trail.clone());
                        return Err(format!("circular reference: {}", stack.join(" -> ")));
                    }
                    let inner = match &s.toks {
                        Ok(toks) => toks.clone(),
                        Err(e) => {
                            return Err(format!("`{name}` at {} cannot be read: {e}", s.line));
                        }
                    };
                    stack.push(trail);
                    let expanded = self.expand(&inner, stack);
                    stack.pop();
                    out.extend(expanded?);
                }
                None => out.push(Tok::Id(name.clone())),
            }
        }
        Ok(out)
    }

    /// Evaluate already-expanded tokens, with C precedence.
    fn eval(&self, toks: &[Tok]) -> Result<u64, String> {
        if toks.is_empty() {
            return Err("there is nothing to evaluate".to_string());
        }
        let as_text = toks.iter().map(tok_text).collect::<Vec<_>>().join("");
        let mut p = Parser {
            t: toks.to_vec(),
            i: 0,
        };
        let v = p.expr().map_err(|e| format!("{e} (in `{as_text}`)"))?;
        if p.i != p.t.len() {
            return Err(format!(
                "`{as_text}` has tokens left over, starting at `{}`",
                tok_text(&p.t[p.i])
            ));
        }
        Ok(v)
    }
}

fn tok_text(t: &Tok) -> String {
    match t {
        Tok::Num(v) => format!("{v}"),
        Tok::Id(s) => s.clone(),
        Tok::Str => "\"…\"".to_string(),
        Tok::P(p) => (*p).to_string(),
    }
}

/// C precedence, lowest first: `| ^ & == != < > << >> + - * /`, then unary and primary.
/// The tokens are already expanded, so the parser needs no model: what it cannot read is an error, not a lookup.
struct Parser {
    t: Vec<Tok>,
    i: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.t.get(self.i)
    }

    fn at(&self, op: &str) -> bool {
        matches!(self.peek(), Some(Tok::P(p)) if *p == op)
    }

    fn eat(&mut self, op: &str) -> bool {
        if self.at(op) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn expr(&mut self) -> Result<u64, String> {
        self.or()
    }

    fn level<F>(&mut self, ops: &[&'static str], mut down: F) -> Result<u64, String>
    where
        F: FnMut(&mut Self) -> Result<u64, String>,
    {
        let mut v = down(self)?;
        while let Some(Tok::P(op)) = self.peek().cloned() {
            if !ops.contains(&op) {
                break;
            }
            self.i += 1;
            let rhs = down(self)?;
            v = apply(op, v, rhs)?;
        }
        Ok(v)
    }

    fn or(&mut self) -> Result<u64, String> {
        self.level(&["|"], Self::xor)
    }
    fn xor(&mut self) -> Result<u64, String> {
        self.level(&["^"], Self::and)
    }
    fn and(&mut self) -> Result<u64, String> {
        self.level(&["&"], Self::eq)
    }
    fn eq(&mut self) -> Result<u64, String> {
        self.level(&["==", "!="], Self::rel)
    }
    fn rel(&mut self) -> Result<u64, String> {
        self.level(&["<", ">", "<=", ">="], Self::shift)
    }
    fn shift(&mut self) -> Result<u64, String> {
        self.level(&["<<", ">>"], Self::add)
    }
    fn add(&mut self) -> Result<u64, String> {
        self.level(&["+", "-"], Self::mul)
    }
    fn mul(&mut self) -> Result<u64, String> {
        self.level(&["*", "/", "%"], Self::unary)
    }

    fn unary(&mut self) -> Result<u64, String> {
        match self.peek() {
            Some(Tok::P("~")) => {
                self.i += 1;
                Ok(!self.unary()?)
            }
            Some(Tok::P("!")) => {
                self.i += 1;
                Ok(u64::from(self.unary()? == 0))
            }
            Some(Tok::P("-")) => {
                self.i += 1;
                Ok(self.unary()?.wrapping_neg())
            }
            Some(Tok::P("+")) => {
                self.i += 1;
                self.unary()
            }
            _ => self.primary(),
        }
    }

    fn primary(&mut self) -> Result<u64, String> {
        match self.t.get(self.i).cloned() {
            Some(Tok::Num(v)) => {
                self.i += 1;
                Ok(v)
            }
            Some(Tok::P("(")) => {
                self.i += 1;
                let v = self.expr()?;
                if !self.eat(")") {
                    return Err("a parenthesis is not closed".to_string());
                }
                Ok(v)
            }
            Some(Tok::Id(name)) => {
                self.i += 1;
                if BUILTINS.contains(&name.as_str()) {
                    let args = self.call_args()?;
                    return builtin(&name, &args);
                }
                Err(format!(
                    "`{name}` has no value: it is not a #define this test read"
                ))
            }
            Some(Tok::Str) => Err("a string literal is not a number".to_string()),
            Some(Tok::P(p)) => Err(format!("`{p}` is not the start of an expression")),
            None => Err("the expression ends too early".to_string()),
        }
    }

    fn call_args(&mut self) -> Result<Vec<u64>, String> {
        if !self.eat("(") {
            return Err("a built-in is used without its parenthesis".to_string());
        }
        let mut args = Vec::new();
        if !self.at(")") {
            loop {
                args.push(self.expr()?);
                if self.eat(",") {
                    continue;
                }
                break;
            }
        }
        if !self.eat(")") {
            return Err("a built-in's argument list is not closed".to_string());
        }
        Ok(args)
    }
}

/// One binary operation. Everything is `u64`, which is how these constants are read; a shift of 64 or more is not a register
/// bit in either file, so it is refused rather than wrapped.
fn apply(op: &'static str, a: u64, b: u64) -> Result<u64, String> {
    if matches!(op, "<<" | ">>") && b > 63 {
        return Err(format!(
            "`{a} {op} {b}` shifts past the width of the constant"
        ));
    }
    Ok(match op {
        "|" => a | b,
        "^" => a ^ b,
        "&" => a & b,
        "==" => u64::from(a == b),
        "!=" => u64::from(a != b),
        "<" => u64::from(a < b),
        ">" => u64::from(a > b),
        "<=" => u64::from(a <= b),
        ">=" => u64::from(a >= b),
        "<<" => a.wrapping_shl(b as u32),
        ">>" => a.wrapping_shr(b as u32),
        "+" => a.wrapping_add(b),
        "-" => a.wrapping_sub(b),
        "*" => a.wrapping_mul(b),
        "/" => match b {
            0 => return Err("a constant divides by zero".to_string()),
            _ => a / b,
        },
        "%" => match b {
            0 => return Err("a constant takes the remainder of a division by zero".to_string()),
            _ => a % b,
        },
        _ => return Err(format!("`{op}` is not an operator this evaluator has")),
    })
}

fn builtin(name: &str, args: &[u64]) -> Result<u64, String> {
    match (name, args) {
        ("BIT" | "BIT_ULL" | "_BITUL", [n]) => {
            if *n > 63 {
                return Err(format!("BIT({n}) is past the width of the constant"));
            }
            Ok(1u64 << n)
        }
        ("UL" | "ULL" | "_UL" | "_ULL", [v]) => Ok(*v),
        ("GENMASK" | "GENMASK_ULL" | "GENMASK_U32", [h, l]) => {
            if *h > 63 || *l > 63 || h < l {
                return Err(format!(
                    "GENMASK({h}, {l}) is outside what this evaluator computes"
                ));
            }
            Ok(mask_between(*h as u32, *l as u32))
        }
        _ => Err(format!(
            "`{name}` is used with {} argument(s), which is not its form",
            args.len()
        )),
    }
}

fn mask_between(hi: u32, lo: u32) -> u64 {
    let ones = if hi == 63 {
        u64::MAX
    } else {
        (1u64 << (hi + 1)) - 1
    };
    ones & !((1u64 << lo) - 1)
}

// ---------------------------------------------------------------------------
// Reading the forms that are not a `#define`: a driver-data initializer and a statement.
// ---------------------------------------------------------------------------

/// The tokens of `HOLDER = { ... }`, from the `{` to its match, each with its line. The name must stand alone on its left.
fn initializer(model: &Model, file: Orig, holder: &str) -> Result<Vec<(usize, Tok)>, String> {
    let lines = model.lines(file);
    let start = lines.iter().find_map(|(no, raw)| {
        let mut from = 0;
        loop {
            let at = raw[from..].find(holder)? + from;
            let before_ok =
                at == 0 || !raw[..at].ends_with(|c: char| c.is_alphanumeric() || c == '_');
            let tail = raw[at + holder.len()..].trim_start();
            if before_ok
                && tail.starts_with('=')
                && tail.trim_start_matches('=').trim_start().starts_with('{')
            {
                return Some(*no);
            }
            from = at + holder.len();
        }
    });
    let start = start.ok_or_else(|| {
        format!(
            "`{holder} = {{` is not in {} as read at {ORIGINAL_REV}",
            file.path()
        )
    })?;
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut open = false;
    for (no, raw) in lines.iter().filter(|(n, _)| *n >= start) {
        for t in tokenize(raw)? {
            if t == Tok::P("{") {
                depth += 1;
                open = true;
            } else if open && t == Tok::P("}") {
                depth -= 1;
            }
            if open {
                out.push((*no, t));
            }
            if open && depth == 0 {
                return Ok(out);
            }
        }
    }
    Err(format!(
        "`{holder}`'s initializer is not closed in {}",
        file.path()
    ))
}

/// The tokens a designated field is set to, `.field = <these>`, with the line of the dot.
fn field_value(toks: &[(usize, Tok)], field: &str) -> Result<(usize, Vec<Tok>), String> {
    let mut i = 0;
    while i + 2 < toks.len() {
        if toks[i].1 == Tok::P(".")
            && toks[i + 1].1 == Tok::Id(field.to_string())
            && toks[i + 2].1 == Tok::P("=")
        {
            let mut out = Vec::new();
            let mut depth = 0i32;
            for (_, t) in &toks[i + 3..] {
                match t {
                    Tok::P("{") | Tok::P("(") => depth += 1,
                    Tok::P("}") | Tok::P(")") => depth -= 1,
                    Tok::P(",") if depth == 0 => break,
                    _ => {}
                }
                out.push(t.clone());
            }
            return Ok((toks[i].0, out));
        }
        i += 1;
    }
    Err(format!(
        "there is no `.{field} =` in the initializer read here"
    ))
}

/// The single name a field is set to, as the C writes it (`UPIO_MEM32`, `S3C2410_UFSTAT_RXMASK`).
fn field_symbol(value: &[Tok]) -> Option<String> {
    let strip: Vec<Tok> = match (value.first(), value.last()) {
        (Some(Tok::P("(")), Some(Tok::P(")"))) if value.len() >= 3 => {
            value[1..value.len() - 1].to_vec()
        }
        _ => value.to_vec(),
    };
    match strip.as_slice() {
        [Tok::Id(name)] => Some(name.clone()),
        _ => None,
    }
}

/// The `parameter & MASK` of a small C function: the parameter's name, the mask's value, and the line of the `&`.
fn statement_mask(model: &Model, func: &str) -> Result<(String, u64, usize), String> {
    let lines = model.lines(Orig::Tty);
    let at = lines
        .iter()
        .find(|(_, raw)| {
            let t = raw.trim_start();
            t.starts_with(func) && t[func.len()..].trim_start().starts_with('(')
        })
        .ok_or_else(|| {
            format!(
                "`{func}(...)` does not begin a line in {}",
                Orig::Tty.path()
            )
        })?;
    let param = {
        let inner = at.1[func.len()..].trim();
        let inner = inner
            .strip_prefix('(')
            .unwrap_or(inner)
            .split(')')
            .next()
            .unwrap_or("");
        let decl = inner.split(',').next().unwrap_or("").trim();
        // A C declaration puts the name last (`u32 ucon`); `void` alone is no parameter.
        decl.split_whitespace().last().unwrap_or("").to_string()
    };
    if param.is_empty() || param == "void" {
        return Err(format!("`{func}` takes no parameter to mask"));
    }
    let mut body: Vec<(usize, Tok)> = Vec::new();
    let mut depth = 0i32;
    let mut open = false;
    'walk: for (no, raw) in lines.iter().filter(|(n, _)| *n >= at.0) {
        for t in tokenize(raw)? {
            if !open {
                if t == Tok::P("{") {
                    open = true;
                    depth = 1;
                }
                continue;
            }
            match t {
                Tok::P("{") => depth += 1,
                Tok::P("}") => {
                    depth -= 1;
                    if depth == 0 {
                        break 'walk;
                    }
                }
                _ => {}
            }
            body.push((*no, t));
        }
    }
    if !open {
        return Err(format!("`{func}` has no body where it was found"));
    }
    for i in 1..body.len() {
        if body[i].1 == Tok::P("&") && body[i - 1].1 == Tok::Id(param.clone()) {
            let tail: Vec<Tok> = cut_at_separator(&body[i + 1..])
                .iter()
                .map(|(_, t)| t.clone())
                .collect();
            let mask = model.value_of(
                &tail,
                &format!(
                    "{param} & {}",
                    tail.iter().map(tok_text).collect::<Vec<_>>().join("")
                ),
            )?;
            return Ok((param, mask, body[i].0));
        }
    }
    Err(format!(
        "`{func}` does not mask its parameter `{param}` with `&`"
    ))
}

/// The tokens of an expression that stands on its own inside a statement: up to the first separator that is not inside
/// parentheses, so the rest of the line (`) != 0 ;`) is not mistaken for part of the mask.
fn cut_at_separator(toks: &[(usize, Tok)]) -> Vec<(usize, Tok)> {
    let mut depth = 0i32;
    let mut out = Vec::new();
    for (no, t) in toks {
        let sep = matches!(t, Tok::P(")") | Tok::P(";") | Tok::P(",") | Tok::P("}"));
        match t {
            Tok::P("(") | Tok::P("{") => depth += 1,
            Tok::P(")") | Tok::P("}") => depth -= 1,
            _ => {}
        }
        if sep && depth <= 0 {
            break;
        }
        out.push((*no, t.clone()));
    }
    out
}

/// The first `"..."` of a line.
fn quoted(line: &str) -> Option<String> {
    Some(line.split('"').nth(1)?.split('"').next()?.to_string())
}

// ---------------------------------------------------------------------------
// The tests.
// ---------------------------------------------------------------------------

/// The tables have to cover our file, in both directions: a constant added to `src/regs.rs` and left out of every table here
/// would be a constant nobody checks. This needs no clone, so it runs everywhere.
#[test]
fn every_constant_is_classified() {
    let declared = ours_declared();
    fn put(
        tabled: &mut BTreeMap<&'static str, &'static str>,
        name: &'static str,
        where_: &'static str,
    ) {
        if let Some(first) = tabled.insert(name, where_) {
            panic!("`{name}` is in both {first} and {where_}: a constant belongs to one table");
        }
    }
    let mut tabled: BTreeMap<&'static str, &'static str> = BTreeMap::new();
    for (ours, _, _) in SCALARS {
        put(&mut tabled, ours, "SCALARS");
    }
    for (ours, _) in DRV_DATA_NUMBERS {
        put(&mut tabled, ours, "DRV_DATA_NUMBERS");
    }
    for (ours, _) in STRING_LITERALS {
        put(&mut tabled, ours, "STRING_LITERALS");
    }
    for (ours, _, _) in STATEMENT_MASKS {
        put(&mut tabled, ours, "STATEMENT_MASKS");
    }
    for (ours, _) in OURS_ONLY {
        put(&mut tabled, ours, "OURS_ONLY");
    }

    let unclassified: Vec<String> = declared
        .keys()
        .filter(|n| !tabled.contains_key(n.as_str()))
        .cloned()
        .collect();
    assert!(
        unclassified.is_empty(),
        "{} of our constants are in no table, so nothing compares them; put each in SCALARS, DRV_DATA_NUMBERS, \
         STRING_LITERALS, STATEMENT_MASKS or OURS_ONLY:\n  {}",
        unclassified.len(),
        unclassified.join("\n  ")
    );
    for (ours, where_) in &tabled {
        assert!(
            declared.contains_key(*ours),
            "`{ours}` is in {where_} but {OURS_PATH} does not declare it"
        );
        assert!(
            our_value(ours).is_some() || our_text(ours).is_some(),
            "`{ours}` is in {where_} but our_value/our_text does not know it, so ours is never read"
        );
    }
    assert_eq!(
        tabled.len(),
        declared.len(),
        "{OURS_PATH} declares {} constants, the tables of this test name {}",
        declared.len(),
        tabled.len()
    );
    // Every layout choice belongs to a constant whose value is compared from one of the tables above.
    for (ours, field, symbol) in DRV_DATA_LAYOUT {
        assert!(
            tabled.contains_key(ours),
            "`{ours}` is in DRV_DATA_LAYOUT but nowhere that reads its value"
        );
        assert!(
            !field.is_empty() && !symbol.is_empty(),
            "an empty column in a DRV_DATA_LAYOUT row"
        );
    }
    eprintln!(
        "classified {} of our constants: {} against a #define, {} against a driver-data number, {} against a string literal \
         or a statement, {} the original states no number for; {} layout choices are checked too",
        declared.len(),
        SCALARS.len(),
        DRV_DATA_NUMBERS.len(),
        STATEMENT_MASKS.len() + STRING_LITERALS.len(),
        OURS_ONLY.len(),
        DRV_DATA_LAYOUT.len()
    );
}

/// The comparison. Every mismatch is collected, so one run names all of them.
#[test]
fn our_constants_equal_the_original() {
    let Some(clone) = clone_dir() else {
        eprintln!("skipped: LUCINA_ASAHI_LINUX not set");
        return;
    };
    let m = Model::load(&clone);
    let declared = ours_declared();
    let mut bad: Vec<String> = Vec::new();
    let mut compared = 0usize;

    let ours_at = |name: &str, table: &str| -> String {
        match declared.get(name) {
            Some(no) => format!("{OURS_PATH}:{no} ({table})"),
            None => panic!("`{name}` is in {table} but {OURS_PATH} does not declare it"),
        }
    };
    let mine = |name: &str, table: &str| -> u64 {
        our_value(name).unwrap_or_else(|| {
            panic!("`{name}` is in {table} but our_value does not know it: add it there")
        })
    };

    // 1. The ones that are a `#define`.
    for (ours, _file, orig) in SCALARS {
        let mine = mine(ours, "SCALARS");
        match m.value(orig) {
            Ok(theirs) => {
                compared += 1;
                if theirs != mine {
                    bad.push(format!(
                        "{ours}: ours {} = {mine:#x} ({mine}); the original's `{orig}` at {} = {theirs:#x} ({theirs})",
                        ours_at(ours, "SCALARS"),
                        m.describe(orig)
                    ));
                }
            }
            Err(e) => bad.push(format!(
                "{ours}: ours {}; the original's `{orig}` at {} could not be read: {e}",
                ours_at(ours, "SCALARS"),
                m.describe(orig)
            )),
        }
    }

    // 2. The driver data, which is where the C states the S5L's own parameters.
    let init = initializer(&m, Orig::Tty, DRV_DATA)
        .unwrap_or_else(|e| panic!("several comparisons read `{DRV_DATA}`: {e}"));
    let init_at = format!(
        "{}:{}",
        Orig::Tty.path(),
        init.first().map(|(n, _)| *n).unwrap_or(0)
    );

    for (ours, field) in DRV_DATA_NUMBERS {
        let mine = mine(ours, "DRV_DATA_NUMBERS");
        let got = field_value(&init, field)
            .and_then(|(line, toks)| m.value_of(&toks, &format!(".{field}")).map(|v| (v, line)));
        match got {
            Ok((theirs, line)) => {
                compared += 1;
                if theirs != mine {
                    bad.push(format!(
                        "{ours}: ours {} = {mine:#x} ({mine}); the original's `.{field}` in `{DRV_DATA}` at {}:{line} is \
                         {theirs:#x} ({theirs})",
                        ours_at(ours, "DRV_DATA_NUMBERS"),
                        Orig::Tty.path()
                    ));
                }
            }
            Err(e) => bad.push(format!(
                "{ours}: ours {}; `{DRV_DATA}` read from {init_at}: {e}",
                ours_at(ours, "DRV_DATA_NUMBERS")
            )),
        }
    }

    for (ours, field, symbol) in DRV_DATA_LAYOUT {
        // A constant of ours that the original states no number for is compared by name only: what it follows must still
        // be that symbol, but our number is ours alone.
        let ours_alone = OURS_ONLY.iter().any(|(n, _)| n == ours);
        match field_value(&init, field) {
            Ok((line, toks)) => match field_symbol(&toks) {
                Some(got) if got == *symbol => {
                    compared += 1;
                    if ours_alone {
                        eprintln!(
                            "  checked: `.{field}` names `{symbol}` ({}:{line}); {ours} is our number for it, ours alone",
                            Orig::Tty.path()
                        );
                    } else {
                        // Where the name we follow also carries a number, the number is compared here too: the same value,
                        // read through the field that chose it.
                        let mine = mine(ours, "SCALARS/DRV_DATA_NUMBERS");
                        match m.value(symbol) {
                            Ok(theirs) if theirs != mine => bad.push(format!(
                                "{ours}: ours {} = {mine:#x} ({mine}); `.{field}` names `{symbol}` at {}:{line}, whose value \
                                 is {theirs:#x} ({theirs})",
                                ours_at(ours, "SCALARS/DRV_DATA_NUMBERS"),
                                Orig::Tty.path()
                            )),
                            Err(e) => bad.push(format!(
                                "{ours}: `.{field}` names `{symbol}` at {}:{line}, but its value could not be read: {e}",
                                Orig::Tty.path()
                            )),
                            _ => eprintln!(
                                "  checked: `.{field}` names `{symbol}` ({}:{line})",
                                Orig::Tty.path()
                            ),
                        }
                    }
                }
                Some(got) => bad.push(format!(
                    "`.{field}` names `{got}` at {}:{line}, not `{symbol}`: what {ours} follows has changed",
                    Orig::Tty.path()
                )),
                None => bad.push(format!(
                    "`.{field}` at {}:{line} is `{}`, not a single macro name, so {ours} cannot be compared to it",
                    Orig::Tty.path(),
                    toks.iter().map(tok_text).collect::<Vec<_>>().join("")
                )),
            },
            Err(e) => bad.push(format!("`{DRV_DATA}` read from {init_at}: {e}")),
        }
    }

    // 3. A string literal, and a mask that the C leaves unnamed inside a statement.
    for (ours, markers) in STRING_LITERALS {
        let want = our_text(ours).unwrap_or_else(|| {
            panic!("`{ours}` is in STRING_LITERALS but our_text does not know it")
        });
        for marker in *markers {
            let hit = m.lines(Orig::Tty).iter().find(|(_, raw)| {
                raw.contains(*marker) && quoted(raw).as_deref() == Some(want.as_str())
            });
            match hit {
                Some((no, _)) => {
                    compared += 1;
                    eprintln!(
                        "  checked: \"{want}\" at {}:{no} (a line naming `{marker}`)",
                        Orig::Tty.path()
                    );
                }
                None => {
                    let others: Vec<String> = m
                        .lines(Orig::Tty)
                        .iter()
                        .filter(|(_, raw)| raw.contains(*marker))
                        .filter_map(|(no, raw)| {
                            quoted(raw).map(|q| format!("{q} ({}:{no})", Orig::Tty.path()))
                        })
                        .take(6)
                        .collect();
                    bad.push(format!(
                        "{ours}: ours {} is \"{want}\"; no line of {} containing `{marker}` carries that literal at \
                         {ORIGINAL_REV}. Its candidates: {}",
                        ours_at(ours, "STRING_LITERALS"),
                        Orig::Tty.path(),
                        if others.is_empty() { "none at all".to_string() } else { others.join(", ") }
                    ));
                }
            }
        }
    }

    for (ours, func, why) in STATEMENT_MASKS {
        let mine = mine(ours, "STATEMENT_MASKS");
        match statement_mask(&m, func) {
            Ok((param, theirs, line)) => {
                compared += 1;
                if theirs != mine {
                    bad.push(format!(
                        "{ours}: ours {} = {mine:#x} ({mine}); the original's `{param} & {theirs:#x}` in `{func}` at \
                         {}:{line} is {theirs} ({theirs:#x}). {why}",
                        ours_at(ours, "STATEMENT_MASKS"),
                        Orig::Tty.path()
                    ));
                }
            }
            Err(e) => bad.push(format!(
                "{ours}: ours {}; `{func}` in {} at {ORIGINAL_REV}: {e}. {why}",
                ours_at(ours, "STATEMENT_MASKS"),
                Orig::Tty.path()
            )),
        }
    }

    for (ours, why) in OURS_ONLY {
        eprintln!(
            "  not in the original: {ours} at {}: {why}",
            ours_at(ours, "OURS_ONLY")
        );
    }

    assert!(
        bad.is_empty(),
        "{} comparison(s) passed; {} problem(s):\n{}",
        compared,
        bad.len(),
        bad.join("\n")
    );
    eprintln!(
        "compared {} values with the original at {ORIGINAL_REV}; {} of our constants are ours alone ({})",
        compared,
        OURS_ONLY.len(),
        OURS_ONLY
            .iter()
            .map(|(n, _)| *n)
            .collect::<Vec<_>>()
            .join(", ")
    );
}

/// The evaluator is not vacuous: it reads each form these originals use, and refuses what it does not know rather than
/// guessing. Hand-written expectations, so a broken evaluator is caught here and not as a false match.
#[test]
fn the_evaluator_reads_the_originals_forms() {
    let mut m = Model {
        defines: BTreeMap::new(),
        text: BTreeMap::new(),
    };
    for f in Orig::ALL {
        m.text.insert(*f, Vec::new());
    }
    let define = |m: &mut Model, name: &str, text: &str, line: usize| {
        m.defines.insert(
            name.to_string(),
            Sym {
                file: Orig::S3c,
                line,
                text: text.to_string(),
                toks: tokenize(text),
            },
        );
    };
    define(&mut m, "APPLE_S5L_UCON_RXTO_ENA", "9", 249);
    define(
        &mut m,
        "APPLE_S5L_UCON_RXTO_ENA_MSK",
        "BIT(APPLE_S5L_UCON_RXTO_ENA)",
        253,
    );
    define(&mut m, "APPLE_S5L_UCON_TXTHRESH_ENA", "13", 252);
    define(
        &mut m,
        "APPLE_S5L_UCON_TXTHRESH_ENA_MSK",
        "BIT(APPLE_S5L_UCON_TXTHRESH_ENA)",
        256,
    );

    let cases: &[(&str, u64)] = &[
        ("(0x0C)", 0x0C),
        ("(1<<0)", 1),
        ("(15<<4)", 240),
        ("BIT(9)", 512),
        ("BIT(APPLE_S5L_UCON_RXTO_ENA)", 512),
        ("GENMASK(9, 3)", 0x3F8),
        (
            "(APPLE_S5L_UCON_RXTO_ENA_MSK | APPLE_S5L_UCON_TXTHRESH_ENA_MSK)",
            (1 << 9) | (1 << 13),
        ),
        ("(3<<2)", 12),
        ("4 + 3 * 2", 10),
        ("(1 + 2) * 3", 9),
        ("~0 & 0xf", 15),
        ("(0x28)", 40),
        ("7 >> 1", 3),
        ("(15<<0)", 15),
    ];
    for (text, want) in cases {
        let toks = tokenize(text).unwrap_or_else(|e| panic!("`{text}` does not tokenize: {e}"));
        // `value_of`, not `eval`: some of these cases name a `#define`, and reading a name is half of what is checked here.
        let got = m
            .value_of(&toks, text)
            .unwrap_or_else(|e| panic!("`{text}` should read as {want}: {e}"));
        assert_eq!(got, *want, "`{text}`");
    }
    assert_eq!(m.value("APPLE_S5L_UCON_RXTO_ENA_MSK").unwrap(), 1 << 9);
    assert!(
        m.value("S3C2410_ULCON").is_err(),
        "a name the model does not define must be an error, not a pass"
    );

    // The `#define` reader, on text written for the purpose: every register offset in `serial_s3c.h` is an object-like macro
    // whose body begins with a parenthesis, which must not be mistaken for a function-like macro.
    let text = "#define OFF_A\t  (0x24)\n#define FUN(x)\t (x)\n#define GONE 1\n#undef GONE\n#define DUP 2\n#define DUP 3\n";
    let mut p = Model {
        defines: BTreeMap::new(),
        text: BTreeMap::new(),
    };
    for f in Orig::ALL {
        p.text.insert(*f, Vec::new());
    }
    p.parse_defines(Orig::S3c, &logical_lines(text));
    assert_eq!(
        p.value("OFF_A").unwrap(),
        0x24,
        "an object-like macro with a parenthesised body"
    );
    assert!(
        p.lookup("FUN").is_none(),
        "a function-like macro is not recorded"
    );
    assert!(p.lookup("GONE").is_none(), "#undef removes the definition");
    assert_eq!(
        p.value("DUP").unwrap(),
        2,
        "the first #define of a name wins"
    );

    // A driver-data initializer and a statement, read from text written for the purpose.
    let c = "static const struct s5l_serial_drv_data s5l_serial_drv_data = {\n\t.info = {\n\t\t.iotype\t\t= UPIO_MEM32,\n\
             \t\t.fifosize\t= 16,\n\t},\n\t.def_cfg = {\n\t\t.ucon\t\t= APPLE_S5L_UCON_MASK,\n\t},\n};\n\
             #define S5L_SERIAL_DRV_DATA (&s5l_serial_drv_data)\n";
    let mut q = Model {
        defines: BTreeMap::new(),
        text: BTreeMap::new(),
    };
    q.parse_defines(
        Orig::S3c,
        &logical_lines("#define APPLE_S5L_UCON_MASK\t(BIT(9) | BIT(13))\n"),
    );
    q.text.insert(Orig::S3c, Vec::new());
    q.text.insert(Orig::Tty, logical_lines(c));
    let init =
        initializer(&q, Orig::Tty, "s5l_serial_drv_data").expect("the initializer written above");
    let (_, toks) = field_value(&init, "fifosize").expect(".fifosize");
    assert_eq!(q.value_of(&toks, ".fifosize").unwrap(), 16);
    let (_, toks) = field_value(&init, "ucon").expect(".ucon");
    assert_eq!(field_symbol(&toks).as_deref(), Some("APPLE_S5L_UCON_MASK"));
    assert_eq!(q.value_of(&toks, ".ucon").unwrap(), (1 << 9) | (1 << 13));
    let (_, toks) = field_value(&init, "iotype").expect(".iotype");
    assert_eq!(field_symbol(&toks).as_deref(), Some("UPIO_MEM32"));
    assert!(
        field_value(&init, "no_such_field").is_err(),
        "a field that is not there must not read as zero"
    );
    assert!(
        initializer(&q, Orig::Tty, "other_drv_data").is_err(),
        "a structure that is not there must be an error"
    );

    let stmt = "static bool\nfoo_port_set(u32 ucon)\n{\n\treturn (ucon & 0xf) != 0;\n}\n";
    let mut r = Model {
        defines: BTreeMap::new(),
        text: BTreeMap::new(),
    };
    for f in Orig::ALL {
        r.text.insert(*f, Vec::new());
    }
    r.text.insert(Orig::Tty, logical_lines(stmt));
    let (param, mask, line) =
        statement_mask(&r, "foo_port_set").expect("the function written above");
    assert_eq!((param.as_str(), mask), ("ucon", 0xf));
    assert_eq!(
        line, 4,
        "the line of the mask, counted through the comment-stripped text"
    );
    assert!(statement_mask(&r, "no_such_func").is_err());

    for bad in [
        "S3C2410_ULCON + undefined_thing",
        "(013)",
        "1 +",
        "BIT()",
        "GENMASK(3, 9)",
        "3 / 0",
    ] {
        let why = match tokenize(bad) {
            Ok(toks) => m
                .eval(&toks)
                .err()
                .unwrap_or_else(|| panic!("`{bad}` must not evaluate")),
            Err(e) => e,
        };
        assert!(
            !why.is_empty(),
            "`{bad}` was refused with no message, which would make a failure unreadable"
        );
    }
}
