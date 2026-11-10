//! A saved crash record's text (`kernel/lucina-crash/src/report.rs` writes it, `store.rs` keeps it) and kernel log lines.
//! The text came through RAM that loses bits across a reset: nothing here trusts it, every byte is read lossily, and a
//! missing piece leaves its field empty instead of failing.

use crate::event::LogLine;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Record {
    pub title: String,
    pub reason: String,
    /// From `where: <kind> on CPU`: `panic`, `soft lockup`, `hardware watchdog`, `userspace watchdog`.
    pub kind: Option<String>,
    pub cpu: Option<u32>,
    pub tid: Option<u32>,
    pub uptime_ms: Option<u64>,
    pub boot: Option<u32>,
    pub build: Option<String>,
    pub pc: Option<u64>,
    /// The words after the ESR and FAR on the `esr` line: `data abort write, translation fault L1`.
    pub fault: Option<String>,
    /// A modem record's `cause:` line (patch 1057): `watchdog interrupt (SPI 266)` or `fatal bit (SMP2P)`.
    pub modem_cause: Option<String>,
    /// An unexplained reset's `pmic:` line (patch 1113): the PMIC's word on how that boot ended, with its raw registers
    /// at the end (`lucina_pon::Pon::from_record_line` reads them back).
    pub pmic: Option<String>,
    pub backtrace: Vec<u64>,
    pub log: Vec<LogLine>,
    /// `[some bits lost]`: RAM lost bits beyond the error correction's reach.
    pub bits_lost: bool,
}

#[derive(PartialEq)]
enum Part {
    Head,
    Reason,
    Registers,
    Backtrace,
    Log,
}

fn hex(w: &str) -> Option<u64> {
    (w.len() == 16)
        .then(|| u64::from_str_radix(w, 16).ok())
        .flatten()
}

/// `5491.524 s` -> 5491524.
fn seconds_ms(s: &str) -> Option<u64> {
    let s = s.trim().trim_end_matches(" s").trim();
    let (whole, frac) = s.split_once('.').unwrap_or((s, "0"));
    let mut frac: String = frac.chars().take(3).collect();
    while frac.len() < 3 {
        frac.push('0');
    }
    Some(whole.parse::<u64>().ok()? * 1000 + frac.parse::<u64>().ok()?)
}

/// `where: panic on CPU 6, thread 12, uptime 5491.524 s, boot 11`.
fn parse_where(rec: &mut Record, rest: &str) {
    let mut parts = rest.split(", ");
    if let Some(first) = parts.next() {
        match first.split_once(" on CPU ") {
            Some((kind, cpu)) => {
                rec.kind = Some(kind.trim().to_string());
                rec.cpu = cpu.trim().parse().ok();
            }
            None => rec.kind = Some(first.trim().to_string()),
        }
    }
    for p in parts {
        if let Some(t) = p.strip_prefix("thread ") {
            rec.tid = t.trim().parse().ok();
        } else if let Some(u) = p.strip_prefix("uptime ") {
            rec.uptime_ms = seconds_ms(u);
        } else if let Some(b) = p.strip_prefix("boot ") {
            rec.boot = b.trim().parse().ok();
        }
    }
}

pub fn parse_record(text: &[u8]) -> Record {
    let text = String::from_utf8_lossy(text);
    let mut rec = Record::default();
    let mut part = Part::Head;
    let mut reason: Vec<&str> = Vec::new();
    let mut first_log = true;
    if text.contains("[some bits lost]") {
        rec.bits_lost = true;
    }
    for line in text.lines() {
        if part != Part::Log {
            if line.starts_with("last kernel log:") {
                part = Part::Log;
                continue;
            }
            if let Some(rest) = line.strip_prefix("where: ") {
                parse_where(&mut rec, rest);
                part = Part::Head;
                continue;
            }
            if let Some(rest) = line.strip_prefix("cause: ") {
                rec.modem_cause = Some(rest.trim().to_string());
                part = Part::Head;
                continue;
            }
            if let Some(rest) = line.strip_prefix("pmic: ") {
                rec.pmic = Some(rest.trim().to_string());
                part = Part::Head;
                continue;
            }
            if let Some(rest) = line.strip_prefix("build: ") {
                rec.build = Some(rest.trim().to_string());
                part = Part::Head;
                continue;
            }
            if line.starts_with("registers:") {
                part = Part::Registers;
                continue;
            }
            if line.starts_with("backtrace") {
                part = Part::Backtrace;
                continue;
            }
            if let Some(rest) = line.strip_prefix("reason: ") {
                reason.push(rest);
                part = Part::Reason;
                continue;
            }
        }
        match part {
            Part::Head => {
                if rec.title.is_empty() && !line.trim().is_empty() {
                    rec.title = line.trim().to_string();
                }
            }
            Part::Reason => reason.push(line),
            Part::Registers => {
                let words: Vec<&str> = line.split_ascii_whitespace().collect();
                if words.first() == Some(&"pc") {
                    rec.pc = words.get(1).and_then(|w| hex(w));
                } else if words.first() == Some(&"esr") {
                    // `esr 96000045  far 00000000dead0000  data abort ...`
                    if let Some(at) = line.find("far ") {
                        let after = &line[at + 4..];
                        let rest = after
                            .trim_start()
                            .split_once(char::is_whitespace)
                            .map(|(_, r)| r.trim());
                        rec.fault = rest.filter(|r| !r.is_empty()).map(str::to_string);
                    }
                }
            }
            Part::Backtrace => {
                for w in line.split_ascii_whitespace() {
                    if let Some(a) = hex(w) {
                        rec.backtrace.push(a);
                    }
                }
            }
            Part::Log => {
                if line.trim().is_empty() {
                    continue;
                }
                let mut l = parse_log_line(line);
                if first_log && !line.starts_with('[') {
                    l.mark = Some("partial".into());
                }
                first_log = false;
                rec.log.push(l);
            }
        }
    }
    rec.reason = reason.join("\n").trim_end().to_string();
    rec
}

fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| *c == '\t' || !c.is_control())
        .collect()
}

pub fn parse_log_line(line: &str) -> LogLine {
    if let Some(rest) = line.strip_prefix('[') {
        if let Some((time, text)) = rest.split_once(']') {
            if let Some(ms) = seconds_ms(time) {
                return LogLine {
                    uptime_ms: Some(ms),
                    text: clean(text.strip_prefix(' ').unwrap_or(text)),
                    mark: None,
                };
            }
        }
    }
    // /dev/kmsg: `<prio>,<seq>,<usec>,<flags>[,...];text`.
    if let Some((head, text)) = line.split_once(';') {
        let fields: Vec<&str> = head.split(',').collect();
        if fields.len() >= 3
            && fields[..3]
                .iter()
                .all(|f| !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()))
        {
            if let Ok(us) = fields[2].parse::<u64>() {
                return LogLine {
                    uptime_ms: Some(us / 1000),
                    text: clean(text),
                    mark: None,
                };
            }
        }
    }
    LogLine {
        uptime_ms: None,
        text: clean(line),
        mark: None,
    }
}

pub fn split_dump(text: &str) -> Vec<(usize, String)> {
    let mut out: Vec<(usize, String)> = Vec::new();
    for line in text.lines() {
        if let Some(slot) = line
            .strip_prefix("=== record ")
            .and_then(|s| s.trim().parse::<usize>().ok())
        {
            out.push((slot, String::new()));
            continue;
        }
        if let Some((_, body)) = out.last_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    out
}

pub fn last_uptime(log: &[LogLine]) -> Option<u64> {
    log.iter().rev().find_map(|l| l.uptime_ms)
}

pub fn subsystem(text: &str) -> Option<&str> {
    let (word, _) = text.split_once(": ")?;
    (2..=16).contains(&word.len()).then_some(word).filter(|w| {
        w.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
    })
}

pub fn fold_repeats(log: &[LogLine]) -> Vec<LogLine> {
    let mut out: Vec<LogLine> = Vec::new();
    let mut run = 0usize;
    for (i, l) in log.iter().enumerate() {
        run += 1;
        let next_same = log.get(i + 1).is_some_and(|n| n.text == l.text);
        if next_same {
            continue;
        }
        let mut kept = l.clone();
        if run > 1 {
            kept.mark = Some(format!("repeated {run} times"));
        }
        out.push(kept);
        run = 0;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const PANIC: &str = "Lucina Juno crashed
reason: Heap allocation error, layout = Layout {
    size: 0x40000,
    align: 1 (1 << 0),
}
where: panic on CPU 6, uptime 5491.524 s, boot 11
build: 5.13.0-LucinaJuno juno-9672e3f5ecc1
registers:
pc ffffffffa08cfe40  lr ffffffffa08cfe24  sp ffffdffffea27ea0  pstate 00000000
x00 0000000000000000  x01 0000000000000000  x02 0000000000000000  x03 0000000000000000
x28 0000000000000089  x29 ffffdffffea28090
backtrace (scripts/juno-symbolize.sh names them):
ffffffffa08a1338
ffffffffa08cbcfc
last kernel log:
 fill 2047, backlog 0
[ 5490.730000] wifi: alive at 5484719 ms
[ 5491.446000] Failed to allocate a large slot
[ 5491.462000] Heap allocation error, layout = Layout {
[ 5491.462000]     size: 0x40000,
[ 5491.462000] }
";

    #[test]
    fn a_panic() {
        let r = parse_record(PANIC.as_bytes());
        assert_eq!(r.title, "Lucina Juno crashed");
        assert_eq!(
            r.reason,
            "Heap allocation error, layout = Layout {\n    size: 0x40000,\n    align: 1 (1 << 0),\n}"
        );
        assert_eq!(r.kind.as_deref(), Some("panic"));
        assert_eq!(
            (r.cpu, r.tid, r.uptime_ms, r.boot),
            (Some(6), None, Some(5_491_524), Some(11))
        );
        assert_eq!(
            r.build.as_deref(),
            Some("5.13.0-LucinaJuno juno-9672e3f5ecc1")
        );
        assert_eq!(r.pc, Some(0xffff_ffff_a08c_fe40));
        assert_eq!(
            r.backtrace,
            vec![0xffff_ffff_a08a_1338, 0xffff_ffff_a08c_bcfc]
        );
        assert_eq!(r.log[0].mark.as_deref(), Some("partial"));
        let last = r.log.last().unwrap();
        assert_eq!((last.text.as_str(), last.uptime_ms), ("}", Some(5_491_462)));
        assert!(!r.bits_lost);
    }

    #[test]
    fn a_fault_with_a_thread() {
        let t = "Lucina Juno crashed\nreason: boom\nwhere: soft lockup on CPU 0, thread 12, uptime 1.5 s\nregisters:\n\
                 pc 0000000000001000  lr 0  sp 0  pstate 0\nesr 96000045  far 00000000dead0000  data abort write, translation fault L1\n\
                 backtrace (x):\n0000000000000001 0000000000000002 0000000000000003\n";
        let r = parse_record(t.as_bytes());
        assert_eq!(
            (r.kind.as_deref(), r.tid, r.uptime_ms),
            (Some("soft lockup"), Some(12), Some(1500))
        );
        assert_eq!(
            r.fault.as_deref(),
            Some("data abort write, translation fault L1")
        );
        assert_eq!(r.backtrace, vec![1, 2, 3]);
    }

    #[test]
    fn an_unexplained_reset() {
        let t = "The machine reset with no crash report\nreason: nothing recorded a crash: a hardware watchdog\nlast kernel log:\n\nabc, def\n[    3.032000] wifi: wlfw: the service is up\n[some bits lost]\n";
        let r = parse_record(t.as_bytes());
        assert_eq!(r.kind, None);
        assert_eq!(r.reason, "nothing recorded a crash: a hardware watchdog");
        assert_eq!(r.log[0].mark.as_deref(), Some("partial"));
        assert_eq!(last_uptime(&r.log), Some(3032));
        assert!(r.bits_lost);
    }

    #[test]
    fn garbage_does_not_panic() {
        let _ = parse_record(&[0xff, 0x00, 0x1b, b'[', b'\n', 0xfe]);
        let _ = parse_record(b"");
        let _ = parse_record(b"where: \nreason: \nlast kernel log:\n[ 1.2.3] x\n");
    }

    #[test]
    fn a_modem_record() {
        let t = "The modem DSP failed\ncause: watchdog interrupt (SPI 266)\nreason: err_qdi.c: PD crash\nlast kernel log:\n[  101.500000] wifi: wmi: command timed out\n";
        let r = parse_record(t.as_bytes());
        assert_eq!(r.title, "The modem DSP failed");
        assert_eq!(
            r.modem_cause.as_deref(),
            Some("watchdog interrupt (SPI 266)")
        );
        assert_eq!(r.reason, "err_qdi.c: PD crash");
        assert_eq!(r.log.len(), 1);
        let r = parse_record(
            b"The modem DSP failed\ncause: fatal bit (SMP2P)\nreason: \nlast kernel log:\n",
        );
        assert_eq!(
            (r.modem_cause.as_deref(), r.reason.as_str()),
            (Some("fatal bit (SMP2P)"), "")
        );
    }

    #[test]
    fn an_unexplained_reset_with_the_pmic_line() {
        let t = "The machine reset with no crash report\nreason: nothing recorded a crash: a hardware watchdog, a firmware reset or a hang with interrupts off\npmic: last power-off by PS_HOLD (the processor itself switched the power off); this boot by hard reset, cold [c0 41 c2 00 c5 02 c7 80 c8 00 c9 08 ca 80]\nlast kernel log:\n[  5.000000] xhci: controller fault\n";
        let r = parse_record(t.as_bytes());
        assert_eq!(
            r.reason,
            "nothing recorded a crash: a hardware watchdog, a firmware reset or a hang with interrupts off"
        );
        let pmic = r.pmic.as_deref().unwrap();
        assert!(pmic.starts_with("last power-off by PS_HOLD"), "{pmic}");
        let pon = lucina_pon::Pon::from_record_line(pmic).unwrap();
        assert_eq!(pon.off_reason(), Some(lucina_pon::OffReason::PsHold));
        assert_eq!(r.log.len(), 1);
        // Older records have none.
        let old = parse_record(b"The machine reset with no crash report\nreason: nothing\nlast kernel log:\n");
        assert_eq!((old.pmic, old.reason.as_str()), (None, "nothing"));
    }

    #[test]
    fn log_lines() {
        let l = parse_log_line("[  116.677000] venus: rpmh: x");
        assert_eq!(
            (l.uptime_ms, l.text.as_str()),
            (Some(116_677), "venus: rpmh: x")
        );
        let l = parse_log_line("6,234,372000,-;power: PON");
        assert_eq!((l.uptime_ms, l.text.as_str()), (Some(372), "power: PON"));
        let l = parse_log_line("plain\x07 text");
        assert_eq!((l.uptime_ms, l.text.as_str()), (None, "plain text"));
        assert_eq!(subsystem("wifi: alive at"), Some("wifi"));
        assert_eq!(subsystem("Heap allocation error, x: y"), None);
        assert_eq!(subsystem("[kexec] entering"), None);
    }

    #[test]
    fn repeats_fold_to_their_last_line() {
        let mut log: Vec<LogLine> = (0..5)
            .map(|i| LogLine {
                uptime_ms: Some(i),
                text: "spam".into(),
                mark: None,
            })
            .collect();
        log.push(LogLine {
            uptime_ms: Some(9),
            text: "other".into(),
            mark: None,
        });
        let f = fold_repeats(&log);
        assert_eq!(f.len(), 2);
        assert_eq!(
            (f[0].uptime_ms, f[0].mark.as_deref()),
            (Some(4), Some("repeated 5 times"))
        );
        assert_eq!(f[1].text, "other");
    }

    #[test]
    fn dumps_split() {
        let d = "junk\n=== record 2\na\nb\n=== record 5\nc\n";
        assert_eq!(
            split_dump(d),
            vec![(2, "a\nb\n".to_string()), (5, "c\n".to_string())]
        );
    }
}
