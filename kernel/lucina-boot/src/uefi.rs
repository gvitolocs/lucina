//! The decisions LucinaUEFI makes (`boot/lucina-uefi/`, the UEFI loader for laptops such as the Galaxy Book Go),
//! kept apart from its firmware calls so they run and are tested on the host.
//!
//! * [`Config`]: `BOOT.CFG`, the loader's only input besides the files it names.
//! * [`load_options_args`], [`merge_cmdline`]: kernel arguments a boot manager passes when it starts the loader.
//! * [`device_path_file`], [`resolve`]: where BOOT.CFG and the files it names are (next to the loader's own EFI
//!   file, or `\LUCINA\`).
//! * [`MemDesc`], [`memory_ranges`]: the UEFI memory map turned into the RAM the kernel may use (`/memory`).
//!   Firmware carve-outs (runtime services, ACPI, reserved, MMIO, unusable) are left out entirely, so the kernel
//!   never maps them; the device tree's own `/reserved-memory` stays as it is.
//! * [`Gop`], [`framebuffer`]: the GOP mode turned into a `/chosen` `simple-framebuffer` node.
//! * [`kernel_window`], [`suggest_base`]: where a kernel built for a given `OSTD_ARM_DRAM_BASE` must go, and a base
//!   that would fit when it does not.
//! * [`TreeValues`], [`edits`]: the property edits applied with [`lucina_fdt::patch`].

use core::fmt::Write as _;

use lucina_fdt::{Edit, Fdt};

use crate::{Range, image::Image};

/// UEFI pages are always 4 KiB.
pub const PAGE: u64 = 4096;
/// Most kernel candidates in [`Config`].
pub const MAX_KERNELS: usize = 4;
/// Most RAM ranges written to `/memory`.
pub const MAX_RANGES: usize = 128;
/// Asterinas' boot tables map the kernel image below 3 GiB (patch 0001's linker check).
pub const KERNEL_LIMIT: u64 = 0xc000_0000;
/// The kernel window is allocated in whole 2 MiB blocks: the boot code maps the image with 2 MiB blocks.
pub const BLOCK: u64 = 2 << 20;

// ---------------------------------------------------------------------------------------------------------------
// BOOT.CFG
// ---------------------------------------------------------------------------------------------------------------

/// A kernel image and the `OSTD_ARM_DRAM_BASE` it was built with (Asterinas runs only at its link address,
/// `dram_base + text_offset`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KernelChoice<'a> {
    pub path: &'a str,
    pub dram_base: u64,
}

/// `\LUCINA\BOOT.CFG`: `key = value` lines, `#` comments.
///
/// ```text
/// kernel  = \LUCINA\KERNEL-A0.IMG 0xa0000000   # tried in order; the first whose window is free RAM wins
/// initrd  = \LUCINA\INITRAMFS.GZ                # optional
/// dtb     = \LUCINA\GBGO.DTB                    # optional: without it, the firmware's device tree
/// cmdline = console=tty0 loglevel=info -- sh -l
/// pause   = 10                                  # seconds to show the summary before ExitBootServices
/// verbose = 1                                   # also print the whole memory map on screen
/// quiet   = 1                                   # nothing on screen but the interface's background, unless it fails; no log file either
/// log     = \LUCINA\LOG.TXT                     # optional: written before ExitBootServices
/// pstore  = off                                 # optional: no RAM region for the kernel's saved crash log
/// rng     = off                                 # optional: do not ask the firmware for a random seed
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config<'a> {
    pub kernels: [Option<KernelChoice<'a>>; MAX_KERNELS],
    pub initrd: Option<&'a str>,
    pub dtb: Option<&'a str>,
    pub cmdline: &'a str,
    pub pause_s: u32,
    pub verbose: bool,
    /// The normal path shows nothing: no text, no band, no log file. A failure shows and writes everything.
    pub quiet: bool,
    pub log: Option<&'a str>,
    /// Whether the kernel gets a RAM region for its saved crash log, [`PSTORE_BASES`].
    pub pstore: bool,
    /// Whether to ask the firmware's `EFI_RNG_PROTOCOL` for the kernel's seed.
    pub rng: bool,
    /// `board = auto`: find the board from the device tree and SMBIOS (`crate::board`), start nothing on an unknown one.
    pub board: bool,
}

/// The generation the installer wrote on a configuration's first lines.
pub fn config_generation(text: &str) -> Option<u32> {
    text.lines().find_map(|l| {
        l.trim()
            .strip_prefix("# lucina-install-disk generation ")?
            .trim()
            .parse()
            .ok()
    })
}

/// The size of the RAM region for the kernel's saved crash log (`lucina_crash::store`).
pub const PSTORE_SIZE: u64 = 2 * 1024 * 1024;

/// Where that region may go, tried in order. It must land in the same place on every boot, or a crash log is lost, so
/// these are fixed addresses. The Galaxy Book Go's free RAM (kernel log, 2026-11-02) has a large range `a0e00000..bdc00000`: the
/// first address is high in it, above what a kernel's frames reach (lowest first) and the kernel candidates, and below where
/// firmware allocates (from the top of RAM). The others are its smaller free ranges, and QEMU's RAM with 2 GiB takes them all.
pub const PSTORE_BASES: [u64; 3] = [0xbd00_0000, 0x9f80_0000, 0x9f00_0000];

/// The node that holds the region, which [`TreeValues`] adds below `/reserved-memory`.
pub const RESERVED_MEMORY: &str = "/reserved-memory";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigError {
    /// A line without `=`.
    NoEquals(usize),
    /// A key the loader does not know.
    UnknownKey(usize),
    /// A value that does not parse (bad number, missing base, empty path).
    BadValue(usize),
    /// A key other than `kernel` given twice.
    Duplicate(usize),
    /// More than [`MAX_KERNELS`] `kernel` lines.
    TooManyKernels(usize),
    /// No `kernel` line.
    NoKernel,
}

impl core::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match *self {
            ConfigError::NoEquals(l) => write!(f, "line {l}: expected key = value"),
            ConfigError::UnknownKey(l) => write!(f, "line {l}: unknown key"),
            ConfigError::BadValue(l) => write!(f, "line {l}: bad value"),
            ConfigError::Duplicate(l) => write!(f, "line {l}: key given twice"),
            ConfigError::TooManyKernels(l) => {
                write!(f, "line {l}: more than {MAX_KERNELS} kernels")
            }
            ConfigError::NoKernel => f.write_str("no kernel line"),
        }
    }
}

/// Parses a hexadecimal number with an optional `0x` prefix and `_` separators.
pub fn parse_hex(s: &str) -> Option<u64> {
    let digits = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    if digits.is_empty() {
        return None;
    }
    let mut v: u64 = 0;
    for c in digits.chars() {
        if c == '_' {
            continue;
        }
        v = v.checked_mul(16)?.checked_add(c.to_digit(16)? as u64)?;
    }
    Some(v)
}

impl<'a> Config<'a> {
    pub fn parse(text: &'a str) -> Result<Config<'a>, ConfigError> {
        let mut c = Config {
            kernels: [None; MAX_KERNELS],
            initrd: None,
            dtb: None,
            cmdline: "",
            pause_s: 0,
            verbose: false,
            quiet: false,
            log: None,
            pstore: true,
            rng: true,
            board: false,
        };
        let (mut n_kernels, mut seen) = (0usize, [false; 10]);
        for (i, raw) in text.lines().enumerate() {
            let line_no = i + 1;
            let line = raw.trim_end_matches('\r');
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let (key, value) = trimmed
                .split_once('=')
                .ok_or(ConfigError::NoEquals(line_no))?;
            let key = key.trim();
            // The command line is taken verbatim (it may contain `#`); every other value loses its comment.
            let value = if key == "cmdline" {
                value.trim()
            } else {
                value.split('#').next().unwrap_or("").trim()
            };
            let bad = ConfigError::BadValue(line_no);
            let mut once = |slot: usize| {
                if core::mem::replace(&mut seen[slot], true) {
                    Err(ConfigError::Duplicate(line_no))
                } else {
                    Ok(())
                }
            };
            match key {
                "kernel" => {
                    let mut parts = value.split_whitespace();
                    let path = parts.next().ok_or(bad)?;
                    let dram_base = parts.next().and_then(parse_hex).ok_or(bad)?;
                    if parts.next().is_some() || !dram_base.is_multiple_of(BLOCK) {
                        return Err(bad);
                    }
                    if n_kernels == MAX_KERNELS {
                        return Err(ConfigError::TooManyKernels(line_no));
                    }
                    c.kernels[n_kernels] = Some(KernelChoice { path, dram_base });
                    n_kernels += 1;
                }
                "initrd" | "dtb" | "log" => {
                    if value.is_empty() || value.contains(char::is_whitespace) {
                        return Err(bad);
                    }
                    let slot = match key {
                        "initrd" => &mut c.initrd,
                        "dtb" => &mut c.dtb,
                        _ => &mut c.log,
                    };
                    once(match key {
                        "initrd" => 0,
                        "dtb" => 1,
                        _ => 2,
                    })?;
                    *slot = Some(value);
                }
                "cmdline" => {
                    once(3)?;
                    c.cmdline = value;
                }
                "pause" => {
                    once(4)?;
                    c.pause_s = value.parse().ok().filter(|&s| s <= 600).ok_or(bad)?;
                }
                "verbose" => {
                    once(5)?;
                    c.verbose = match value {
                        "0" => false,
                        "1" => true,
                        _ => return Err(bad),
                    };
                }
                "quiet" => {
                    once(7)?;
                    c.quiet = match value {
                        "0" => false,
                        "1" => true,
                        _ => return Err(bad),
                    };
                }
                "rng" => {
                    once(8)?;
                    c.rng = match value {
                        "off" => false,
                        "on" => true,
                        _ => return Err(bad),
                    };
                }
                "board" => {
                    once(9)?;
                    c.board = match value {
                        "auto" => true,
                        _ => return Err(bad),
                    };
                }
                "pstore" => {
                    once(6)?;
                    c.pstore = match value {
                        "off" => false,
                        "on" => true,
                        _ => return Err(bad),
                    };
                }
                _ => return Err(ConfigError::UnknownKey(line_no)),
            }
        }
        if n_kernels == 0 {
            return Err(ConfigError::NoKernel);
        }
        Ok(c)
    }

    pub fn kernels(&self) -> impl Iterator<Item = KernelChoice<'a>> + '_ {
        self.kernels.iter().flatten().copied()
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------------------------------------------

/// Longest path the loader opens, terminator included (its UCS-2 buffers hold 260 units).
pub const MAX_PATH: usize = 260;

/// The file part of a UEFI device path (`EFI_LOADED_IMAGE_PROTOCOL.FilePath`): its media file path nodes (type 4,
/// subtype 4) joined into one absolute ASCII path with `\` separators. Firmware puts the whole path in one node,
/// some boot managers one node per directory, GRUB's chainloader may use `/`. None when there is no file node, a
/// name is not ASCII, the path does not fit or the node list is malformed.
pub fn device_path_file<'b>(dp: &[u8], out: &'b mut [u8; MAX_PATH]) -> Option<&'b str> {
    out[0] = b'\\';
    let mut n = 1;
    let mut rest = dp;
    loop {
        let len = usize::from(u16::from_le_bytes([*rest.get(2)?, *rest.get(3)?]));
        if len < 4 || len > rest.len() {
            return None;
        }
        match (rest[0], rest[1]) {
            (0x7f, 0xff) => break,
            (4, 4) => {
                if out[n - 1] != b'\\' {
                    out[n] = b'\\';
                    n += 1;
                }
                for c in rest[4..len].as_chunks::<2>().0 {
                    let b = match u16::from_le_bytes(*c) {
                        0 => break,
                        0x2f | 0x5c => b'\\',
                        u @ 0x20..0x7f => u as u8,
                        _ => return None,
                    };
                    if b == b'\\' && out[n - 1] == b'\\' {
                        continue;
                    }
                    if n + 1 >= MAX_PATH {
                        return None;
                    }
                    out[n] = b;
                    n += 1;
                }
            }
            _ => {}
        }
        rest = &rest[len..];
    }
    let n = if n > 1 && out[n - 1] == b'\\' {
        n - 1
    } else {
        n
    };
    (n > 1).then(|| core::str::from_utf8(&out[..n]).ok())?
}

/// The directory part of `path`: `\asterinas\BOOTAA64.EFI` → `\asterinas`, a file at the root → ``.
pub fn parent_dir(path: &str) -> &str {
    path.rfind(['\\', '/']).map_or("", |i| &path[..i])
}

/// A path from BOOT.CFG as the loader opens it: absolute ones (leading `\` or `/`) unchanged, relative ones under
/// `dir`, the directory BOOT.CFG was read from. None when the result does not fit [`MAX_PATH`].
pub fn resolve<'b>(dir: &str, path: &str, out: &'b mut [u8; MAX_PATH]) -> Option<&'b str> {
    let parts: [&str; 3] = if path.starts_with(['\\', '/']) {
        ["", "", path]
    } else {
        [dir, "\\", path]
    };
    let mut n = 0;
    for p in parts {
        if n + p.len() >= MAX_PATH {
            return None;
        }
        out[n..n + p.len()].copy_from_slice(p.as_bytes());
        n += p.len();
    }
    core::str::from_utf8(&out[..n]).ok()
}

// ---------------------------------------------------------------------------------------------------------------
// Load options
// ---------------------------------------------------------------------------------------------------------------

/// Kernel arguments from the loader's own UEFI load options (`EFI_LOADED_IMAGE_PROTOCOL.LoadOptions`), e.g.
/// `maxcpus=1` from GRUB's `chainloader /asterinas/BOOTAA64.EFI maxcpus=1`. The options are UCS-2 up to the first
/// NUL; a leading token naming an `.efi` file (the edk2 shell and some boot managers pass the whole command line)
/// is dropped. None when nothing is left, the options are not printable ASCII (firmware boot entries often carry
/// binary data there) or they do not fit `out`.
pub fn load_options_args<'b>(raw: &[u8], out: &'b mut [u8]) -> Option<&'b str> {
    let mut n = 0;
    for c in raw.as_chunks::<2>().0 {
        let b = match u16::from_le_bytes(*c) {
            0 => break,
            0x09 => b' ',
            u @ 0x20..0x7f => u as u8,
            _ => return None,
        };
        *out.get_mut(n)? = b;
        n += 1;
    }
    let text = core::str::from_utf8(&out[..n]).ok()?.trim();
    let first = text.split_ascii_whitespace().next()?;
    let rest = if first.len() > 4 && first[first.len() - 4..].eq_ignore_ascii_case(".efi") {
        text[first.len()..].trim()
    } else {
        text
    };
    (!rest.is_empty()).then_some(rest)
}

/// `base` (BOOT.CFG's `cmdline`) with `extra` added to the kernel's arguments: before the ` -- ` that starts the
/// init process's arguments, else at the end. None when the result does not fit `out`.
pub fn merge_cmdline<'b>(base: &str, extra: &str, out: &'b mut [u8]) -> Option<&'b str> {
    let (kernel, init) = if let Some(rest) = base.strip_prefix("-- ") {
        ("", Some(rest))
    } else if base == "--" {
        ("", Some(""))
    } else {
        match base.find(" -- ") {
            Some(i) => (&base[..i], Some(&base[i + 4..])),
            None if base.ends_with(" --") => (&base[..base.len() - 3], Some("")),
            None => (base, None),
        }
    };
    let mut n = 0;
    let mut push = |s: &str| -> Option<()> {
        out.get_mut(n..n + s.len())?.copy_from_slice(s.as_bytes());
        n += s.len();
        Some(())
    };
    for (i, part) in [kernel.trim(), extra.trim()]
        .into_iter()
        .filter(|p| !p.is_empty())
        .enumerate()
    {
        if i > 0 {
            push(" ")?;
        }
        push(part)?;
    }
    if let Some(init) = init {
        push(" --")?;
        if !init.is_empty() {
            push(" ")?;
            push(init)?;
        }
    }
    core::str::from_utf8(&out[..n]).ok().map(str::trim_start)
}

// ---------------------------------------------------------------------------------------------------------------
// Memory map
// ---------------------------------------------------------------------------------------------------------------

/// EFI_MEMORY_TYPE values (UEFI 2.10, 7.2.1).
pub mod mem_type {
    pub const RESERVED: u32 = 0;
    pub const LOADER_CODE: u32 = 1;
    pub const LOADER_DATA: u32 = 2;
    pub const BOOT_SERVICES_CODE: u32 = 3;
    pub const BOOT_SERVICES_DATA: u32 = 4;
    pub const RUNTIME_SERVICES_CODE: u32 = 5;
    pub const RUNTIME_SERVICES_DATA: u32 = 6;
    pub const CONVENTIONAL: u32 = 7;
    pub const UNUSABLE: u32 = 8;
    pub const ACPI_RECLAIM: u32 = 9;
    pub const ACPI_NVS: u32 = 10;
    pub const MMIO: u32 = 11;
    pub const MMIO_PORT_SPACE: u32 = 12;
    pub const PAL_CODE: u32 = 13;
    pub const PERSISTENT: u32 = 14;
    pub const UNACCEPTED: u32 = 15;
}

/// EFI_MEMORY_WB: the range can be mapped write-back cacheable.
pub const ATTR_WB: u64 = 0x8;
/// EFI_MEMORY_SP: specific-purpose memory, not for general use.
pub const ATTR_SP: u64 = 0x4_0000;

/// One EFI_MEMORY_DESCRIPTOR (the fields the loader reads).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemDesc {
    pub typ: u32,
    pub start: u64,
    pub pages: u64,
    pub attr: u64,
}

impl MemDesc {
    /// Parses one descriptor from the start of `b` (type u32, pad u32, physical start, virtual start, pages,
    /// attribute; firmware may append more, which the descriptor size skips).
    pub fn parse(b: &[u8]) -> Option<MemDesc> {
        let u64_at = |off: usize| -> Option<u64> {
            let mut v = [0u8; 8];
            v.copy_from_slice(b.get(off..off + 8)?);
            Some(u64::from_le_bytes(v))
        };
        let mut t = [0u8; 4];
        t.copy_from_slice(b.get(0..4)?);
        Some(MemDesc {
            typ: u32::from_le_bytes(t),
            start: u64_at(8)?,
            pages: u64_at(24)?,
            attr: u64_at(32)?,
        })
    }

    pub fn range(&self) -> Option<Range> {
        Range::new(self.start, self.pages.checked_mul(PAGE)?)
    }

    /// RAM the kernel may use after ExitBootServices: free memory, and what the loader and boot services held.
    /// Runtime services, ACPI tables, reserved and specific-purpose memory and anything that cannot be cached
    /// write-back stay out.
    pub fn usable(&self) -> bool {
        use mem_type::*;
        matches!(
            self.typ,
            LOADER_CODE | LOADER_DATA | BOOT_SERVICES_CODE | BOOT_SERVICES_DATA | CONVENTIONAL
        ) && self.attr & ATTR_WB != 0
            && self.attr & ATTR_SP == 0
    }

    /// Free memory the loader may allocate at a fixed address.
    pub fn free(&self) -> bool {
        self.typ == mem_type::CONVENTIONAL && self.usable()
    }
}

/// Short name of a memory type, for the log.
pub fn type_name(typ: u32) -> &'static str {
    use mem_type::*;
    match typ {
        RESERVED => "Reserved",
        LOADER_CODE => "LoaderCode",
        LOADER_DATA => "LoaderData",
        BOOT_SERVICES_CODE => "BSCode",
        BOOT_SERVICES_DATA => "BSData",
        RUNTIME_SERVICES_CODE => "RTCode",
        RUNTIME_SERVICES_DATA => "RTData",
        CONVENTIONAL => "Conventional",
        UNUSABLE => "Unusable",
        ACPI_RECLAIM => "ACPIReclaim",
        ACPI_NVS => "ACPINVS",
        MMIO => "MMIO",
        MMIO_PORT_SPACE => "MMIOPort",
        PAL_CODE => "PalCode",
        PERSISTENT => "Persistent",
        UNACCEPTED => "Unaccepted",
        _ => "Other",
    }
}

/// Too many ranges for the output, or a descriptor whose range overflows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeError {
    TooMany,
    Overflow,
}

/// The ranges of the descriptors `keep` accepts, sorted, with touching and overlapping ones merged, written to
/// `out`; returns how many. Fails rather than drop a range.
pub fn memory_ranges(
    descs: impl Iterator<Item = MemDesc>,
    keep: impl Fn(&MemDesc) -> bool,
    out: &mut [Range],
) -> Result<usize, RangeError> {
    let mut n = 0usize;
    for d in descs.filter(|d| keep(d) && d.pages != 0) {
        let r = d.range().ok_or(RangeError::Overflow)?;
        // Insertion sort by start; maps are short and nearly sorted.
        if n == out.len() {
            return Err(RangeError::TooMany);
        }
        let mut i = n;
        while i > 0 && out[i - 1].start > r.start {
            out[i] = out[i - 1];
            i -= 1;
        }
        out[i] = r;
        n += 1;
    }
    let mut merged = 0usize;
    for i in 0..n {
        let r = out[i];
        if merged > 0 && r.start <= out[merged - 1].end {
            out[merged - 1].end = out[merged - 1].end.max(r.end);
        } else {
            out[merged] = r;
            merged += 1;
        }
    }
    Ok(merged)
}

/// The physical window a kernel built for `dram_base` occupies: from its load address, whole 2 MiB blocks
/// covering `image_size`.
pub fn kernel_window(img: &Image, dram_base: u64) -> Option<Range> {
    let start = img.load_address(dram_base)?;
    let size = img.image_size.checked_add(BLOCK - 1)? & !(BLOCK - 1);
    let r = Range::new(start, size)?;
    (r.end <= KERNEL_LIMIT).then_some(r)
}

/// The lowest 2 MiB-aligned `OSTD_ARM_DRAM_BASE` at or above `from` whose kernel window lies inside one of the
/// sorted `free` ranges and below [`KERNEL_LIMIT`]: what to rebuild the kernel with when no candidate fits.
pub fn suggest_base(free: &[Range], img: &Image, from: u64) -> Option<u64> {
    let size = img.image_size.checked_add(BLOCK - 1)? & !(BLOCK - 1);
    for r in free {
        // Smallest block-aligned window start inside r whose base (start - text_offset) is block-aligned.
        let lo = r.start.max(from.checked_add(img.text_offset)?);
        let base = (lo.checked_sub(img.text_offset)?).checked_add(BLOCK - 1)? & !(BLOCK - 1);
        let start = base.checked_add(img.text_offset)?;
        let end = start.checked_add(size)?;
        if end <= r.end && end <= KERNEL_LIMIT {
            return Some(base);
        }
    }
    None
}

// ---------------------------------------------------------------------------------------------------------------
// Framebuffer
// ---------------------------------------------------------------------------------------------------------------

/// EFI_GRAPHICS_PIXEL_FORMAT.
pub mod pixel {
    /// Bytes R, G, B, X in memory.
    pub const RGBX: u32 = 0;
    /// Bytes B, G, R, X in memory.
    pub const BGRX: u32 = 1;
    pub const BITMASK: u32 = 2;
    pub const BLT_ONLY: u32 = 3;
}

/// The current GOP mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gop {
    pub width: u32,
    pub height: u32,
    pub pixels_per_line: u32,
    pub format: u32,
    /// Red, green, blue masks, for [`pixel::BITMASK`].
    pub masks: [u32; 3],
    pub base: u64,
    pub size: u64,
}

/// The `simple-framebuffer` node made from a GOP mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fb {
    pub base: u64,
    pub size: u64,
    pub width: u32,
    pub height: u32,
    /// Bytes per line: GOP's pixels per scan line, which may exceed the width.
    pub stride: u32,
    /// Always `x8r8g8b8`: the only 32 bpp layout the kernel draws.
    pub format: &'static str,
    /// The firmware's layout is R, G, B, X: red and blue appear swapped.
    pub swapped: bool,
}

/// Turns a GOP mode into a framebuffer the kernel accepts (patch 0005: x8r8g8b8; patch 0404: lines may be padded),
/// or `None` for a BLT-only or unknown layout.
pub fn framebuffer(g: &Gop) -> Option<Fb> {
    let swapped = match g.format {
        pixel::BGRX => false,
        pixel::RGBX => true,
        pixel::BITMASK if g.masks == [0x00ff_0000, 0x0000_ff00, 0x0000_00ff] => false,
        pixel::BITMASK if g.masks == [0x0000_00ff, 0x0000_ff00, 0x00ff_0000] => true,
        _ => return None,
    };
    if g.width == 0 || g.height == 0 || g.base == 0 || g.pixels_per_line < g.width {
        return None;
    }
    let stride = g.pixels_per_line.checked_mul(4)?;
    let need = u64::from(stride) * u64::from(g.height);
    if need > g.size || g.base.checked_add(need).is_none() {
        return None;
    }
    Some(Fb {
        base: g.base,
        size: need,
        width: g.width,
        height: g.height,
        stride,
        format: "x8r8g8b8",
        swapped,
    })
}

// ---------------------------------------------------------------------------------------------------------------
// Device tree edits
// ---------------------------------------------------------------------------------------------------------------

/// Most extra memory nodes: the first memory node gets the RAM, up to this many others get an empty `reg`, so no
/// reader counts them, whether it goes by `device_type` (the kernel) or by name (a tree with more is refused).
pub const MAX_STALE_MEMORY_NODES: usize = 3;
const PATH_MAX: usize = 96;

/// The values the edits point at; [`edits`] borrows them.
pub struct TreeValues {
    memory_path: [u8; PATH_MAX],
    memory_path_len: usize,
    stale_paths: [[u8; PATH_MAX]; MAX_STALE_MEMORY_NODES],
    stale_lens: [usize; MAX_STALE_MEMORY_NODES],
    stale: usize,
    reg: [u8; MAX_RANGES * 16],
    reg_len: usize,
    initrd: Option<([u8; 8], [u8; 8])>,
    bootargs: [u8; 4096],
    bootargs_len: usize,
    fb_path: [u8; PATH_MAX],
    fb_path_len: usize,
    fb_reg: [u8; 16],
    fb_reg_len: usize,
    fb_props: [[u8; 4]; 3],
    fb_format: &'static str,
    chosen_cells: Option<([u8; 4], [u8; 4])>,
    has_fb: bool,
    pstore_path: [u8; PATH_MAX],
    pstore_path_len: usize,
    pstore_reg: [u8; 16],
    pstore_reg_len: usize,
    rng: Option<RngSeed>,
    rng_note: Option<&'static [u8]>,
}

/// How much the loader asks the firmware for: Linux's `EFI_RANDOM_SEED_SIZE`.
pub const RNG_SEED_BYTES: usize = 32;
const RNG_SOURCE_MAX: usize = 48;

/// What goes to `/chosen/rng-seed`, and to `/chosen/lucina,rng-source` for the kernel's log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RngSeed {
    bytes: [u8; RNG_SEED_BYTES],
    source: [u8; RNG_SOURCE_MAX],
    source_len: usize,
}

impl RngSeed {
    /// `raw`: the firmware took `EFI_RNG_ALGORITHM_RAW`, not its default algorithm.
    pub fn new(bytes: [u8; RNG_SEED_BYTES], raw: bool, took_us: u64) -> Self {
        let mut source = [0u8; RNG_SOURCE_MAX];
        let mut w = NameWriter {
            out: &mut source,
            len: 0,
        };
        let algorithm = if raw { "raw" } else { "its default" };
        let _ = write!(w, "EFI_RNG_PROTOCOL {algorithm}, {took_us} us");
        let source_len = w.len;
        Self {
            bytes,
            source,
            source_len: (source_len + 1).min(RNG_SOURCE_MAX),
        }
    }

    /// Overwrites the seed, as far as a loader can: the kernel has a copy in the tree, and this memory becomes its RAM.
    pub fn wipe(&mut self) {
        self.bytes.fill(0);
        core::hint::black_box(&self.bytes);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeError {
    Fdt(lucina_fdt::Error),
    /// The root lacks `#address-cells`/`#size-cells` of 1 or 2, so RAM cannot be encoded.
    Cells,
    /// More than [`MAX_RANGES`] RAM ranges, or a range that does not fit the cells.
    Ranges,
    /// A node name too long for the path buffers.
    Path,
    /// The command line does not fit 4095 bytes.
    Cmdline,
    /// No `/chosen` to put the framebuffer under (the caller first adds one).
    NoChosen,
}

impl From<lucina_fdt::Error> for TreeError {
    fn from(e: lucina_fdt::Error) -> Self {
        TreeError::Fdt(e)
    }
}

impl core::fmt::Display for TreeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            TreeError::Fdt(e) => write!(f, "device tree: {e}"),
            TreeError::Cells => f.write_str("root #address-cells/#size-cells not 1 or 2"),
            TreeError::Ranges => f.write_str("RAM ranges do not fit /memory"),
            TreeError::Path => f.write_str("node name too long"),
            TreeError::Cmdline => f.write_str("command line longer than 4095 bytes"),
            TreeError::NoChosen => f.write_str("no /chosen node"),
        }
    }
}

fn put_cells(out: &mut [u8], at: &mut usize, v: u64, cells: u32) -> Result<(), TreeError> {
    match cells {
        1 => {
            let v = u32::try_from(v).map_err(|_| TreeError::Ranges)?;
            out.get_mut(*at..*at + 4)
                .ok_or(TreeError::Ranges)?
                .copy_from_slice(&v.to_be_bytes());
            *at += 4;
        }
        2 => {
            out.get_mut(*at..*at + 8)
                .ok_or(TreeError::Ranges)?
                .copy_from_slice(&v.to_be_bytes());
            *at += 8;
        }
        _ => return Err(TreeError::Cells),
    }
    Ok(())
}

fn put_path(buf: &mut [u8; PATH_MAX], parent: &str, name: &str) -> Result<usize, TreeError> {
    let sep = if parent.ends_with('/') { "" } else { "/" };
    let len = parent.len() + sep.len() + name.len();
    if len > PATH_MAX {
        return Err(TreeError::Path);
    }
    buf[..parent.len()].copy_from_slice(parent.as_bytes());
    buf[parent.len()..parent.len() + sep.len()].copy_from_slice(sep.as_bytes());
    buf[parent.len() + sep.len()..len].copy_from_slice(name.as_bytes());
    Ok(len)
}

/// A `framebuffer@<hex>` name.
fn fb_name(buf: &mut [u8; 32], addr: u64) -> &str {
    let prefix = b"framebuffer@";
    buf[..prefix.len()].copy_from_slice(prefix);
    let mut digits = [0u8; 16];
    let mut n = 0;
    let mut v = addr;
    loop {
        digits[n] = b"0123456789abcdef"[(v & 0xf) as usize];
        n += 1;
        v >>= 4;
        if v == 0 {
            break;
        }
    }
    for i in 0..n {
        buf[prefix.len() + i] = digits[n - 1 - i];
    }
    core::str::from_utf8(&buf[..prefix.len() + n]).unwrap_or("framebuffer")
}

fn as_str(b: &[u8]) -> &str {
    core::str::from_utf8(b).unwrap_or("/")
}

impl TreeValues {
    /// Computes every value from the source tree, the RAM ranges, the initramfs, the command line and the
    /// framebuffer.
    pub fn new(
        fdt: &Fdt<'_>,
        ram: &[Range],
        initrd: Option<Range>,
        cmdline: &str,
        fb: Option<&Fb>,
    ) -> Result<TreeValues, TreeError> {
        let root = fdt.root()?;
        let (ac, sc) = (root.address_cells()?, root.size_cells()?);
        if !matches!(ac, 1 | 2) || !matches!(sc, 1 | 2) {
            return Err(TreeError::Cells);
        }
        let mut v = TreeValues {
            memory_path: [0; PATH_MAX],
            memory_path_len: 0,
            stale_paths: [[0; PATH_MAX]; MAX_STALE_MEMORY_NODES],
            stale_lens: [0; MAX_STALE_MEMORY_NODES],
            stale: 0,
            reg: [0; MAX_RANGES * 16],
            reg_len: 0,
            initrd: initrd.map(|r| (r.start.to_be_bytes(), r.end.to_be_bytes())),
            bootargs: [0; 4096],
            bootargs_len: 0,
            fb_path: [0; PATH_MAX],
            fb_path_len: 0,
            fb_reg: [0; 16],
            fb_reg_len: 0,
            fb_props: [[0; 4]; 3],
            fb_format: "x8r8g8b8",
            chosen_cells: None,
            has_fb: false,
            pstore_path: [0; PATH_MAX],
            pstore_path_len: 0,
            pstore_reg: [0; 16],
            pstore_reg_len: 0,
            rng: None,
            rng_note: None,
        };

        // Memory nodes: by device_type, as the kernel finds them (patch 0012).
        for child in root.children() {
            let child = child?;
            let is_memory = child
                .property("device_type")?
                .is_some_and(|p| p.as_str() == Ok("memory"));
            if !is_memory {
                continue;
            }
            if v.memory_path_len == 0 {
                v.memory_path_len = put_path(&mut v.memory_path, "/", child.name())?;
            } else if v.stale < MAX_STALE_MEMORY_NODES {
                v.stale_lens[v.stale] = put_path(&mut v.stale_paths[v.stale], "/", child.name())?;
                v.stale += 1;
            } else {
                return Err(TreeError::Ranges);
            }
        }
        if v.memory_path_len == 0 {
            v.memory_path_len = put_path(&mut v.memory_path, "/", "memory")?;
        }

        if ram.len() > MAX_RANGES {
            return Err(TreeError::Ranges);
        }
        let mut at = 0;
        for r in ram {
            put_cells(&mut v.reg, &mut at, r.start, ac)?;
            put_cells(&mut v.reg, &mut at, r.size(), sc)?;
        }
        v.reg_len = at;

        if cmdline.len() >= v.bootargs.len() || cmdline.contains('\0') {
            return Err(TreeError::Cmdline);
        }
        v.bootargs[..cmdline.len()].copy_from_slice(cmdline.as_bytes());
        v.bootargs_len = cmdline.len() + 1;

        if let Some(fb) = fb {
            let chosen = fdt.node("/chosen")?.ok_or(TreeError::NoChosen)?;
            // A /chosen without its own cells gets the root's, as `lucina-fdt add-framebuffer` does.
            let (fac, fsc) = if chosen.property("#address-cells")?.is_none()
                && chosen.property("#size-cells")?.is_none()
            {
                v.chosen_cells = Some((ac.to_be_bytes(), sc.to_be_bytes()));
                (ac, sc)
            } else {
                (chosen.address_cells()?, chosen.size_cells()?)
            };
            let mut at = 0;
            put_cells(&mut v.fb_reg, &mut at, fb.base, fac)?;
            put_cells(&mut v.fb_reg, &mut at, fb.size, fsc)?;
            v.fb_reg_len = at;
            let mut name = [0u8; 32];
            v.fb_path_len = put_path(&mut v.fb_path, "/chosen", fb_name(&mut name, fb.base))?;
            v.fb_props = [
                fb.width.to_be_bytes(),
                fb.height.to_be_bytes(),
                fb.stride.to_be_bytes(),
            ];
            v.fb_format = fb.format;
            v.has_fb = true;
        }
        Ok(v)
    }
}

impl TreeValues {
    /// Adds the saved log's region as a node under `/reserved-memory`, which must exist: `no-map`, so that the kernel keeps
    /// it out of its RAM and maps it itself. `compatible` is what the kernel looks for.
    /// The firmware's seed goes along.
    pub fn set_rng_seed(&mut self, seed: &RngSeed) {
        self.rng = Some(*seed);
    }

    /// Why there is no seed from the firmware, for the kernel's log. NUL-terminated.
    pub fn set_rng_note(&mut self, note: &'static [u8]) {
        self.rng_note = Some(note);
    }

    /// Overwrites this copy of the seed, once the tree is written.
    pub fn wipe_rng_seed(&mut self) {
        if let Some(seed) = &mut self.rng {
            seed.wipe();
        }
    }

    pub fn set_pstore(&mut self, fdt: &Fdt<'_>, region: Range) -> Result<(), TreeError> {
        let node = fdt.node(RESERVED_MEMORY)?.ok_or(TreeError::NoChosen)?;
        let (ac, sc) = (node.address_cells()?, node.size_cells()?);
        let mut at = 0;
        put_cells(&mut self.pstore_reg, &mut at, region.start, ac)?;
        put_cells(&mut self.pstore_reg, &mut at, region.size(), sc)?;
        self.pstore_reg_len = at;
        let mut name = [0u8; 32];
        let len = {
            let mut w = NameWriter {
                out: &mut name,
                len: 0,
            };
            let _ = write!(w, "lucina-pstore@{:x}", region.start);
            w.len
        };
        let name = core::str::from_utf8(&name[..len]).map_err(|_| TreeError::Ranges)?;
        self.pstore_path_len = put_path(&mut self.pstore_path, RESERVED_MEMORY, name)?;
        Ok(())
    }
}

struct NameWriter<'a> {
    out: &'a mut [u8],
    len: usize,
}

impl core::fmt::Write for NameWriter<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let end = self.len + s.len();
        self.out
            .get_mut(self.len..end)
            .ok_or(core::fmt::Error)?
            .copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}

/// The edits that make a `/reserved-memory` node for a tree without one, as `lucina-fdt add-reserved` does: the root's
/// cells would also do, but this node's `reg` must not depend on them.
pub const RESERVED_MEMORY_EDITS: [Edit<'static>; 3] = [
    Edit::Set {
        node: RESERVED_MEMORY,
        name: "#address-cells",
        value: &[0, 0, 0, 2],
    },
    Edit::Set {
        node: RESERVED_MEMORY,
        name: "#size-cells",
        value: &[0, 0, 0, 2],
    },
    Edit::Set {
        node: RESERVED_MEMORY,
        name: "ranges",
        value: &[],
    },
];

/// Every edit, in a fixed array; returns it and how many are used. At most 2 (memory) + 3 (stale memory nodes) +
/// 1 (bootargs) + 2 (initrd) + 2 (`/chosen` cells) + 6 (framebuffer) + 3 (random seed) + 3 (saved log), below [`lucina_fdt::MAX_EDITS`].
pub fn edits<'a>(v: &'a TreeValues) -> ([Edit<'a>; lucina_fdt::MAX_EDITS], usize) {
    let mut e = [Edit::Remove {
        node: "/",
        name: "-",
    }; lucina_fdt::MAX_EDITS];
    let mut n = 0;
    let mut add = |edit: Edit<'a>| {
        e[n] = edit;
        n += 1;
    };
    let memory = as_str(&v.memory_path[..v.memory_path_len]);
    add(Edit::Set {
        node: memory,
        name: "device_type",
        value: b"memory\0",
    });
    add(Edit::Set {
        node: memory,
        name: "reg",
        value: &v.reg[..v.reg_len],
    });
    for i in 0..v.stale {
        add(Edit::Set {
            node: as_str(&v.stale_paths[i][..v.stale_lens[i]]),
            name: "reg",
            value: &[],
        });
    }
    add(Edit::Set {
        node: "/chosen",
        name: "bootargs",
        value: &v.bootargs[..v.bootargs_len],
    });
    for (name, value) in [("linux,initrd-start", 0), ("linux,initrd-end", 1)] {
        add(match &v.initrd {
            Some(pair) => Edit::Set {
                node: "/chosen",
                name,
                value: if value == 0 { &pair.0 } else { &pair.1 },
            },
            None => Edit::Remove {
                node: "/chosen",
                name,
            },
        });
    }
    if v.has_fb {
        if let Some((ac, sc)) = &v.chosen_cells {
            add(Edit::Set {
                node: "/chosen",
                name: "#address-cells",
                value: ac,
            });
            add(Edit::Set {
                node: "/chosen",
                name: "#size-cells",
                value: sc,
            });
        }
        let fb = as_str(&v.fb_path[..v.fb_path_len]);
        let format: &[u8] = match v.fb_format {
            "x8r8g8b8" => b"x8r8g8b8\0",
            _ => b"a8r8g8b8\0",
        };
        for (name, value) in [
            ("compatible", &b"simple-framebuffer\0"[..]),
            ("reg", &v.fb_reg[..v.fb_reg_len]),
            ("width", &v.fb_props[0][..]),
            ("height", &v.fb_props[1][..]),
            ("stride", &v.fb_props[2][..]),
            ("format", format),
        ] {
            add(Edit::Set {
                node: fb,
                name,
                value,
            });
        }
    }
    if let Some(seed) = &v.rng {
        add(Edit::Set {
            node: "/chosen",
            name: "rng-seed",
            value: &seed.bytes,
        });
        add(Edit::Set {
            node: "/chosen",
            name: "lucina,rng-source",
            value: &seed.source[..seed.source_len],
        });
    }
    if let Some(note) = v.rng_note {
        add(Edit::Set {
            node: "/chosen",
            name: "lucina,rng-note",
            value: note,
        });
    }
    if v.pstore_path_len != 0 {
        let node = as_str(&v.pstore_path[..v.pstore_path_len]);
        add(Edit::Set {
            node,
            name: "compatible",
            value: b"lucina,pstore\0",
        });
        add(Edit::Set {
            node,
            name: "reg",
            value: &v.pstore_reg[..v.pstore_reg_len],
        });
        add(Edit::Set {
            node,
            name: "no-map",
            value: &[],
        });
    }
    (e, n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lucina_fdt::{Tree, patch};

    const VIRT: &[u8] = include_bytes!("../../../kernel/lucina-fdt/tests/data/qemu-virt-gicv3-8g.dtb");

    /// What an installer writes to commit a new set in one 4096-byte block: comment padding of any length, in a line
    /// of its own, and short file names next to BOOT.CFG. NUL bytes as padding are not a comment.
    #[test]
    fn config_padded_to_a_block_parses() {
        let mut text =
            String::from("kernel = K12.IMG 0xa0000000\ninitrd = I12.GZ\ncmdline = quiet # kept\n");
        text.push('#');
        text.push_str(&" ".repeat(4096 - text.len() - 1));
        text.push('\n');
        assert_eq!(text.len(), 4096);
        let c = Config::parse(&text).unwrap();
        assert_eq!(c.kernels[0].unwrap().path, "K12.IMG");
        assert_eq!(c.initrd, Some("I12.GZ"));
        assert_eq!(c.cmdline, "quiet # kept");
        let mut nul = String::from("kernel = K12.IMG 0xa0000000\n");
        nul.push_str(&"\0".repeat(100));
        assert!(Config::parse(&nul).is_err());
    }
    #[test]
    fn config_parses_and_rejects() {
        let text = "# LucinaUEFI\r\n\
                    kernel = \\LUCINA\\KERNEL-A0.IMG 0xa0000000\r\n\
                    kernel=\\LUCINA\\KERNEL-B0.IMG 0xb000_0000 # second\n\
                    initrd = \\LUCINA\\INITRAMFS.GZ\n\
                    cmdline = console=tty0 x=#1 -- sh -l\n\
                    pause = 10\nverbose = 1\nlog = \\LUCINA\\LOG.TXT\n";
        let c = Config::parse(text).unwrap();
        let k: Vec<_> = c.kernels().collect();
        assert_eq!(
            k,
            [
                KernelChoice {
                    path: "\\LUCINA\\KERNEL-A0.IMG",
                    dram_base: 0xa000_0000
                },
                KernelChoice {
                    path: "\\LUCINA\\KERNEL-B0.IMG",
                    dram_base: 0xb000_0000
                }
            ]
        );
        assert_eq!(c.initrd, Some("\\LUCINA\\INITRAMFS.GZ"));
        assert_eq!(c.dtb, None);
        assert_eq!(c.cmdline, "console=tty0 x=#1 -- sh -l");
        assert_eq!((c.pause_s, c.verbose), (10, true));
        assert!(!c.quiet);
        assert!(Config::parse("kernel = a 0x0\nquiet = 1\n").unwrap().quiet);
        assert!(Config::parse("kernel = a 0x0\n").unwrap().rng);
        assert!(!Config::parse("kernel = a 0x0\nrng = off\n").unwrap().rng);
        assert!(Config::parse("kernel = a 0x0\nrng = maybe\n").is_err());
        assert!(Config::parse("kernel = a 0x0\nrng = on\nrng = on\n").is_err());
        assert!(!Config::parse("kernel = a 0x0\nquiet = 0\n").unwrap().quiet);
        assert!(Config::parse("kernel = a 0x0\nquiet = yes\n").is_err());
        assert!(Config::parse("kernel = a 0x0\nquiet = 1\nquiet = 1\n").is_err());
        assert_eq!(c.log, Some("\\LUCINA\\LOG.TXT"));

        assert_eq!(Config::parse("initrd = a\n"), Err(ConfigError::NoKernel));
        assert_eq!(Config::parse("kernel a 0x0"), Err(ConfigError::NoEquals(1)));
        assert_eq!(Config::parse("kernel = a\n"), Err(ConfigError::BadValue(1)));
        assert_eq!(
            Config::parse("kernel = a 0x1000\n"),
            Err(ConfigError::BadValue(1)),
            "a base that is not 2 MiB-aligned"
        );
        assert_eq!(
            Config::parse("kernel = a 0x0\nfoo = 1\n"),
            Err(ConfigError::UnknownKey(2))
        );
        assert_eq!(
            Config::parse("kernel = a 0x0\ndtb = x\ndtb = y\n"),
            Err(ConfigError::Duplicate(3))
        );
        assert_eq!(
            Config::parse("kernel = a 0x0\npause = 9999\n"),
            Err(ConfigError::BadValue(2))
        );
        let five = "kernel = a 0x0\n".repeat(5);
        assert_eq!(Config::parse(&five), Err(ConfigError::TooManyKernels(5)));
        assert_eq!(parse_hex("0x_"), Some(0));
        assert_eq!(parse_hex("0x"), None);
        assert_eq!(parse_hex("0x1_0000_0000_0000_0000"), None);
    }

    fn d(typ: u32, start: u64, pages: u64) -> MemDesc {
        MemDesc {
            typ,
            start,
            pages,
            attr: ATTR_WB | 0x1,
        }
    }

    #[test]
    fn memory_map_becomes_ram() {
        use mem_type::*;
        let map = [
            d(CONVENTIONAL, 0x4800_0000, 0x100),
            d(LOADER_DATA, 0x4000_0000, 0x10),
            d(BOOT_SERVICES_DATA, 0x4001_0000, 0x10),
            d(RUNTIME_SERVICES_DATA, 0x4002_0000, 0x10),
            d(CONVENTIONAL, 0x4003_0000, 0x10),
            d(ACPI_RECLAIM, 0x4004_0000, 1),
            d(MMIO, 0x0900_0000, 1),
            MemDesc {
                attr: 0x1,
                ..d(CONVENTIONAL, 0x5000_0000, 1)
            },
            MemDesc {
                attr: ATTR_WB | ATTR_SP,
                ..d(CONVENTIONAL, 0x5100_0000, 1)
            },
            d(CONVENTIONAL, 0x4004_1000, 0),
        ];
        let mut out = [Range::default(); 8];
        let n = memory_ranges(map.iter().copied(), MemDesc::usable, &mut out).unwrap();
        assert_eq!(
            &out[..n],
            &[
                Range {
                    start: 0x4000_0000,
                    end: 0x4002_0000
                },
                Range {
                    start: 0x4003_0000,
                    end: 0x4004_0000
                },
                Range {
                    start: 0x4800_0000,
                    end: 0x4810_0000
                },
            ]
        );
        let n = memory_ranges(map.iter().copied(), MemDesc::free, &mut out).unwrap();
        assert_eq!(n, 2);
        let mut small = [Range::default(); 2];
        assert_eq!(
            memory_ranges(map.iter().copied(), MemDesc::usable, &mut small),
            Err(RangeError::TooMany)
        );
        let huge = [d(CONVENTIONAL, u64::MAX - 0xfff, 2)];
        assert_eq!(
            memory_ranges(huge.iter().copied(), MemDesc::usable, &mut out),
            Err(RangeError::Overflow)
        );
        // Descriptor bytes as the firmware lays them out (48-byte stride on edk2).
        let mut raw = [0u8; 48];
        raw[0..4].copy_from_slice(&7u32.to_le_bytes());
        raw[8..16].copy_from_slice(&0x4000_0000u64.to_le_bytes());
        raw[24..32].copy_from_slice(&3u64.to_le_bytes());
        raw[32..40].copy_from_slice(&0xfu64.to_le_bytes());
        assert_eq!(
            MemDesc::parse(&raw),
            Some(MemDesc {
                typ: 7,
                start: 0x4000_0000,
                pages: 3,
                attr: 0xf
            })
        );
        assert_eq!(MemDesc::parse(&raw[..39]), None);
    }

    fn image(image_size: u64) -> Image {
        Image {
            text_offset: 0x20_0000,
            image_size,
            flags: 0b10,
            file_size: image_size,
        }
    }

    #[test]
    fn kernel_window_and_suggestion() {
        let img = image(0x51_0000);
        assert_eq!(
            kernel_window(&img, 0xa000_0000),
            Some(Range {
                start: 0xa020_0000,
                end: 0xa080_0000
            })
        );
        assert_eq!(
            kernel_window(&img, 0xbfe0_0000),
            None,
            "must end below 3 GiB"
        );
        let free = [
            Range {
                start: 0x8000_0000,
                end: 0x8050_0000,
            },
            Range {
                start: 0x9c12_3000,
                end: 0x9d00_0000,
            },
            Range {
                start: 0xa000_0000,
                end: 0x1_0000_0000,
            },
        ];
        // From 0x9c123000 the base rounds up to 0x9c000000 (window 0x9c200000-0x9c800000): fits the second range.
        assert_eq!(suggest_base(&free, &img, 0x8000_0000), Some(0x9c00_0000));
        // The window may start right at 0xa0000000: base 0x9fe00000.
        assert_eq!(suggest_base(&free, &img, 0x9d00_0000), Some(0x9fe0_0000));
        assert_eq!(suggest_base(&free, &img, 0xc000_0000), None);
    }

    #[test]
    fn gop_becomes_framebuffer() {
        let g = Gop {
            width: 1366,
            height: 768,
            pixels_per_line: 1376,
            format: pixel::BGRX,
            masks: [0; 3],
            base: 0x9c00_0000,
            size: 0x50_0000,
        };
        let fb = framebuffer(&g).unwrap();
        assert_eq!(
            (fb.width, fb.stride, fb.size, fb.swapped),
            (1366, 5504, 5504 * 768, false)
        );
        assert!(
            framebuffer(&Gop {
                format: pixel::RGBX,
                ..g
            })
            .unwrap()
            .swapped
        );
        assert!(
            framebuffer(&Gop {
                format: pixel::BLT_ONLY,
                ..g
            })
            .is_none()
        );
        assert!(
            framebuffer(&Gop {
                format: pixel::BITMASK,
                masks: [0xff0000, 0xff00, 0xff],
                ..g
            })
            .is_some()
        );
        assert!(framebuffer(&Gop { size: 0x1000, ..g }).is_none());
        assert!(framebuffer(&Gop { base: 0, ..g }).is_none());
    }

    #[test]
    fn config_turns_the_saved_log_off() {
        assert!(Config::parse("kernel = a 0x0\n").unwrap().pstore);
        assert!(
            !Config::parse("kernel = a 0x0\npstore = off\n")
                .unwrap()
                .pstore
        );
        assert!(
            Config::parse("kernel = a 0x0\npstore = on\n")
                .unwrap()
                .pstore
        );
        assert_eq!(
            Config::parse("kernel = a 0x0\npstore = maybe\n"),
            Err(ConfigError::BadValue(2))
        );
    }

    #[test]
    fn saved_log_region_becomes_a_no_map_node() {
        // QEMU's tree has no /reserved-memory: the loader adds it first.
        let fdt = Fdt::new(VIRT).unwrap();
        assert!(fdt.node(RESERVED_MEMORY).unwrap().is_none());
        let mut with_node = vec![0u8; VIRT.len() + 4096];
        let len = patch(&fdt, &RESERVED_MEMORY_EDITS, &mut with_node).unwrap();
        let fdt = Fdt::new(&with_node[..len]).unwrap();

        let region = Range {
            start: PSTORE_BASES[0],
            end: PSTORE_BASES[0] + PSTORE_SIZE,
        };
        let ram = [Range {
            start: 0x4000_0000,
            end: 0xc000_0000,
        }];
        let mut v = TreeValues::new(&fdt, &ram, None, "x", None).unwrap();
        v.set_pstore(&fdt, region).unwrap();
        let (e, n) = edits(&v);
        let mut out = vec![0u8; len + 4096];
        let new_len = patch(&fdt, &e[..n], &mut out).unwrap();
        let new = Fdt::new(&out[..new_len]).unwrap();

        let node = new
            .node("/reserved-memory/lucina-pstore@bd000000")
            .unwrap()
            .unwrap();
        assert_eq!(
            node.property("compatible").unwrap().unwrap().value,
            b"lucina,pstore\0"
        );
        assert!(node.property("no-map").unwrap().is_some());
        assert_eq!(
            node.property("reg").unwrap().unwrap().value,
            [0, 0, 0, 0, 0xbd, 0, 0, 0, 0, 0, 0, 0, 0, 0x20, 0, 0]
        );
    }

    #[test]
    fn edits_write_memory_chosen_and_framebuffer() {
        // QEMU's tree plus a second memory node and a stale initrd: the edits must leave one RAM node.
        let mut t = Tree::parse(VIRT).unwrap();
        let extra = {
            let mut n = lucina_fdt::NodeBuf::new("memory@200000000");
            n.set_prop("device_type", lucina_fdt::string("memory"));
            n.set_prop("reg", lucina_fdt::cells32(&[2, 0, 0, 0x1000_0000]));
            n
        };
        t.root.children.push(extra);
        let src = t.to_bytes().unwrap();
        let fdt = Fdt::new(&src).unwrap();
        let ram = [
            Range {
                start: 0x4000_0000,
                end: 0x4800_0000,
            },
            Range {
                start: 0x5000_0000,
                end: 0x1_0000_0000,
            },
        ];
        let fb = framebuffer(&Gop {
            width: 800,
            height: 600,
            pixels_per_line: 800,
            format: pixel::BGRX,
            masks: [0; 3],
            base: 0x7f00_0000,
            size: 800 * 600 * 4,
        })
        .unwrap();
        let initrd = Range {
            start: 0x4600_0000,
            end: 0x4610_0000,
        };
        let v = TreeValues::new(&fdt, &ram, Some(initrd), "console=hvc0 -- sh", Some(&fb)).unwrap();
        let (e, n) = edits(&v);
        assert!(n <= lucina_fdt::MAX_EDITS);
        let mut out = vec![0u8; src.len() + 4096];
        let len = patch(&fdt, &e[..n], &mut out).unwrap();
        let new = Fdt::new(&out[..len]).unwrap();

        let mem: Vec<_> = new
            .memory()
            .unwrap()
            .map(|r| r.unwrap())
            .map(|r| (r.base, r.size))
            .collect();
        assert_eq!(mem, [(0x4000_0000, 0x800_0000), (0x5000_0000, 0xb000_0000)]);
        let stale = new.node("/memory@200000000").unwrap().unwrap();
        assert!(stale.property("reg").unwrap().unwrap().value.is_empty());
        let chosen = new.chosen().unwrap().unwrap();
        assert_eq!(chosen.bootargs, Some("console=hvc0 -- sh"));
        assert_eq!(chosen.initrd, Some((0x4600_0000, 0x4610_0000)));
        let f = chosen.framebuffer.unwrap();
        assert_eq!(
            (f.base, f.size, f.width, f.height, f.stride, f.format),
            (0x7f00_0000, 800 * 600 * 4, 800, 600, 3200, "x8r8g8b8")
        );
        assert_eq!(f.name, "framebuffer@7f000000");

        // Without an initrd the old properties go; without a framebuffer nothing is added under /chosen.
        let src2 = out[..len].to_vec();
        let fdt2 = Fdt::new(&src2).unwrap();
        let v = TreeValues::new(&fdt2, &ram, None, "", None).unwrap();
        let (e, n) = edits(&v);
        let len = patch(&fdt2, &e[..n], &mut out).unwrap();
        let chosen = Fdt::new(&out[..len]).unwrap().chosen().unwrap().unwrap();
        assert_eq!(chosen.initrd, None);
        assert_eq!(chosen.bootargs, Some(""));
    }

    #[test]
    fn edits_create_memory_node_when_missing() {
        let mut t = Tree::parse(VIRT).unwrap();
        t.root.children.retain(|c| !c.name.starts_with("memory"));
        let src = t.to_bytes().unwrap();
        let fdt = Fdt::new(&src).unwrap();
        let ram = [Range {
            start: 0x4000_0000,
            end: 0x5000_0000,
        }];
        let v = TreeValues::new(&fdt, &ram, None, "x", None).unwrap();
        let (e, n) = edits(&v);
        let mut out = vec![0u8; src.len() + 4096];
        let len = patch(&fdt, &e[..n], &mut out).unwrap();
        let new = Fdt::new(&out[..len]).unwrap();
        let mem: Vec<_> = new.memory().unwrap().map(|r| r.unwrap().base).collect();
        assert_eq!(mem, [0x4000_0000]);
    }

    fn file_node(path: &str) -> Vec<u8> {
        let mut v = vec![4, 4, 0, 0];
        for u in path.encode_utf16().chain([0]) {
            v.extend(u.to_le_bytes());
        }
        let len = v.len() as u16;
        v[2..4].copy_from_slice(&len.to_le_bytes());
        v
    }

    #[test]
    fn device_path_file_joins_nodes() {
        const END: [u8; 4] = [0x7f, 0xff, 4, 0];
        let mut buf = [0u8; MAX_PATH];
        let one = [file_node("\\asterinas\\BOOTAA64.EFI"), END.to_vec()].concat();
        assert_eq!(
            device_path_file(&one, &mut buf),
            Some("\\asterinas\\BOOTAA64.EFI")
        );
        // One node per directory, a non-file node in front, GRUB-style slashes.
        let split = [
            vec![1, 4, 8, 0, 0, 0, 0, 0],
            file_node("\\EFI\\"),
            file_node("BOOT"),
            file_node("/asterinas//x.efi"),
            END.to_vec(),
        ]
        .concat();
        assert_eq!(
            device_path_file(&split, &mut buf),
            Some("\\EFI\\BOOT\\asterinas\\x.efi")
        );
        assert_eq!(device_path_file(&END, &mut buf), None);
        assert_eq!(
            device_path_file(&file_node("\\a.efi"), &mut buf),
            None,
            "no end node"
        );
        let bad = [file_node("\\é.efi"), END.to_vec()].concat();
        assert_eq!(device_path_file(&bad, &mut buf), None);
        assert_eq!(device_path_file(&[4, 4, 2, 0], &mut buf), None);
    }

    #[test]
    fn board_key() {
        let base = "kernel = K.IMG 0xa0000000\n";
        assert!(!Config::parse(base).unwrap().board);
        assert!(
            Config::parse(&format!("{base}board = auto\n"))
                .unwrap()
                .board
        );
        assert!(Config::parse(&format!("{base}board = galaxy\n")).is_err());
        assert!(Config::parse(&format!("{base}board = auto\nboard = auto\n")).is_err());
    }

    #[test]
    fn paths_resolve_next_to_the_config() {
        let mut buf = [0u8; MAX_PATH];
        assert_eq!(parent_dir("\\asterinas\\BOOTAA64.EFI"), "\\asterinas");
        assert_eq!(parent_dir("\\BOOT.CFG"), "");
        assert_eq!(
            resolve("\\asterinas", "KERNEL-A0.IMG", &mut buf),
            Some("\\asterinas\\KERNEL-A0.IMG")
        );
        assert_eq!(resolve("", "BOOT.CFG", &mut buf), Some("\\BOOT.CFG"));
        assert_eq!(
            resolve("\\asterinas", "\\LUCINA\\GBGO.DTB", &mut buf),
            Some("\\LUCINA\\GBGO.DTB")
        );
        assert_eq!(resolve("\\x", "/y/z", &mut buf), Some("/y/z"));
        assert_eq!(resolve(&"d".repeat(250), "0123456789", &mut buf), None);
    }

    fn ucs2(s: &str) -> Vec<u8> {
        s.encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect()
    }

    #[test]
    fn load_options_become_kernel_args() {
        let mut buf = [0u8; 256];
        assert_eq!(
            load_options_args(&ucs2("maxcpus=1"), &mut buf),
            Some("maxcpus=1")
        );
        assert_eq!(
            load_options_args(
                &ucs2("fs0:\\asterinas\\BOOTAA64.EFI  nosmp\tx=1 "),
                &mut buf
            ),
            Some("nosmp x=1")
        );
        assert_eq!(
            load_options_args(&ucs2("\\x\\BOOTAA64.efi"), &mut buf),
            None
        );
        assert_eq!(load_options_args(&ucs2("   "), &mut buf), None);
        assert_eq!(load_options_args(&[], &mut buf), None);
        assert_eq!(
            load_options_args(&[0x01, 0x00, 0x41, 0x00], &mut buf),
            None,
            "binary"
        );
        assert_eq!(load_options_args(&ucs2("é"), &mut buf), None);
        assert_eq!(load_options_args(&ucs2(&"a".repeat(300)), &mut buf), None);
    }

    #[test]
    fn the_firmware_seed_goes_to_chosen_and_can_be_wiped() {
        let src = Tree::parse(VIRT).unwrap().to_bytes().unwrap();
        let fdt = Fdt::new(&src).unwrap();
        let ram = [Range {
            start: 0x4000_0000,
            end: 0x5000_0000,
        }];
        let mut v = TreeValues::new(&fdt, &ram, None, "x", None).unwrap();
        let mut seed = RngSeed::new([7; RNG_SEED_BYTES], true, 123);
        v.set_rng_seed(&seed);
        let (e, n) = edits(&v);
        let mut out = vec![0u8; src.len() + 4096];
        let len = patch(&fdt, &e[..n], &mut out).unwrap();
        let new = Fdt::new(&out[..len]).unwrap();
        let value = new.property("/chosen", "rng-seed").unwrap().unwrap();
        assert_eq!(value.value, &[7; RNG_SEED_BYTES]);
        let source = new
            .property("/chosen", "lucina,rng-source")
            .unwrap()
            .unwrap();
        assert_eq!(source.as_str().unwrap(), "EFI_RNG_PROTOCOL raw, 123 us");

        v.wipe_rng_seed();
        seed.wipe();
        assert_eq!(seed.bytes, [0; RNG_SEED_BYTES]);
        assert_eq!(v.rng.unwrap().bytes, [0; RNG_SEED_BYTES]);

        // Without a seed nothing is added.
        let v = TreeValues::new(&fdt, &ram, None, "x", None).unwrap();
        let (e, n) = edits(&v);
        let len = patch(&fdt, &e[..n], &mut out).unwrap();
        let new = Fdt::new(&out[..len]).unwrap();
        assert!(
            new.property("/chosen", "lucina,rng-source")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn extra_args_go_before_the_init_args() {
        let mut buf = [0u8; 256];
        let m = |base: &str, extra: &str, buf: &mut [u8; 256]| {
            merge_cmdline(base, extra, buf).map(String::from)
        };
        assert_eq!(
            m("console=tty0 loglevel=info -- sh -l", "maxcpus=1", &mut buf).as_deref(),
            Some("console=tty0 loglevel=info maxcpus=1 -- sh -l")
        );
        assert_eq!(
            m("console=tty0", "maxcpus=1", &mut buf).as_deref(),
            Some("console=tty0 maxcpus=1")
        );
        assert_eq!(
            m("-- sh -l", "nosmp", &mut buf).as_deref(),
            Some("nosmp -- sh -l")
        );
        assert_eq!(m("a --", "b", &mut buf).as_deref(), Some("a b --"));
        assert_eq!(m("", "b", &mut buf).as_deref(), Some("b"));
        assert_eq!(
            m("a -- b -- c", "x", &mut buf).as_deref(),
            Some("a x -- b -- c")
        );
        let mut small = [0u8; 8];
        assert_eq!(merge_cmdline("console=tty0", "maxcpus=1", &mut small), None);
    }
}
