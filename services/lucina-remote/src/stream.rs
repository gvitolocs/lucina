//! The reload stream: a short text header, then the kernel image and the initramfs back to back.
//!
//! ```text
//! LUCINA-RELOAD 1
//! kernel <bytes> <sha256 hex>
//! initrd <bytes> <sha256 hex>
//! cmdline <hex of the bytes>        (optional; without it the running kernel's command line is kept)
//! end
//! <kernel image><initramfs>
//! ```
//!
//! The header is text so that a shell can write it (`printf`, `sha256sum`, `stat`) where the `lucina-reload pack`
//! tool is not built.

use std::io::{self, BufRead};

use sha2::{Digest, Sha256};

pub const MAGIC: &str = "LUCINA-RELOAD 1";

/// The largest kernel or initramfs accepted (RAM on the targets is a few GiB).
pub const MAX_PART: u64 = 1 << 30;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    pub size: u64,
    pub sha256: [u8; 32],
}

impl Part {
    pub fn of(data: &[u8]) -> Self {
        Part {
            size: data.len() as u64,
            sha256: Sha256::digest(data).into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub kernel: Part,
    pub initrd: Part,
    pub cmdline: Option<Vec<u8>>,
}

impl Header {
    pub fn encode(&self) -> Vec<u8> {
        let mut s = format!(
            "{MAGIC}\nkernel {} {}\ninitrd {} {}\n",
            self.kernel.size,
            hex(&self.kernel.sha256),
            self.initrd.size,
            hex(&self.initrd.sha256)
        );
        if let Some(cmdline) = &self.cmdline {
            s += &format!("cmdline {}\n", hex(cmdline));
        }
        s += "end\n";
        s.into_bytes()
    }

    /// Reads a header, leaving `r` at the first byte of the kernel image.
    pub fn read(r: &mut impl BufRead) -> io::Result<Header> {
        let bad = |msg: String| io::Error::new(io::ErrorKind::InvalidData, msg);
        let mut line = String::new();
        let mut next_line = |line: &mut String| -> io::Result<()> {
            line.clear();
            // Bound the line, so that a wrong stream (a raw kernel image) does not fill the memory.
            let n = io::Read::take(&mut *r, 4096).read_line(line)?;
            if n == 0 || !line.ends_with('\n') {
                return Err(bad("the reload header ended early".into()));
            }
            line.pop();
            Ok(())
        };
        next_line(&mut line)?;
        if line != MAGIC {
            return Err(bad(format!(
                "not a reload stream (first line {:?})",
                truncate(&line)
            )));
        }
        let (mut kernel, mut initrd, mut cmdline) = (None, None, None);
        loop {
            next_line(&mut line)?;
            let mut fields = line.split(' ');
            match (fields.next(), fields.next(), fields.next(), fields.next()) {
                (Some("end"), None, ..) => break,
                (Some(name @ ("kernel" | "initrd")), Some(size), Some(sum), None) => {
                    let size: u64 = size
                        .parse()
                        .map_err(|_| bad(format!("bad {name} size {size:?}")))?;
                    if size > MAX_PART {
                        return Err(bad(format!("{name} is {size} bytes, more than {MAX_PART}")));
                    }
                    let sha256 = unhex(sum)
                        .and_then(|v| <[u8; 32]>::try_from(v).ok())
                        .ok_or_else(|| bad(format!("bad {name} sha256 {sum:?}")))?;
                    let slot = if name == "kernel" {
                        &mut kernel
                    } else {
                        &mut initrd
                    };
                    if slot.replace(Part { size, sha256 }).is_some() {
                        return Err(bad(format!("{name} given twice")));
                    }
                }
                (Some("cmdline"), Some(h), None, _) => {
                    let v = unhex(h).ok_or_else(|| bad("bad cmdline hex".into()))?;
                    if cmdline.replace(v).is_some() {
                        return Err(bad("cmdline given twice".into()));
                    }
                }
                _ => return Err(bad(format!("bad header line {:?}", truncate(&line)))),
            }
        }
        Ok(Header {
            kernel: kernel.ok_or_else(|| bad("no kernel line".into()))?,
            initrd: initrd.ok_or_else(|| bad("no initrd line".into()))?,
            cmdline,
        })
    }
}

fn truncate(s: &str) -> &str {
    let end = s.char_indices().nth(60).map_or(s.len(), |(i, _)| i);
    &s[..end]
}

pub fn hex(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

/// The command line with kernel arguments added before the `--` that starts init's arguments. An added argument
/// replaces those with the same key (`lucina.hid=report` replaces `lucina.hid=off`), so that nothing depends on which
/// of two values the kernel takes. Init's arguments stay as they are.
pub fn add_kernel_args(cmdline: &str, add: &str) -> String {
    let cmdline = cmdline.trim();
    let (kernel, init) = match cmdline.strip_prefix("-- ") {
        Some(init) => ("", Some(init)),
        None => match cmdline.split_once(" -- ") {
            Some((kernel, init)) => (kernel, Some(init)),
            None => (cmdline.strip_suffix(" --").unwrap_or(cmdline), None),
        },
    };
    let key = |arg: &'_ str| arg.split('=').next().unwrap_or("").to_string();
    let added: Vec<&str> = add.split_whitespace().collect();
    let kept = kernel
        .split_whitespace()
        .filter(|arg| !added.iter().any(|a| key(a) == key(arg)));
    let mut args: Vec<&str> = kept.collect();
    args.extend(&added);
    let mut out = args.join(" ");
    if let Some(init) = init {
        out += " -- ";
        out += init;
    }
    out
}

/// The command line without the kernel arguments named `key` (before the `--`). A reload drops `lucina.trial`: only the loader
/// sets it, on a real boot of a generation on trial, and a reloaded kernel that kept it would run the trial again, mark the
/// generation good on a kernel that is not the generation's, and start its watch after the good mark over (2026-11-05).
pub fn drop_kernel_arg(cmdline: &str, key: &str) -> String {
    let cmdline = cmdline.trim();
    let (kernel, init) = match cmdline.strip_prefix("-- ") {
        Some(init) => ("", Some(init)),
        None => match cmdline.split_once(" -- ") {
            Some((kernel, init)) => (kernel, Some(init)),
            None => (cmdline.strip_suffix(" --").unwrap_or(cmdline), None),
        },
    };
    let mut out = kernel
        .split_whitespace()
        .filter(|arg| arg.split('=').next().unwrap_or("") != key)
        .collect::<Vec<_>>()
        .join(" ");
    if let Some(init) = init {
        if !out.is_empty() {
            out += " ";
        }
        out += "-- ";
        out += init;
    }
    out
}

/// The arm64 Linux image header (Documentation/arch/arm64/booting.rst), as much as reload checks.
#[derive(Debug, PartialEq, Eq)]
pub struct Arm64Image {
    pub text_offset: u64,
    pub image_size: u64,
}

/// Checks the arm64 image magic ("ARM\x64" at 0x38) and reads the load offset and the effective size.
pub fn arm64_image(head: &[u8]) -> Option<Arm64Image> {
    let word = |at: usize| -> Option<u64> {
        Some(u64::from_le_bytes(head.get(at..at + 8)?.try_into().ok()?))
    };
    if head.get(0x38..0x3c)? != b"ARM\x64" {
        return None;
    }
    Some(Arm64Image {
        text_offset: word(8)?,
        image_size: word(16)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header() -> Header {
        Header {
            kernel: Part::of(b"kernel"),
            initrd: Part::of(b"initramfs"),
            cmdline: None,
        }
    }

    #[test]
    fn round_trip() {
        for cmdline in [
            None,
            Some(b"console=tty0 -- sh -l".to_vec()),
            Some(Vec::new()),
        ] {
            let h = Header {
                cmdline,
                ..header()
            };
            let mut bytes = h.encode();
            bytes.extend_from_slice(b"kernelinitramfs");
            let mut r = io::Cursor::new(bytes);
            assert_eq!(Header::read(&mut r).unwrap(), h);
            assert_eq!(r.position() as usize, h.encode().len());
        }
    }

    #[test]
    fn shell_written_header() {
        // What `printf` + `sha256sum` in a shell produce.
        let text = format!(
            "LUCINA-RELOAD 1\nkernel 6 {}\ninitrd 9 {}\nend\n",
            hex(&Part::of(b"kernel").sha256),
            hex(&Part::of(b"initramfs").sha256)
        );
        assert_eq!(Header::read(&mut text.as_bytes()).unwrap(), header());
    }

    #[test]
    fn refusals() {
        let ok = String::from_utf8(header().encode()).unwrap();
        let cases = [
            "\u{7f}ELF garbage\n".to_string(),
            ok.replace("LUCINA-RELOAD 1", "LUCINA-RELOAD 2"),
            ok.replace("end\n", ""),
            ok.replace("kernel 6", "kernel x"),
            ok.replace("kernel 6", &format!("kernel {}", MAX_PART + 1)),
            ok.replace("initrd 9 ", "initrd 9 zz"),
            ok.replace("end\n", "kernel 6 00\nend\n"),
            ok.replace("end\n", "bogus\nend\n"),
            "x".repeat(10000),
        ];
        for case in cases {
            assert!(
                Header::read(&mut case.as_bytes()).is_err(),
                "accepted {case:?}"
            );
        }
    }

    #[test]
    fn adding_kernel_args() {
        let line =
            "SHELL=/bin/sh console=tty0 maxcpus=1 lucina.hid=off -- /etc/lucina/remote.sh sh -l\n";
        assert_eq!(
            add_kernel_args(line, "lucina.charge=report"),
            "SHELL=/bin/sh console=tty0 maxcpus=1 lucina.hid=off lucina.charge=report -- /etc/lucina/remote.sh sh -l"
        );
        assert_eq!(
            add_kernel_args(line, "lucina.hid=report  ufs=probe"),
            "SHELL=/bin/sh console=tty0 maxcpus=1 lucina.hid=report ufs=probe -- /etc/lucina/remote.sh sh -l"
        );
        // A bare flag replaces itself; init's arguments are never touched.
        assert_eq!(
            add_kernel_args("nosmp -- a nosmp=1", "nosmp"),
            "nosmp -- a nosmp=1"
        );
        assert_eq!(add_kernel_args("a=1", "b=2"), "a=1 b=2");
        assert_eq!(add_kernel_args("-- sh", "b=2"), "b=2 -- sh");
        assert_eq!(add_kernel_args("a=1 --", "b=2"), "a=1 b=2");
        assert_eq!(add_kernel_args("a=1 -- sh", ""), "a=1 -- sh");
    }

    #[test]
    fn arm64_header() {
        let mut head = vec![0u8; 64];
        head[8..16].copy_from_slice(&0x20_0000u64.to_le_bytes());
        head[16..24].copy_from_slice(&0x60_0000u64.to_le_bytes());
        assert_eq!(arm64_image(&head), None);
        head[0x38..0x3c].copy_from_slice(b"ARM\x64");
        assert_eq!(
            arm64_image(&head),
            Some(Arm64Image {
                text_offset: 0x20_0000,
                image_size: 0x60_0000
            })
        );
        assert_eq!(arm64_image(&head[..40]), None);
    }
    #[test]
    fn a_reload_drops_the_trial_flag_and_nothing_else() {
        let line = "console=tty0 lucina.trial=on:32 lucina.charge=on -- /etc/lucina/remote.sh sh lucina.trial=x";
        assert_eq!(
            drop_kernel_arg(line, "lucina.trial"),
            "console=tty0 lucina.charge=on -- /etc/lucina/remote.sh sh lucina.trial=x"
        );
        assert_eq!(drop_kernel_arg("lucina.trial=rolledback:3", "lucina.trial"), "");
        assert_eq!(drop_kernel_arg("lucina.trial=on:1 -- sh", "lucina.trial"), "-- sh");
        assert_eq!(drop_kernel_arg("a=1 lucina.trialx=2", "lucina.trial"), "a=1 lucina.trialx=2");
    }
}
