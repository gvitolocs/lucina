//! LucinaUEFI: a UEFI application (`EFI/BOOT/BOOTAA64.EFI`) that boots the LucinaOS kernel on AArch64 laptops
//! whose firmware is UEFI, first the Samsung Galaxy Book Go (Snapdragon 7c Gen 2, SC7180).
//!
//! 1. Read `BOOT.CFG` next to its own EFI file, else `\LUCINA\BOOT.CFG` or `\asterinas\BOOT.CFG`, on the volume
//!    it was started from, else on any FAT volume ([`lucina_boot::uefi::Config`]); relative paths in it are relative
//!    to BOOT.CFG's directory. So it runs as `EFI/BOOT/BOOTAA64.EFI` on its own stick, chainloaded from another boot
//!    manager's folder, and started from memory with no file path (LucinaLoader).
//! 2. Load the first kernel candidate whose window (`OSTD_ARM_DRAM_BASE + text_offset`, whole 2 MiB blocks) is
//!    free RAM; Asterinas cannot relocate itself.
//! 3. Load the initramfs and the device tree (a file, or the firmware's DTB configuration table).
//! 4. Describe RAM from the UEFI memory map in `/memory` (firmware carve-outs left out), the GOP mode as a
//!    `/chosen` `simple-framebuffer`, and `/chosen` `bootargs`/`linux,initrd-*` (`lucina_fdt::patch`).
//! 5. Print what it did and the exception level, write it to a log file on the volume, ExitBootServices, paint
//!    the status band, turn the MMU off (dropping from EL2 to EL1 if needed) and jump.
//!
//! The decisions are in `kernel/lucina-boot` (`uefi`, `image`, `band`) and `kernel/lucina-fdt`, `no_std`,
//! `unsafe`-free and tested on the host. `unsafe` is confined to `src/efi.rs` (firmware calls), `src/arch.rs`
//! (system registers, caches, the jump) and `src/screen.rs` (framebuffer pixels).

#![no_std]
#![no_main]

mod arch;
mod efi;
mod screen;

use core::{fmt, fmt::Write, panic::PanicInfo};

use lucina_boot::{
    rng_marker,
    trial,
    Range,
    band::Status as BandStatus,
    board,
    image::{self, Image},
    uefi::{
        self, Config, ConfigError, Fb, MAX_PATH, MAX_RANGES, MemDesc, PSTORE_BASES, PSTORE_SIZE,
        RESERVED_MEMORY, RESERVED_MEMORY_EDITS, RangeError, TreeError, TreeValues, edits,
        kernel_window, memory_ranges, suggest_base, type_name, RngSeed, RNG_SEED_BYTES,
    },
};
use lucina_fdt::{Edit, Fdt, patch};

use efi::{ACPI20_TABLE, DTB_TABLE, Efi, Place, SMBIOS3_TABLE, Status, descriptors, status_name};
use screen::Screen;

const VERSION: &str = env!("CARGO_PKG_VERSION");
/// BOOT.CFG's name next to the loader's own EFI file.
const CONFIG_NAME: &str = "BOOT.CFG";
/// Where BOOT.CFG is looked for when it is not next to the loader: this loader's own stick, then the folder on
/// the LucinaOS Linux stick (decision D-021).
const FALLBACK_CONFIG: &str = "\\LUCINA\\BOOT.CFG";
const LINUX_STICK_CONFIG: &str = "\\asterinas\\BOOT.CFG";
/// The log's name next to BOOT.CFG (or the loader) when BOOT.CFG names none.
const LOG_NAME: &str = "LOG.TXT";
/// The board table (`lucina_boot::board`), from the root of the volume the loader booted from.
const BOARDS_PATH: &str = "\\LUCINA\\BOARDS";
/// Largest device tree read from the firmware's configuration table.
const DTB_LIMIT: usize = 8 << 20;
/// Room the edits may add to the device tree.
const DTB_SLACK: usize = 64 << 10;
/// Keep the initramfs and the device tree below 4 GiB when the firmware can.
const LOW_LIMIT: u64 = 0xffff_ffff;
/// Seconds an error stays on screen before the loader returns to the firmware's boot menu.
const ERROR_PAUSE_S: usize = 15;
/// `GROUND` of `apps/lucina-app` (the night sky's top): what a `quiet = 1` boot shows from here to the interface's first frame.
const QUIET_BACKGROUND: u32 = 0x070b18;

/// Status band code for a panic (the [`Fail`] codes are 1 to 7).
const PANIC_CODE: u8 = 8;
/// Attempts at ExitBootServices (the map key changes when the firmware allocates in between).
const EXIT_ATTEMPTS: usize = 8;

enum Fail {
    Config(&'static str, Status),
    ConfigSyntax(ConfigError),
    NoKernel,
    Kernel(&'static str, Status),
    Image(image::Error),
    Initrd(Status),
    NoDeviceTree,
    /// `board = auto` and no rule matches: the screen and the log have the lines to add.
    UnknownBoard,
    DeviceTree(Status),
    Fdt(lucina_fdt::Error),
    Tree(TreeError),
    MemoryMap(Status),
    Ranges(RangeError),
    El3,
}

impl Fail {
    /// The code the status band shows (`lucina_boot::band`); 8 is a panic ([`PANIC_CODE`]).
    fn code(&self) -> u8 {
        match self {
            Fail::Config(..) | Fail::ConfigSyntax(_) => 1,
            Fail::NoKernel | Fail::Kernel(..) | Fail::Image(_) => 2,
            Fail::Initrd(_) => 3,
            Fail::NoDeviceTree
            | Fail::UnknownBoard
            | Fail::DeviceTree(_)
            | Fail::Fdt(_)
            | Fail::Tree(_) => 4,
            Fail::MemoryMap(_) | Fail::Ranges(_) => 5,
            Fail::El3 => 7,
        }
    }
}

impl fmt::Display for Fail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Fail::Config(what, s) => write!(f, "{CONFIG_NAME}: {what}: {}", status_name(*s)),
            Fail::ConfigSyntax(e) => write!(f, "{CONFIG_NAME}: {e}"),
            Fail::NoKernel => f.write_str("no kernel candidate fits (see above)"),
            Fail::Kernel(what, s) => write!(f, "kernel: {what}: {}", status_name(*s)),
            Fail::Image(e) => write!(f, "kernel: {e}"),
            Fail::Initrd(s) => write!(f, "initramfs: {}", status_name(*s)),
            Fail::NoDeviceTree => f.write_str(
                "no dtb= in the config and no device tree from the firmware (ACPI only?)",
            ),
            Fail::UnknownBoard => {
                write!(f, "unknown board: add one of these lines to {BOARDS_PATH}")
            }
            Fail::DeviceTree(s) => write!(f, "device tree: {}", status_name(*s)),
            Fail::Fdt(e) => write!(f, "device tree: {e}"),
            Fail::Tree(e) => write!(f, "{e}"),
            Fail::MemoryMap(s) => write!(f, "memory map: {}", status_name(*s)),
            Fail::Ranges(e) => write!(f, "memory map: {e:?}"),
            Fail::El3 => f.write_str("entered at EL3: the kernel needs EL1 (or EL2 to drop from)"),
        }
    }
}

impl From<lucina_fdt::Error> for Fail {
    fn from(e: lucina_fdt::Error) -> Self {
        Fail::Fdt(e)
    }
}

impl From<TreeError> for Fail {
    fn from(e: TreeError) -> Self {
        Fail::Tree(e)
    }
}

impl From<RangeError> for Fail {
    fn from(e: RangeError) -> Self {
        Fail::Ranges(e)
    }
}

/// The firmware console plus a copy of everything for the log file. `note!` lines go to the screen only in
/// verbose mode.
struct Out<'e> {
    efi: &'e Efi,
    log: Option<&'static mut [u8]>,
    len: usize,
    screen: bool,
    verbose: bool,
    /// Nothing reaches the screen yet: BOOT.CFG may say `quiet = 1`. The log has it all.
    held: bool,
}

impl Out<'_> {
    fn log_bytes(&self) -> &[u8] {
        self.log.as_deref().map_or(&[], |l| &l[..self.len])
    }

    /// From here on it is shown, with what was held back: a config without `quiet`, or a failure.
    fn release(&mut self) {
        if self.held {
            self.held = false;
            if let Ok(text) = core::str::from_utf8(self.log_bytes()) {
                self.efi.print(text);
            }
        }
    }
}

impl fmt::Write for Out<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        if self.screen && !self.held {
            self.efi.print(s);
        }
        if let Some(log) = self.log.as_deref_mut() {
            let n = s.len().min(log.len() - self.len);
            log[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
            self.len += n;
        }
        Ok(())
    }
}

macro_rules! say {
    ($o:expr, $($t:tt)*) => {{
        $o.screen = true;
        let _ = writeln!($o, $($t)*);
    }};
}

macro_rules! note {
    ($o:expr, $($t:tt)*) => {{
        $o.screen = $o.verbose;
        let _ = writeln!($o, $($t)*);
    }};
}

/// Everything the jump needs, all in `LoaderData` pages that outlive boot services.
struct Prepared {
    kernel: Range,
    initrd: Option<Range>,
    src: &'static [u8],
    dtb_out: &'static mut [u8],
    map_buf: &'static mut [u8],
    fb: Option<Fb>,
    cmdline: &'static str,
    own_image: Option<Range>,
    el: u8,
    pstore: Option<Range>,
    quiet: bool,
    rng: Option<RngSeed>,
    rng_note: Option<&'static [u8]>,
}

/// The directory relative paths resolve against: the loader's own until BOOT.CFG is found, then BOOT.CFG's; and
/// the volume it is on.
struct Base {
    buf: [u8; MAX_PATH],
    len: usize,
    volume: Option<efi::Handle>,
}

impl Base {
    fn set(&mut self, dir: &str) {
        self.len = dir.len().min(MAX_PATH - 1);
        self.buf[..self.len].copy_from_slice(&dir.as_bytes()[..self.len]);
    }

    fn dir(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }

    fn path<'b>(&self, p: &str, out: &'b mut [u8; MAX_PATH]) -> Result<&'b str, Status> {
        uefi::resolve(self.dir(), p, out).ok_or(efi::NOT_FOUND)
    }
}

fn pages(bytes: usize) -> usize {
    bytes.div_ceil(4096)
}

/// Reads a whole file into fresh pages placed by `place`, falling back to anywhere.
/// A few bytes of text, for the trial's argument.
struct TextBuf {
    bytes: [u8; 96],
    len: usize,
}

impl TextBuf {
    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.len]).unwrap_or("")
    }
}

impl fmt::Write for TextBuf {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let end = self.len + s.len();
        self.bytes.get_mut(self.len..end).ok_or(fmt::Error)?.copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}

/// The trial (`lucina_boot::trial`): counts this boot before anything else can go wrong, and says which configuration to start and what to tell
/// userspace. Any read that fails means no record, so that a boot with no trial is what it always was. A count that cannot be written
/// does not stop the boot: it is logged, and a machine that boots is better than one that refuses over a file.
fn trial_step<'t>(
    efi: &Efi,
    dir: &efi::Dir<'_>,
    base: &Base,
    text: &'t str,
    out: &mut Out<'_>,
) -> (&'t str, Option<(bool, u32)>) {
    let Some(generation) = uefi::config_generation(text) else {
        return (text, None);
    };
    let read = |path: &str| {
        read_file(efi, dir, path, Place::Anywhere)
            .ok()
            .and_then(|b| trial::Record::decode(b))
    };
    let newest = trial::newest(read(trial::COPY_A), read(trial::COPY_B));
    let Some(record) = newest.record else {
        return (text, None);
    };
    if record.generation != generation {
        return (text, None);
    }
    // PREV.CFG counts only if it parses, names another generation, and its kernel is there.
    let mut prev_path = [0u8; MAX_PATH];
    let previous = base.path("PREV.CFG", &mut prev_path).ok().and_then(|p| {
        let t = read_file(efi, dir, p, Place::Anywhere).ok()?;
        let t: &'static str = core::str::from_utf8(t).ok()?;
        let cfg = Config::parse(t).ok()?;
        if uefi::config_generation(t)? == generation {
            return None;
        }
        let kernel = cfg.kernels.iter().flatten().next()?;
        let mut k = [0u8; MAX_PATH];
        dir.open(base.path(kernel.path, &mut k).ok()?).ok()?;
        Some(t)
    });
    let decision = trial::decide(Some(record), generation, previous.is_some());
    if let Some(next) = decision.write {
        let path = match newest.write_to {
            trial::Copy::A => trial::COPY_A,
            trial::Copy::B => trial::COPY_B,
        };
        match dir.overwrite(path, &next.encode()) {
            Ok(()) => say!(out, "trial: generation {generation}, {} tries left", next.tries),
            Err(s) => say!(out, "trial: cannot write {path}: {s:#x}; booting without counting"),
        }
    }
    match (decision.start, previous) {
        (trial::Start::Previous, Some(t)) => {
            say!(out, "trial: generation {generation} failed: starting PREV.CFG");
            (t, Some((true, generation)))
        }
        _ if record.state == trial::State::Good => (text, None),
        _ => (text, Some((false, generation))),
    }
}

fn read_file(
    efi: &Efi,
    dir: &efi::Dir<'_>,
    path: &str,
    place: Place,
) -> Result<&'static mut [u8], Status> {
    let f = dir.open(path)?;
    let size = f.size()? as usize;
    let buf = match efi.allocate_pages(place, pages(size.max(1))) {
        Ok(b) => b,
        Err(_) => efi.allocate_pages(Place::Anywhere, pages(size.max(1)))?,
    };
    f.read_exact(&mut buf[..size])?;
    Ok(&mut buf[..size])
}

fn range_of(buf: &[u8]) -> Range {
    Range {
        start: buf.as_ptr() as u64,
        end: buf.as_ptr() as u64 + buf.len() as u64,
    }
}

fn mib(bytes: u64) -> u64 {
    bytes >> 20
}

/// Loads the first kernel candidate whose window is free.
fn load_kernel(
    efi: &Efi,
    dir: &efi::Dir<'_>,
    base: &Base,
    cfg: &Config<'_>,
    out: &mut Out<'_>,
    free: &[Range],
) -> Result<Range, Fail> {
    let mut seen: Option<Image> = None;
    for k in cfg.kernels() {
        let mut buf = [0u8; MAX_PATH];
        let f = match base.path(k.path, &mut buf).and_then(|p| dir.open(p)) {
            Ok(f) => f,
            Err(s) => {
                say!(out, "kernel {}: {}", k.path, status_name(s));
                continue;
            }
        };
        let size = f.size().map_err(|s| Fail::Kernel("size", s))?;
        let mut head = [0u8; image::HEADER_SIZE];
        f.read_exact(&mut head)
            .map_err(|s| Fail::Kernel("header", s))?;
        let mut img = Image::parse(&head).map_err(Fail::Image)?;
        if img.image_size < size {
            return Err(Fail::Image(image::Error::BadLayout));
        }
        img.file_size = size;
        seen.get_or_insert(img);
        let Some(w) = kernel_window(&img, k.dram_base) else {
            say!(
                out,
                "kernel {} for base {:#x}: window ends above 3 GiB",
                k.path,
                k.dram_base
            );
            continue;
        };
        let buf = match efi.allocate_pages(Place::At(w.start), (w.size() / uefi::PAGE) as usize) {
            Ok(b) => b,
            Err(s) => {
                say!(
                    out,
                    "kernel {} for base {:#x}: window {w} is not free RAM ({})",
                    k.path,
                    k.dram_base,
                    status_name(s)
                );
                continue;
            }
        };
        f.rewind().map_err(|s| Fail::Kernel("rewind", s))?;
        f.read_exact(&mut buf[..size as usize])
            .map_err(|s| Fail::Kernel("read", s))?;
        say!(
            out,
            "kernel {} ({} KiB, {} KiB loaded) at {w} (OSTD_ARM_DRAM_BASE {:#x})",
            k.path,
            size >> 10,
            img.image_size >> 10,
            k.dram_base
        );
        return Ok(w);
    }
    if let Some(img) = seen {
        match suggest_base(free, &img, 0) {
            Some(base) => say!(
                out,
                "free RAM below 3 GiB fits this kernel at OSTD_ARM_DRAM_BASE={base:#x} (rebuild with it, add a kernel= line)"
            ),
            None => say!(
                out,
                "no free RAM below 3 GiB fits this kernel ({} KiB)",
                img.image_size >> 10
            ),
        }
    }
    Err(Fail::NoKernel)
}

/// `board = auto`: which board this is, from the firmware device tree's root `compatible` and SMBIOS, against `\LUCINA\BOARDS`.
/// An unknown board prints the lines to add, waits for a key and fails: nothing is started.
fn find_board(efi: &Efi, dir: &efi::Dir<'_>, out: &mut Out<'_>) -> Result<&'static str, Fail> {
    let compatible = efi
        .config_table(&DTB_TABLE)
        .and_then(|a| efi.device_tree_at(a, DTB_LIMIT))
        .and_then(|d| Fdt::new(d).ok())
        .and_then(|f| f.root().ok())
        .and_then(|r| r.property("compatible").ok().flatten());
    let strings = || {
        compatible
            .iter()
            .flat_map(|p| p.strings())
            .filter_map(Result::ok)
    };
    let smbios = efi
        .config_table(&SMBIOS3_TABLE)
        .and_then(|a| efi.smbios3_table(a))
        .and_then(board::smbios_system);
    let table: &'static str = match read_file(efi, dir, BOARDS_PATH, Place::Anywhere) {
        Ok(t) => core::str::from_utf8(t)
            .map_err(|_| Fail::Config("BOARDS not UTF-8", efi::LOAD_ERROR))?,
        Err(s) => {
            say!(out, "board: {BOARDS_PATH}: {}", status_name(s));
            ""
        }
    };
    let bad = board::malformed(table);
    if bad > 0 {
        say!(
            out,
            "board: {bad} malformed line(s) in {BOARDS_PATH} are ignored"
        );
    }
    if let Some((id, line)) = board::pick(table, strings(), smbios) {
        say!(out, "board: {id} (rule at {BOARDS_PATH} line {line})");
        return Ok(id);
    }
    out.release();
    if let Some(c) = strings().next() {
        say!(out, "compatible \"{c}\" <new-board-id>");
    }
    if let Some((m, p)) = smbios {
        say!(out, "smbios \"{m}\" \"{p}\" <new-board-id>");
    }
    say!(
        out,
        "unknown board: add one of these lines to {BOARDS_PATH}"
    );
    say!(out, "press a key to return to the firmware");
    // The log is written by the error path below, before the wait would end.
    Err(Fail::UnknownBoard)
}

fn prepare(
    efi: &Efi,
    out: &mut Out<'_>,
    el: u8,
    band: &mut Option<Screen>,
    stages: &mut u8,
    base: &mut Base,
) -> Result<Prepared, Fail> {
    if el == 3 {
        return Err(Fail::El3);
    }
    let mut vendor = [0u8; 64];
    let (n, rev) = efi.vendor(&mut vendor);
    say!(
        out,
        "firmware: {} rev {rev:#x}",
        core::str::from_utf8(&vendor[..n]).unwrap_or("?")
    );
    let table = |g| efi.config_table(g).unwrap_or(0);
    say!(
        out,
        "tables: ACPI 2.0 {:#x}, device tree {:#x}, SMBIOS 3 {:#x} (0 = none)",
        table(&ACPI20_TABLE),
        table(&DTB_TABLE),
        table(&SMBIOS3_TABLE)
    );

    let mut own = [0u8; MAX_PATH];
    let own = efi.own_path(&mut own);
    base.set(uefi::parent_dir(own.unwrap_or("")));
    base.volume = efi.own_device();
    let mut next_to = [0u8; MAX_PATH];
    let next_to = own.and_then(|_| base.path(CONFIG_NAME, &mut next_to).ok());
    // BOOT.CFG next to the loader, else in one of the known folders; on the loader's own volume first, then on
    // every volume (a boot manager that starts the loader from memory, as LucinaLoader does, gives neither a path
    // nor a volume).
    let candidates = [next_to, Some(FALLBACK_CONFIG), Some(LINUX_STICK_CONFIG)];
    let own_volume = base.volume;
    let volumes = own_volume.into_iter().chain(
        efi.file_systems()
            .iter()
            .copied()
            .filter(|&h| Some(h) != own_volume),
    );
    let mut found = None;
    'search: for volume in volumes {
        let Ok(dir) = efi.volume_dir(volume) else {
            continue;
        };
        for path in candidates.iter().flatten() {
            if let Ok(text) = read_file(efi, &dir, path, Place::Anywhere) {
                found = Some((volume, dir, *path, text));
                break 'search;
            }
        }
    }
    let Some((volume, dir, cfg_path, text)) = found else {
        return Err(Fail::Config("not found", efi::NOT_FOUND));
    };
    say!(
        out,
        "started from {}; config {cfg_path}{}",
        own.unwrap_or("? (no file path)"),
        match own_volume {
            None => " (no boot volume given: found by searching every volume)",
            Some(v) if v != volume => " (on another volume)",
            Some(_) => "",
        }
    );
    base.set(uefi::parent_dir(cfg_path));
    base.volume = Some(volume);
    let text =
        core::str::from_utf8(text).map_err(|_| Fail::Config("not UTF-8", efi::LOAD_ERROR))?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let (text, trial_arg) = trial_step(efi, &dir, &base, text, out);
    let cfg = Config::parse(text).map_err(Fail::ConfigSyntax)?;
    if !cfg.quiet {
        out.release();
    }
    let board = if cfg.board {
        Some(find_board(efi, &dir, out)?)
    } else {
        None
    };
    // Arguments from whoever started the loader (GRUB's `chainloader <file> <args>`) join BOOT.CFG's.
    let mut args = [0u8; 1024];
    let cmdline: &'static str = match uefi::load_options_args(efi.load_options(), &mut args) {
        Some(extra) => {
            say!(out, "load options: {extra}");
            let buf = efi
                .allocate_pages(Place::Anywhere, 1)
                .map_err(|s| Fail::Config("cmdline", s))?;
            uefi::merge_cmdline(cfg.cmdline, extra, buf)
                .ok_or(Fail::Config("cmdline too long", efi::LOAD_ERROR))?
        }
        None => cfg.cmdline,
    };
    // The trial's word to userspace, `lucina.trial=on:16` or `rolledback:16`, joins the rest.
    let mut trial_text = TextBuf { bytes: [0; 96], len: 0 };
    let cmdline: &'static str = match trial_arg {
        Some((rolled_back, generation)) => {
            let _ = write!(
                trial_text,
                "lucina.trial={}:{generation}",
                if rolled_back { "rolledback" } else { "on" }
            );
            let buf = efi
                .allocate_pages(Place::Anywhere, 1)
                .map_err(|s| Fail::Config("cmdline", s))?;
            uefi::merge_cmdline(cmdline, trial_text.as_str(), buf)
                .ok_or(Fail::Config("cmdline too long", efi::LOAD_ERROR))?
        }
        None => cmdline,
    };
    // The board's name to userspace joins the rest, before ` -- `.
    let mut board_text = TextBuf {
        bytes: [0; 96],
        len: 0,
    };
    let cmdline: &'static str = match board {
        Some(id) => {
            let _ = write!(board_text, "lucinaos.board={id}");
            let buf = efi
                .allocate_pages(Place::Anywhere, 1)
                .map_err(|s| Fail::Config("cmdline", s))?;
            uefi::merge_cmdline(cmdline, board_text.as_str(), buf)
                .ok_or(Fail::Config("cmdline too long", efi::LOAD_ERROR))?
        }
        None => cmdline,
    };
    out.verbose = cfg.verbose;
    *stages = 1;

    // GOP first: the band can then show errors from here on.
    let fb = match efi.gop() {
        Some(g) => {
            say!(
                out,
                "GOP: {}x{}, {} pixels per line, pixel format {}, framebuffer {:#x}+{:#x}",
                g.width,
                g.height,
                g.pixels_per_line,
                g.format,
                g.base,
                g.size
            );
            let fb = uefi::framebuffer(&g);
            match &fb {
                Some(fb) if fb.swapped => say!(
                    out,
                    "framebuffer: firmware layout is RGBX, red and blue swap"
                ),
                Some(_) => {}
                None => say!(
                    out,
                    "framebuffer: none usable (BLT only or unknown layout): no kernel log on screen"
                ),
            }
            fb
        }
        None => {
            say!(out, "GOP: none: no kernel log on screen");
            None
        }
    };
    *band = fb.as_ref().and_then(Screen::new);
    // Quiet: the interface's background at once, and it stays until the kernel, then the interface, paint the same colour.
    if cfg.quiet
        && let Some(b) = band.as_ref()
    {
        b.fill(QUIET_BACKGROUND);
    }

    // The kernel's saved crash log, before anything else lands there. These pages keep what they hold.
    let pstore = if cfg.pstore {
        let pages = (PSTORE_SIZE / uefi::PAGE) as usize;
        let at = PSTORE_BASES
            .into_iter()
            .find(|&at| efi.reserve_pages(at, pages).is_ok());
        match at {
            Some(at) => say!(out, "saved crash log: {:#x}+{:#x}", at, PSTORE_SIZE),
            None => say!(
                out,
                "saved crash log: none of the addresses is free: crashes are not kept"
            ),
        }
        at.and_then(|at| Range::new(at, PSTORE_SIZE))
    } else {
        None
    };

    // The memory map as it is now: what is free for the kernel, and what the kernel will be told is RAM.
    let map_buf = efi
        .allocate_pages(Place::Anywhere, pages(efi.memory_map_size() * 2 + 16384))
        .map_err(Fail::MemoryMap)?;
    let (used, _, desc_size) = efi
        .memory_map(map_buf)
        .map_err(|(s, _)| Fail::MemoryMap(s))?;
    let map = &map_buf[..used];
    note!(out, "memory map ({} descriptors):", used / desc_size);
    for d in descriptors(map, desc_size) {
        let end = d.start + d.pages * uefi::PAGE;
        note!(
            out,
            "  {:<12} {:#011x}-{:#011x} attr {:#x}",
            type_name(d.typ),
            d.start,
            end,
            d.attr
        );
    }
    let mut free = [Range::default(); MAX_RANGES];
    let n_free = memory_ranges(descriptors(map, desc_size), MemDesc::free, &mut free)?;
    let free = &free[..n_free];

    let kernel = load_kernel(efi, &dir, base, &cfg, out, free)?;
    *stages = 2;

    let initrd = match cfg.initrd {
        Some(p) => {
            let mut path = [0u8; MAX_PATH];
            let buf = base
                .path(p, &mut path)
                .and_then(|p| read_file(efi, &dir, p, Place::Below(LOW_LIMIT)))
                .map_err(Fail::Initrd)?;
            let r = range_of(buf);
            say!(out, "initramfs {p} ({} KiB) at {r}", r.size() >> 10);
            Some(r)
        }
        None => None,
    };

    // The board's device tree, unless BOOT.CFG names one.
    let mut board_dtb = TextBuf {
        bytes: [0; 96],
        len: 0,
    };
    if let (None, Some(id)) = (cfg.dtb, board) {
        let _ = write!(board_dtb, "\\LUCINA\\DTB\\{id}.dtb");
    }
    let dtb_path = cfg.dtb.or((board_dtb.len > 0).then(|| board_dtb.as_str()));
    let mut src: &'static [u8] = match dtb_path {
        Some(p) => {
            let mut path = [0u8; MAX_PATH];
            let buf = base
                .path(p, &mut path)
                .and_then(|p| read_file(efi, &dir, p, Place::Below(LOW_LIMIT)))
                .map_err(Fail::DeviceTree)?;
            say!(out, "device tree {p} ({} bytes)", buf.len());
            buf
        }
        None => {
            let addr = efi.config_table(&DTB_TABLE).ok_or(Fail::NoDeviceTree)?;
            let dtb = efi
                .device_tree_at(addr, DTB_LIMIT)
                .ok_or(Fail::NoDeviceTree)?;
            say!(
                out,
                "device tree from the firmware at {addr:#x} ({} bytes)",
                dtb.len()
            );
            dtb
        }
    };
    let mut fdt = Fdt::new(src)?;
    if fdt.node("/chosen")?.is_none() {
        // The framebuffer node needs /chosen as a parent: add it first.
        let tmp = efi
            .allocate_pages(Place::Below(LOW_LIMIT), pages(src.len() + 4096))
            .map_err(Fail::DeviceTree)?;
        let edit = [Edit::Set {
            node: "/chosen",
            name: "bootargs",
            value: b"\0",
        }];
        let len = patch(&fdt, &edit, tmp)?;
        src = &tmp[..len];
        fdt = Fdt::new(src)?;
    }
    if pstore.is_some() && fdt.node(RESERVED_MEMORY)?.is_none() {
        let tmp = efi
            .allocate_pages(Place::Below(LOW_LIMIT), pages(src.len() + 4096))
            .map_err(Fail::DeviceTree)?;
        // A tree without the node: as for /chosen above.
        let len = patch(&fdt, &RESERVED_MEMORY_EDITS, tmp)?;
        src = &tmp[..len];
        fdt = Fdt::new(src)?;
    }
    let dtb_out = efi
        .allocate_pages(Place::Below(LOW_LIMIT), pages(src.len() + DTB_SLACK))
        .map_err(Fail::DeviceTree)?;

    // A dry run with the current map, so that any error shows here, while it can still be printed.
    let (used, _, desc_size) = efi
        .memory_map(map_buf)
        .map_err(|(s, _)| Fail::MemoryMap(s))?;
    let mut ram = [Range::default(); MAX_RANGES];
    let n_ram = memory_ranges(
        descriptors(&map_buf[..used], desc_size),
        MemDesc::usable,
        &mut ram,
    )?;
    let ram = &ram[..n_ram];
    let mut values = TreeValues::new(&fdt, ram, initrd, cmdline, fb.as_ref())?;
    let (rng, rng_note) = if cfg.rng {
        firmware_seed(efi, &dir, out)
    } else {
        say!(out, "random seed: not asked for (rng = off)");
        (None, Some(&b"not asked for the firmware's seed: rng = off\0"[..]))
    };
    if let Some(seed) = &rng {
        values.set_rng_seed(seed);
    }
    if let Some(note) = rng_note {
        values.set_rng_note(note);
    }
    // Nothing about the saved log may stop a boot.
    let pstore = pstore.filter(|&region| match values.set_pstore(&fdt, region) {
        Ok(()) => true,
        Err(err) => {
            say!(out, "saved crash log: left out of the device tree: {err:?}");
            false
        }
    });
    let (e, k) = edits(&values);
    let len = patch(&fdt, &e[..k], dtb_out)?;
    values.wipe_rng_seed();
    let total: u64 = ram.iter().map(Range::size).sum();
    say!(
        out,
        "RAM for the kernel: {} MiB in {n_ram} ranges; device tree {len} bytes at {:#x}",
        mib(total),
        dtb_out.as_ptr() as u64
    );
    for r in ram {
        note!(out, "  /memory {r}");
    }
    say!(out, "cmdline: {cmdline}");
    *stages = 3;

    match el {
        2 => say!(out, "CurrentEL=EL2: the loader drops to EL1 for the kernel"),
        _ => say!(out, "CurrentEL=EL{el}: the kernel starts at EL{el}"),
    }
    let own_image = efi
        .own_image()
        .and_then(|(base, size)| Range::new(base, size));

    note!(out, "counter before the jump: {} ms", arch::counter_ms());
    let mut log_path = [0u8; MAX_PATH];
    let log_path = base.path(cfg.log.unwrap_or(LOG_NAME), &mut log_path);
    say!(
        out,
        "log: {}; exiting boot services and starting the kernel",
        log_path.unwrap_or("path too long")
    );
    // Writing it costs time on every boot; a failure above wrote it anyway.
    if !cfg.quiet
        && let Err(s) = log_path.and_then(|p| dir.write_file(p, out.log_bytes()))
    {
        say!(out, "log: not written ({})", status_name(s));
    }
    drop(dir);
    if !cfg.quiet {
        for s in (1..=cfg.pause_s).rev() {
            let _ = write!(PrintOnly(efi), "\rstarting in {s} s ");
            efi.stall_ms(1000);
        }
        if cfg.pause_s > 0 {
            efi.print("\n");
        }
    }

    Ok(Prepared {
        kernel,
        initrd,
        src,
        dtb_out,
        map_buf,
        fb,
        cmdline,
        own_image,
        el,
        pstore,
        quiet: cfg.quiet,
        rng,
        rng_note,
    })
}

/// How long the firmware gets for the marker, the call and clearing the marker.
const RNG_WATCHDOG_S: usize = 30;

/// The kernel's seed from the firmware, and why not if not. A firmware with none costs the kernel its head start and nothing else,
/// and one that does not come back from the call costs one restart: `rng_marker` has the rules.
fn firmware_seed(
    efi: &Efi,
    dir: &efi::Dir<'_>,
    out: &mut Out<'_>,
) -> (Option<RngSeed>, Option<&'static [u8]>) {
    if !efi.has_random_protocol() {
        say!(out, "random seed: the firmware has no EFI_RNG_PROTOCOL");
        return (None, Some(b"the firmware has no EFI_RNG_PROTOCOL\0"));
    }
    let read = |path: &str| {
        read_file(efi, dir, path, Place::Anywhere)
            .ok()
            .and_then(|b| rng_marker::Record::decode(b))
    };
    let newest = rng_marker::newest(read(rng_marker::COPY_A), read(rng_marker::COPY_B));
    let marker = match rng_marker::plan(newest.record) {
        rng_marker::Plan::Call(marker) => marker,
        rng_marker::Plan::NoCall(why, write) => {
            if let Some(next) = write
                && let Err(s) = dir.overwrite(rng_marker::path(newest.write_to), &next.encode())
            {
                say!(out, "random seed: cannot write the record: {}", status_name(s));
            }
            let note: &'static [u8] = match why {
                rng_marker::Why::NoRecord => b"firmware seed not asked for: no marker record on the EFI partition\0",
                rng_marker::Why::DidNotReturn => {
                    b"firmware seed skipped for good: GetRNG did not come back on an earlier boot\0"
                }
                rng_marker::Why::SkippedBefore => {
                    b"firmware seed skipped for good: GetRNG once did not come back\0"
                }
                rng_marker::Why::OutOfNumbers => b"firmware seed not asked for: the marker record is out of numbers\0",
            };
            say!(out, "random seed: none: {why:?}");
            return (None, Some(note));
        }
    };
    efi.arm_watchdog(RNG_WATCHDOG_S);
    let path = rng_marker::path(newest.write_to);
    let written = dir
        .overwrite(path, &marker.encode())
        .map_err(status_name)
        .and_then(|()| (read(path) == Some(marker)).then_some(()).ok_or("it does not read back"));
    if let Err(why) = written {
        efi.disable_watchdog();
        say!(out, "random seed: the marker cannot be written ({why}): no call");
        return (None, Some(b"firmware seed not asked for: the marker cannot be written\0"));
    }
    say!(out, "random seed: marker written");
    let mut bytes = [0u8; RNG_SEED_BYTES];
    let started = arch::counter_us();
    let result = efi.random_bytes(&mut bytes);
    let took_us = arch::counter_us().wrapping_sub(started);
    // It came back, whatever it says. If this write fails the next boot takes it for a hang, the safe way round.
    let clear = rng_marker::after_call(marker);
    let cleared = dir.overwrite(rng_marker::path(rng_marker::other(newest.write_to)), &clear.encode());
    efi.disable_watchdog();
    if let Err(s) = cleared {
        say!(out, "random seed: the marker cannot be cleared ({}): no more calls", status_name(s));
    }
    match result {
        Ok(raw) => {
            say!(out, "random seed: {RNG_SEED_BYTES} bytes from EFI_RNG_PROTOCOL in {took_us} us");
            (Some(RngSeed::new(bytes, raw, took_us)), None)
        }
        Err(status) => {
            say!(out, "random seed: none from the firmware ({})", status_name(status));
            (None, Some(b"the firmware's GetRNG returned an error\0"))
        }
    }
}

/// Console output that is not logged (the countdown).
struct PrintOnly<'e>(&'e Efi);

impl fmt::Write for PrintOnly<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.0.print(s);
        Ok(())
    }
}

/// Exits boot services and starts the kernel. From the first ExitBootServices call on, only GetMemoryMap may be
/// called and nothing can be printed: the device tree is rebuilt from each new map without allocating.
fn boot(efi: Efi, mut p: Prepared, band: Option<Screen>) -> ! {
    let fdt = match Fdt::new(p.src) {
        Ok(f) => f,
        Err(_) => stop(&band, 4),
    };
    let mut efi = efi;
    let mut dtb_len = 0;
    let mut exited = false;
    for _ in 0..EXIT_ATTEMPTS {
        let Ok((used, key, desc_size)) = efi.memory_map(p.map_buf) else {
            stop(&band, 5)
        };
        let mut ram = [Range::default(); MAX_RANGES];
        let Ok(n) = memory_ranges(
            descriptors(&p.map_buf[..used], desc_size),
            MemDesc::usable,
            &mut ram,
        ) else {
            stop(&band, 5)
        };
        let Ok(mut values) = TreeValues::new(&fdt, &ram[..n], p.initrd, p.cmdline, p.fb.as_ref())
        else {
            stop(&band, 4)
        };
        if let Some(region) = p.pstore
            && values.set_pstore(&fdt, region).is_err()
        {
            stop(&band, 4)
        }
        if let Some(seed) = &p.rng {
            values.set_rng_seed(seed);
        }
        if let Some(note) = p.rng_note {
            values.set_rng_note(note);
        }
        let (e, k) = edits(&values);
        let Ok(len) = patch(&fdt, &e[..k], p.dtb_out) else {
            stop(&band, 4)
        };
        dtb_len = len;
        values.wipe_rng_seed();
        match efi.exit_boot_services(key) {
            Ok(()) => {
                exited = true;
                break;
            }
            Err((back, _)) => efi = back,
        }
    }
    if !exited {
        stop(&band, 6);
    }
    if let Some(seed) = &mut p.rng {
        seed.wipe();
    }

    if !p.quiet
        && let Some(b) = &band
    {
        b.show(&BandStatus {
            el: p.el,
            stages: 5,
            error: None,
        });
    }
    let dtb = range_of(&p.dtb_out[..dtb_len]);
    for r in [Some(p.kernel), p.initrd, Some(dtb), p.own_image]
        .into_iter()
        .flatten()
    {
        arch::dcache_clean(r);
    }
    // SAFETY: boot services are gone; the kernel window holds the image (BSS zeroed by allocate_pages), the
    // device tree and initramfs are written, and all of them plus this loader are clean to the PoC.
    unsafe { arch::enter_kernel(p.kernel.start, dtb.start, p.el) }
}

/// After ExitBootServices nothing can be printed: show the error code on the band and stop.
fn stop(band: &Option<Screen>, code: u8) -> ! {
    if let Some(b) = band {
        b.show(&BandStatus {
            el: arch::current_el(),
            stages: 4,
            error: Some(code),
        });
    }
    arch::wait_forever()
}

#[unsafe(no_mangle)]
extern "efiapi" fn efi_main(image: efi::Handle, st: *mut efi::SystemTable) -> Status {
    // SAFETY: the firmware passes our image handle and its live system table to the entry point.
    let efi = unsafe { Efi::new(image, st) };
    efi.disable_watchdog();
    let el = arch::current_el();
    let log = efi.allocate_pages(Place::Anywhere, 16).ok();
    let mut band = None;
    let mut stages = 0;
    let mut base = Base {
        buf: [0; MAX_PATH],
        len: 0,
        volume: None,
    };
    let prepared = {
        let mut out = Out {
            efi: &efi,
            log,
            len: 0,
            screen: true,
            verbose: false,
            held: true,
        };
        say!(out, "LucinaUEFI {VERSION}: CurrentEL=EL{el}");
        note!(
            out,
            "counter at the loader's start: {} ms",
            arch::counter_ms()
        );
        let r = prepare(&efi, &mut out, el, &mut band, &mut stages, &mut base);
        if let Err(f) = &r {
            out.release();
            say!(out, "LucinaUEFI: error: {f}");
            if let Some(b) = &band {
                b.show(&BandStatus {
                    el,
                    stages,
                    error: Some(f.code()),
                });
            }
            if let Some(Ok(dir)) = base.volume.map(|v| efi.volume_dir(v)) {
                let mut path = [0u8; MAX_PATH];
                if let Ok(p) = base.path(LOG_NAME, &mut path) {
                    let _ = dir.write_file(p, out.log_bytes());
                }
            }
            if let Fail::UnknownBoard = f {
                // The lines to add are on the screen: wait for the key `find_board` asked for.
                efi.wait_key();
            } else {
                say!(out, "returning to the firmware in {ERROR_PAUSE_S} s");
                efi.stall_ms(ERROR_PAUSE_S * 1000);
            }
        }
        r
    };
    match prepared {
        Ok(p) => boot(efi, p, band),
        Err(_) => efi::LOAD_ERROR,
    }
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    let _ = writeln!(
        efi::PanicConsole,
        "LucinaUEFI: panic (band code {PANIC_CODE}): {info}"
    );
    arch::wait_forever()
}
