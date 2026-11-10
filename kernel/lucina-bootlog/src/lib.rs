//! The parts of lucina-bootlog that do not touch the machine: which disks it may use, the next log file's number and
//! the text of a kernel log line.

/// The folder on the stick that holds the logs. A partition without it is left alone.
pub const LOG_DIR: &str = "lucinaos";

/// Whether a /dev name is a USB disk or one of its partitions: `sd`, letters, then an optional number. USB mass storage
/// is the only driver with these names (`aster-usb`, `disk_name`). The laptop's internal disk is `ufs0lun0`, virtio
/// disks are `vda`: they never match, so that nothing is written there.
pub fn is_usb_disk_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("sd") else {
        return false;
    };
    let digits = rest.trim_start_matches(|c: char| c.is_ascii_lowercase());
    digits.len() < rest.len() && digits.bytes().all(|b| b.is_ascii_digit())
}

/// Orders candidates: partitions before whole disks, in name order. A stick holds a partition table more often than a
/// bare file system.
pub fn order_candidates(names: &mut [String]) {
    names.sort_by_key(|name| {
        (
            !name.as_bytes().last().is_some_and(u8::is_ascii_digit),
            name.clone(),
        )
    });
}

/// The number of the next log file: one above the highest `juno-log-<N>.txt` among `names`, whatever their case (FAT).
pub fn next_log_number<S: AsRef<str>>(names: impl IntoIterator<Item = S>) -> u64 {
    names
        .into_iter()
        .filter_map(|name| {
            let name = name.as_ref().to_ascii_lowercase();
            name.strip_prefix("juno-log-")?
                .strip_suffix(".txt")?
                .parse::<u64>()
                .ok()
        })
        .max()
        .map_or(1, |n| n.saturating_add(1))
}

/// What `/sys/fs/pstore/status` says about the saved crashes: the slot, sequence number and boot of each.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SavedCrashes {
    pub boot: u32,
    /// `crashed` or `vanished` means the last boot's console tells how it ended.
    pub last_boot: String,
    pub records: Vec<(usize, u32, u32)>,
}

/// Reads `/sys/fs/pstore/status`. Lines it does not know are skipped.
pub fn parse_saved_status(text: &str) -> SavedCrashes {
    let mut saved = SavedCrashes::default();
    for line in text.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.as_slice() {
            ["boot", boot] => saved.boot = boot.parse().unwrap_or(0),
            ["last_boot", how] => saved.last_boot = (*how).to_string(),
            ["record", slot, "seq", seq, "boot", boot, ..] => {
                if let (Ok(slot), Ok(seq), Ok(boot)) = (slot.parse(), seq.parse(), boot.parse()) {
                    saved.records.push((slot, seq, boot));
                }
            }
            _ => {}
        }
    }
    saved
}

/// The name of a saved crash on the stick. It is the same on every boot that finds this crash, so a crash is copied once.
pub fn crash_file_name(boot: u32, seq: u32) -> String {
    format!("juno-crash-{boot}-{seq}.txt")
}

/// The name of the console of a boot that ended badly.
pub fn last_console_file_name(boot: u32) -> String {
    format!("juno-lastlog-{boot}.txt")
}

/// The log file's name for a number.
pub fn log_file_name(number: u64) -> String {
    format!("juno-log-{number}.txt")
}

/// Turns a /dev/kmsg record (`<level>,<sequence>,<timestamp in us>,<flags>;<escaped text>\n`) into a line as the console
/// prints it, `[    12.345] INFO  : text\n`, with its escapes undone and terminal colour sequences removed. A record
/// that does not have this form is kept as it is.
pub fn format_record(record: &[u8]) -> String {
    let text = String::from_utf8_lossy(record);
    let text = text.trim_end_matches('\n');
    let Some((head, message)) = text.split_once(';') else {
        return format!("{text}\n");
    };
    let mut fields = head.split(',');
    let (Some(level), Some(_), Some(us)) = (
        fields.next().and_then(|f| f.parse::<u32>().ok()),
        fields.next(),
        fields.next().and_then(|f| f.parse::<u64>().ok()),
    ) else {
        return format!("{text}\n");
    };
    format!(
        "[{:>6}.{:03}] {:<6}: {}\n",
        us / 1_000_000,
        us / 1000 % 1000,
        level_name(level),
        strip_colours(&unescape(message))
    )
}

fn level_name(level: u32) -> &'static str {
    match level % 8 {
        0 => "EMERG",
        1 => "ALERT",
        2 => "CRIT",
        3 => "ERROR",
        4 => "WARN",
        5 => "NOTICE",
        6 => "INFO",
        _ => "DEBUG",
    }
}

/// Undoes /dev/kmsg's `\xNN` escapes.
fn unescape(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let value = (bytes[i] == b'\\' && bytes.get(i + 1) == Some(&b'x'))
            .then(|| text.get(i + 2..i + 4))
            .flatten()
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match value {
            Some(value) => {
                out.push(value);
                i += 4;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Removes `ESC [ ... letter` sequences, which the kernel's logo is full of.
fn strip_colours(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
        } else if chars.next() == Some('[') {
            chars.find(char::is_ascii_alphabetic);
        }
    }
    out
}

/// Whether a kernel log line tells about a USB device: the report's USB section, as the kernel exposes no list of them.
pub fn is_usb_device_line(line: &str) -> bool {
    [
        "enumerated",
        "no driver",
        ": driver ",
        "registered as",
        ": cfg ",
    ]
    .iter()
    .any(|text| line.contains(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_status_is_read() {
        let saved = parse_saved_status(
            "boot 3\ncrashes_in_a_row 2\nlast_boot crashed\nsomething new\n\
             record 0 seq 0 boot 1 kind panic len 900 reason boom at x.rs:1\n\
             record 1 seq 1 boot 2 kind unexplained reset len 800 reason nothing\n\
             record x seq 1 boot 2\n",
        );
        assert_eq!(saved.boot, 3);
        assert_eq!(saved.last_boot, "crashed");
        assert_eq!(saved.records, [(0, 0, 1), (1, 1, 2)]);
        assert_eq!(parse_saved_status(""), SavedCrashes::default());
    }

    #[test]
    fn saved_crash_file_names() {
        assert_eq!(crash_file_name(3, 7), "juno-crash-3-7.txt");
        assert_eq!(last_console_file_name(4), "juno-lastlog-4.txt");
    }

    #[test]
    fn only_usb_disk_names_match() {
        for name in ["sda", "sda4", "sdb", "sdb12", "sdaa", "sdaa1"] {
            assert!(is_usb_disk_name(name), "{name}");
        }
        for name in [
            "sd",
            "sd1",
            "sda4x",
            "vda",
            "vda1",
            "ufs0lun0",
            "ufs0lun0p1",
            "nvme0n1",
            "mmcblk0",
            "sda-4",
            "SDA",
            "tty0",
        ] {
            assert!(!is_usb_disk_name(name), "{name}");
        }
    }

    #[test]
    fn partitions_come_before_whole_disks() {
        let mut names = ["sda", "sdb1", "sda4", "sda1", "sdb"].map(String::from);
        order_candidates(&mut names);
        assert_eq!(names, ["sda1", "sda4", "sdb1", "sda", "sdb"]);
    }

    #[test]
    fn the_next_number_is_above_the_highest() {
        assert_eq!(next_log_number(Vec::<String>::new()), 1);
        assert_eq!(next_log_number(["grub.cfg", "wipe-log.txt"]), 1);
        assert_eq!(next_log_number(["juno-log-1.txt", "juno-log-3.txt"]), 4);
        assert_eq!(next_log_number(["JUNO-LOG-9.TXT", "juno-log-x.txt"]), 10);
        assert_eq!(log_file_name(4), "juno-log-4.txt");
    }

    #[test]
    fn a_record_becomes_a_console_line() {
        assert_eq!(
            format_record(b"6,17,12345678,-;usb: xHCI running\n"),
            "[    12.345] INFO  : usb: xHCI running\n"
        );
        assert_eq!(
            format_record(b"4,0,0,-;Unimplemented syscall number: 293\n"),
            "[     0.000] WARN  : Unimplemented syscall number: 293\n"
        );
    }

    #[test]
    fn escapes_are_undone_and_colours_removed() {
        assert_eq!(
            format_record(b"6,1,1000,-;\\x1b[38;2;1;2;3mlogo\\x1b[0m a\\x5cb \\xc3\\xa9;c\n"),
            "[     0.001] INFO  : logo a\\b \u{e9};c\n"
        );
    }

    #[test]
    fn a_record_of_another_form_is_kept() {
        assert_eq!(format_record(b"no fields\n"), "no fields\n");
        assert_eq!(format_record(b"x,y;text\n"), "x,y;text\n");
    }

    #[test]
    fn usb_lines_are_recognised() {
        assert!(is_usb_device_line(
            "xhci/dev3: device 0bda:8152 (USB 2.10, class 00) enumerated, interfaces []"
        ));
        assert!(is_usb_device_line(
            "xhci/dev2: hub 0424:2514 (USB 2.00) enumerated"
        ));
        assert!(!is_usb_device_line("xhci: running"));
    }
}
