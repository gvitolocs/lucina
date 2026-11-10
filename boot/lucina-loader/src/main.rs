//! LucinaLoader: the UEFI boot menu (`EFI/BOOT/BOOTAA64.EFI`) that starts LucinaOS's Linux on the Samsung Galaxy
//! Book Go in GRUB's place. It does what lucinaos' standalone GRUB and its `grub.cfg` did:
//!
//! 1. Find the FAT partition holding `/lucinaos/boot.toml` (the one it was started from first, then every other
//!    one: GRUB's built-in `search --file`), read it, the files it includes and the GRUB environment block it
//!    names ([`lucina_boot::menu::Config`]).
//! 2. Show the menu with the LucinaOS theme on GOP (or as text when there is no GOP, as under QEMU's serial
//!    tests), count down, follow the arrow keys and Enter ([`lucina_boot::menu::Menu`]).
//! 3. Start the chosen entry: read the EFI-stub kernel and LoadImage it, give it `BOOT_IMAGE=<path> <cmdline>` as
//!    load options, install the device tree file as the DTB configuration table, publish the initrd files
//!    (concatenated) through LoadFile2 on Linux's initrd device path, and StartImage. The kernel's stub exits boot
//!    services. Or restart into the firmware's setup screen (`fwsetup`).
//!
//! The installed laptop's system lives in update slots (`lucinaos/a|b/`): before the menu shows, the loader chooses
//! one from the GRUB environment block, as grub.cfg does ([`lucina_boot::menu::slot_boot`]).
//!
//! The installed laptop's system lives in update slots (`lucinaos/a|b/`): before the menu shows, the loader chooses
//! one from the GRUB environment block, as grub.cfg does ([`lucina_boot::menu::slot_boot`]).
//!
//! The decisions are in `kernel/lucina-boot/src/menu.rs` (`no_std`, `unsafe`-free, tested on the host). `unsafe` is
//! confined to `src/efi.rs`.

#![no_std]
#![no_main]

mod draw;
mod efi;
mod font;

use core::{
    fmt::{self, Write},
    panic::PanicInfo,
    ptr::null_mut,
};

use lucina_boot::menu::{
    Action, CMDLINE_MAX, Config, ConfigError, ENV_BLOCK_SIZE, Entry, Key, Layout, MAX_INITRDS,
    MAX_LINES, MAX_SAVES, Menu, Shown, SlotKind, Step, Vars, env_get, env_update, env_vars, expand,
    fat_volume_id, initrd_layout, kernel_cmdline, slot_boot, trial_slot,
};
use lucina_fdt::Fdt;

use draw::{Canvas, Line};
use efi::{DTB_TABLE, Dir, Display, Efi, Handle, KeyPress, Status, status_name};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const MENU_FILE: &str = "/lucinaos/boot.toml";
const MAX_VOLUMES: usize = 64;
/// Largest menu file, include or environment block read.
const TEXT_MAX: u64 = 64 << 10;
/// Free space after the device tree (GRUB_EFI_LINUX_FDT_EXTRA_SPACE).
const DTB_EXTRA: usize = 0x400;
const TICK_MS: u64 = 20;
/// How long the reason stays on screen when there is no menu to show (GRUB's built-in config slept 30 s).
const NO_MENU_WAIT_MS: u64 = 30_000;
const PATH_MAX: usize = 256;
/// OsIndications bit EFI_OS_INDICATIONS_BOOT_TO_FW_UI.
const BOOT_TO_FW_UI: u64 = 1;

struct Con<'e>(&'e Efi);

impl fmt::Write for Con<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.0.print(s);
        Ok(())
    }
}

/// Why there is no menu.
enum Fatal {
    NoMenu,
    Read(&'static str, Status),
    NotText(&'static str),
    Config(&'static str, ConfigError),
    Empty,
}

impl fmt::Display for Fatal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Fatal::NoMenu => write!(f, "no partition holds {MENU_FILE}"),
            Fatal::Read(p, s) => write!(f, "{p}: {}", status_name(*s)),
            Fatal::NotText(p) => write!(f, "{p}: not UTF-8 text"),
            Fatal::Config(p, e) => write!(f, "{p}: {e}"),
            Fatal::Empty => write!(f, "{MENU_FILE}: no entry to show"),
        }
    }
}

/// Where the menu goes: the theme on GOP, or the firmware's text console.
enum Screen<'e> {
    Graphics {
        display: Display<'e>,
        canvas: Canvas<'static>,
        layout: Layout,
    },
    Text,
}

/// Reads a whole file into LoaderData pages.
fn read_file(efi: &Efi, dir: &Dir<'_>, path: &str) -> Result<&'static mut [u8], Status> {
    let f = dir.open(path)?;
    let size = usize::try_from(f.size()?).map_err(|_| efi::LOAD_ERROR)?;
    let buf = efi.allocate(size)?;
    let data = &mut buf[..size];
    if let Err(s) = f.read_exact(data) {
        efi.free(data);
        return Err(s);
    }
    Ok(data)
}

fn read_text(efi: &Efi, dir: &Dir<'_>, path: &'static str) -> Result<&'static str, Fatal> {
    let size = dir
        .open(path)
        .and_then(|f| f.size())
        .map_err(|s| Fatal::Read(path, s))?;
    if size > TEXT_MAX {
        return Err(Fatal::Read(path, efi::BUFFER_TOO_SMALL));
    }
    let data = read_file(efi, dir, path).map_err(|s| Fatal::Read(path, s))?;
    core::str::from_utf8(data).map_err(|_| Fatal::NotText(path))
}

/// The partition with the menu file, and the menu with its includes and environment block read.
fn load_menu<'e>(efi: &'e Efi) -> Result<(Dir<'e>, Handle, Config<'static>), Fatal> {
    let mut handles: [Handle; MAX_VOLUMES] = [null_mut(); MAX_VOLUMES];
    let n = efi.volumes(&mut handles);
    let (dir, volume, text) = handles[..n]
        .iter()
        .find_map(|&h| {
            let dir = efi.open_volume(h).ok()?;
            let text = read_text(efi, &dir, MENU_FILE).ok()?;
            Some((dir, h, text))
        })
        .ok_or(Fatal::NoMenu)?;
    let mut cfg = Config::parse(text).map_err(|e| Fatal::Config(MENU_FILE, e))?;
    let includes = cfg.include;
    for &p in includes.as_slice() {
        if dir.exists(p) {
            let t = read_text(efi, &dir, p)?;
            cfg.read(t).map_err(|e| Fatal::Config(p, e))?;
        }
    }
    if let Some(p) = cfg.env.filter(|p| dir.exists(p)) {
        let t = read_text(efi, &dir, p)?;
        for (k, v) in env_vars(t).into_iter().flatten() {
            // An empty value keeps the menu file's default (grub.cfg's `if [ -z "$x" ]; then set x=...`). A full
            // variable table only loses the environment's extra names.
            if !v.is_empty() {
                let _ = cfg.vars.set(k, v);
            }
        }
    }
    cfg.check().map_err(|e| Fatal::Config(MENU_FILE, e))?;
    Ok((dir, volume, cfg))
}

fn screen<'e>(efi: &'e Efi, cfg: &Config<'_>) -> Screen<'e> {
    let Some(display) = efi.display(cfg.modes()) else {
        return Screen::Text;
    };
    let (w, h) = (display.width, display.height);
    let canvas = efi
        .allocate_pixels(w as usize * h as usize)
        .ok()
        .and_then(|px| Canvas::new(px, w, h));
    match canvas {
        Some(canvas) => Screen::Graphics {
            display,
            canvas,
            layout: Layout::new(w, h),
        },
        None => Screen::Text,
    }
}

fn key(k: KeyPress) -> Key {
    match (k.scan, k.ch) {
        (0x01, _) | (0, 0x10) => Key::Up,           // up, Ctrl-P
        (0x02, _) | (0, 0x0e) => Key::Down,         // down, Ctrl-N
        (0x03, _) | (0, 0x0a | 0x0d) => Key::Enter, // right, Enter
        (0x05, _) => Key::Home,
        (0x06, _) => Key::End,
        (0x09, _) => Key::PageUp,
        (0x0a, _) => Key::PageDown,
        _ => Key::Other,
    }
}

fn show_menu(efi: &Efi, cfg: &Config<'_>, shown: &Shown, screen: &mut Screen<'_>, m: &Menu) {
    let titles = shown.entries().map(|i| cfg.entries()[i].title);
    match screen {
        Screen::Graphics {
            display,
            canvas,
            layout,
        } => {
            draw::menu(canvas, layout, cfg.title, cfg.subtitle, titles, m);
            display.show(canvas.pixels());
        }
        Screen::Text => {
            efi.clear_console();
            let mut o = Con(efi);
            let _ = writeln!(o, "\n  {}  {}\n", cfg.title, cfg.subtitle);
            for (n, t) in titles.enumerate() {
                let mark = if n == m.selected { '*' } else { ' ' };
                let _ = writeln!(o, "  {mark} {t}");
            }
            if let Some(s) = m.seconds_left() {
                let _ = writeln!(o, "\n  Starting in {s} s. Arrow keys and Enter to choose.");
            }
        }
    }
}

/// Shows the menu until an entry is chosen; returns its position in `shown`.
fn choose(
    efi: &Efi,
    cfg: &Config<'_>,
    shown: &Shown,
    screen: &mut Screen<'_>,
    m: &mut Menu,
) -> usize {
    show_menu(efi, cfg, shown, screen, m);
    loop {
        let step = match efi.read_key() {
            Some(k) => m.key(key(k)),
            None => {
                efi.stall_ms(TICK_MS);
                m.tick(TICK_MS)
            }
        };
        match step {
            Step::Wait => {}
            Step::Redraw => show_menu(efi, cfg, shown, screen, m),
            Step::Boot(n) => return n,
        }
    }
}

/// Lines of text on the screen: what an entry prints while it loads, or why it failed. They also go
/// to the firmware console, as GRUB's `echo` does: on the serial port that is the only record (the
/// QEMU tests read it), and on the screen the firmware writes them at the top left, where the EFI
/// stub's own lines come next.
fn show_lines(efi: &Efi, cfg: &Config<'_>, screen: &mut Screen<'_>, lines: &[&str]) {
    if let Screen::Graphics {
        display,
        canvas,
        layout,
    } = screen
    {
        draw::message(canvas, layout, cfg.title, lines);
        display.show(canvas.pixels());
    }
    for l in lines {
        efi.print(l);
        efi.print("\n");
    }
}

/// What a started entry left behind; undone when the kernel returns or something fails before it starts.
struct Staged {
    initrd: Option<(Handle, &'static mut [u8])>,
    old_dtb: Option<*const core::ffi::c_void>,
    dtb: Option<&'static mut [u8]>,
    image: Option<Handle>,
}

impl Staged {
    fn undo(self, efi: &Efi) {
        if let Some((h, data)) = self.initrd {
            efi.uninstall_initrd(h);
            efi.free(data);
        }
        if let Some(dtb) = self.dtb {
            let _ = efi.install_config_table(&DTB_TABLE, self.old_dtb.unwrap_or(core::ptr::null()));
            efi.free(dtb);
        }
        if let Some(child) = self.image {
            efi.unload_image(child);
        }
    }
}

/// GRUB's `save_env`: writes `items` (`name=value`, the values expanded with `vars` first) into the menu's `env`
/// block, rewriting its 1 KiB in place. Nothing happens when the menu names no block or the file is missing.
// No heap to box into: the reason is a fixed line buffer.
#[allow(clippy::result_large_err)]
fn save_env(
    efi: &Efi,
    dir: &Dir<'_>,
    cfg: &Config<'_>,
    vars: &Vars<'_>,
    items: &[&str],
) -> Result<(), Line> {
    let mut err = Line::new();
    let Some(p) = cfg.env.filter(|p| !items.is_empty() && dir.exists(p)) else {
        return Ok(());
    };
    let mut bufs = [[0u8; 256]; MAX_SAVES];
    let mut updates = [("", ""); MAX_SAVES];
    for ((u, b), item) in updates.iter_mut().zip(bufs.iter_mut()).zip(items) {
        let (k, v) = item.split_once('=').unwrap_or((item, ""));
        let Ok(v) = expand(v, vars, b) else {
            let _ = write!(err, "{p}: the value of {k} is too long");
            return Err(err);
        };
        *u = (k, v);
    }
    let old = match dir.open(p).and_then(|f| f.size()) {
        Ok(size) if size <= TEXT_MAX => read_file(efi, dir, p),
        Ok(_) => Err(efi::BUFFER_TOO_SMALL),
        Err(s) => Err(s),
    };
    let old = match old {
        Ok(old) => old,
        Err(s) => {
            let _ = write!(err, "{p}: {}", status_name(s));
            return Err(err);
        }
    };
    let mut out = [0u8; ENV_BLOCK_SIZE];
    let updated = env_update(
        core::str::from_utf8(old).unwrap_or(""),
        &updates[..items.len()],
        &mut out,
    );
    efi.free(old);
    if updated.is_err() {
        let _ = write!(err, "{p}: the environment block is full");
        return Err(err);
    }
    dir.overwrite(p, &out).map_err(|s| {
        let _ = write!(err, "{p}: {}", status_name(s));
        err
    })
}

/// A string that stays for the loader's life, for the menu's variables.
fn keep(efi: &Efi, s: &str) -> &'static str {
    match efi.allocate(s.len()) {
        Ok(page) => {
            let data = &mut page[..s.len()];
            data.copy_from_slice(s.as_bytes());
            core::str::from_utf8(data).unwrap_or("")
        }
        Err(_) => "",
    }
}

/// grub.cfg's update slots: when `<slots>/a` or `<slots>/b` holds a kernel, chooses one and sets the variables the
/// entries use. A trial is cleared in the environment block and recorded in `lucina_tried` first, and booted only if
/// that write reads back, so a reset of the trial kernel always goes home.
fn choose_slot(efi: &Efi, dir: &Dir<'_>, volume: Handle, cfg: &mut Config<'static>) {
    let Some(root) = cfg.slots else {
        return;
    };
    let kernel_in = |slot: &str| {
        let mut p = Line::new();
        let _ = write!(p, "{root}/{slot}/vmlinuz");
        dir.exists(p.as_str())
    };
    if !kernel_in("a") && !kernel_in("b") {
        return;
    }
    let trial = trial_slot(cfg.vars.get("lucina_trial")).filter(|t| {
        let tried = if *t == "a" {
            "lucina_tried=a"
        } else {
            "lucina_tried=b"
        };
        let items = ["lucina_trial=", tried];
        let cleared = save_env(efi, dir, cfg, &cfg.vars, &items).is_ok();
        cleared
            && cfg.env.is_some_and(|p| {
                read_text(efi, dir, p).is_ok_and(|block| env_get(block, "lucina_trial") == Some(""))
            })
    });
    let boot = slot_boot(cfg.vars.get("lucina_slot"), trial, kernel_in);
    let mut id = [0u8; 9];
    let block = efi.first_block(volume);
    let esp = match &block {
        Ok(b) => fat_volume_id(b, &mut id).unwrap_or(""),
        Err(_) => "",
    };
    let mut d = Line::new();
    let _ = write!(d, "{root}/{}", boot.slot);
    let mut args = Line::new();
    let _ = write!(
        args,
        "lucinaos.slot={} lucinaos.boot={} lucinaos.esp={esp}",
        boot.slot,
        boot.kind.word()
    );
    if boot.kind == SlotKind::Trial {
        // A trial kernel that panics restarts by itself, into home.
        let _ = write!(args, " panic=10");
    }
    let vars = [
        ("lucina_boot", boot.slot),
        ("lucina_other", boot.other),
        ("lucina_kind", boot.kind.word()),
        ("lucina_dir", keep(efi, d.as_str())),
        ("lucina_esp", keep(efi, esp)),
        ("slot_args", keep(efi, args.as_str())),
        (
            "lucina_title",
            if boot.kind == SlotKind::Trial {
                ", trying an update"
            } else {
                ""
            },
        ),
    ];
    for (k, v) in vars {
        // Only a menu file with over a hundred variables fills the table.
        let _ = cfg.vars.set(k, v);
    }
    if let Ok(b) = block {
        efi.free(b);
    }
}

/// Titles may name variables (`$lucina_title`).
fn expand_titles(efi: &Efi, cfg: &mut Config<'static>) {
    for i in 0..cfg.entries().len() {
        let title = cfg.entries()[i].title;
        if title.contains('$') {
            let mut buf = [0u8; 160];
            if let Ok(t) = expand(title, &cfg.vars, &mut buf) {
                let t = keep(efi, t);
                cfg.set_title(i, t);
            }
        }
    }
}

/// What happened to a chosen entry that did not boot.
enum Outcome {
    /// It failed, for this reason.
    Failed(Line),
    /// It did what it does and the menu comes back (`action = "message"`).
    Back,
}

/// Waits `s` seconds or until a key; 0 waits for a key.
fn wait(efi: &Efi, s: u32) {
    let mut left = u64::from(s) * 1000;
    while s == 0 || left > 0 {
        if efi.read_key().is_some() {
            return;
        }
        efi.stall_ms(TICK_MS);
        left = left.saturating_sub(TICK_MS);
    }
}

/// Starts an entry. Returns only when it does not boot.
fn start(
    efi: &Efi,
    dir: &Dir<'_>,
    cfg: &Config<'_>,
    e: &Entry<'_>,
    screen: &mut Screen<'_>,
) -> Outcome {
    let mut err = Line::new();
    let mut gbuf = [0u8; 64];
    let group = expand(e.with.unwrap_or(""), &cfg.vars, &mut gbuf).unwrap_or("");
    let vars = match cfg.vars.scoped(group) {
        Ok(v) => v,
        Err(why) => {
            let _ = write!(err, "{why}");
            return Outcome::Failed(err);
        }
    };
    if let Screen::Text = screen {
        let _ = writeln!(Con(efi), "  Booting `{}'\n", e.title);
    }
    if let Err(why) = save_env(efi, dir, cfg, &vars, e.save.as_slice()) {
        return Outcome::Failed(why);
    }
    let mut bufs = [[0u8; 160]; MAX_LINES];
    let mut lines = [""; MAX_LINES];
    let message = e.message.as_slice();
    for ((l, b), m) in lines.iter_mut().zip(bufs.iter_mut()).zip(message) {
        *l = expand(m, &vars, b).unwrap_or(m);
    }
    if !message.is_empty() {
        show_lines(efi, cfg, screen, &lines[..message.len()]);
    }
    match e.action {
        Action::Linux => {}
        Action::Message => {
            wait(efi, e.wait);
            return Outcome::Back;
        }
        Action::FirmwareSetup => {
            let supported = efi.global_u64("OsIndicationsSupported").unwrap_or(0);
            if supported & BOOT_TO_FW_UI == 0 {
                let _ = write!(
                    err,
                    "this firmware cannot be asked to open its setup screen"
                );
                return Outcome::Failed(err);
            }
            let now = efi.global_u64("OsIndications").unwrap_or(0);
            if let Err(s) = efi.set_global_u64("OsIndications", now | BOOT_TO_FW_UI) {
                let _ = write!(err, "OsIndications: {}", status_name(s));
                return Outcome::Failed(err);
            }
            efi.reset_cold()
        }
    }
    let mut staged = Staged {
        initrd: None,
        old_dtb: efi.config_table(&DTB_TABLE),
        dtb: None,
        image: None,
    };
    if let Err(()) = stage_linux(efi, dir, &vars, e, &mut staged, &mut err) {
        staged.undo(efi);
        return Outcome::Failed(err);
    }
    let s = efi.start_image(staged.image.unwrap_or(null_mut()));
    let _ = write!(err, "the kernel returned {}", status_name(s));
    staged.undo(efi);
    Outcome::Failed(err)
}

fn stage_linux(
    efi: &Efi,
    dir: &Dir<'_>,
    vars: &Vars<'_>,
    e: &Entry<'_>,
    staged: &mut Staged,
    err: &mut Line,
) -> Result<(), ()> {
    let mut fail = |args: fmt::Arguments<'_>| {
        let _ = err.write_fmt(args);
    };
    let mut path = [0u8; PATH_MAX];

    // linux
    let Ok(kpath) = expand(e.linux, vars, &mut path) else {
        fail(format_args!("the kernel path is too long"));
        return Err(());
    };
    let kernel = read_file(efi, dir, kpath)
        .map_err(|s| fail(format_args!("{kpath}: {}", status_name(s))))?;
    let kind = match &kernel[..] {
        [0x4d, 0x5a, ..] => None,
        [0x1f, 0x8b, ..] => {
            Some("gzip-compressed; LucinaLoader starts EFI-stub kernels (vmlinuz as built)")
        }
        _ => Some("not an EFI-stub kernel (no PE header)"),
    };
    if let Some(why) = kind {
        efi.free(kernel);
        fail(format_args!("{kpath}: {why}"));
        return Err(());
    }
    let mut cmdline = [0u8; CMDLINE_MAX];
    let Ok(cmdline) = kernel_cmdline(e, vars, &mut cmdline) else {
        efi.free(kernel);
        fail(format_args!(
            "the command line is longer than {CMDLINE_MAX} bytes"
        ));
        return Err(());
    };
    let loaded = efi.load_image(kernel);
    efi.free(kernel);
    let child = loaded.map_err(|s| fail(format_args!("{kpath}: LoadImage: {}", status_name(s))))?;
    staged.image = Some(child);
    let options = efi
        .allocate_u16(cmdline.len() + 1)
        .map_err(|s| fail(format_args!("command line: {}", status_name(s))))?;
    for (o, b) in options.iter_mut().zip(cmdline.bytes()) {
        *o = u16::from(b);
    }
    efi.set_load_options(child, options)
        .map_err(|s| fail(format_args!("load options: {}", status_name(s))))?;

    // devicetree
    if let Some(t) = e.devicetree {
        let Ok(dpath) = expand(t, vars, &mut path) else {
            fail(format_args!("the device tree path is too long"));
            return Err(());
        };
        if dpath.is_empty() {
            // `devicetree = "$dtb"` with an empty $dtb: none (the QEMU test uses QEMU's own tree).
            return stage_initrd(efi, dir, vars, e, staged, err);
        }
        let raw = read_file(efi, dir, dpath)
            .map_err(|s| fail(format_args!("{dpath}: {}", status_name(s))))?;
        let len = raw.len();
        let valid = Fdt::new(raw).map(|f| f.total_size()).ok();
        let dtb = efi.allocate(len + DTB_EXTRA);
        match (valid, dtb) {
            (Some(total), Ok(dtb)) if total <= len => {
                dtb[..total].copy_from_slice(&raw[..total]);
                // As GRUB's `devicetree`: the tree's own totalsize grows by the free space after it.
                let grown = (total + DTB_EXTRA) as u32;
                dtb[4..8].copy_from_slice(&grown.to_be_bytes());
                efi.free(raw);
                let table = dtb.as_ptr() as *const core::ffi::c_void;
                staged.dtb = Some(dtb);
                efi.install_config_table(&DTB_TABLE, table).map_err(|s| {
                    fail(format_args!(
                        "{dpath}: InstallConfigurationTable: {}",
                        status_name(s)
                    ))
                })?;
            }
            (_, dtb) => {
                if let Ok(d) = dtb {
                    efi.free(d);
                }
                efi.free(raw);
                fail(format_args!("{dpath}: not a valid device tree"));
                return Err(());
            }
        }
    }
    // Without `devicetree` the firmware's own tree (if any) stays installed as it is; the EFI stub adds `/chosen`
    // to whichever tree it gets (libstub `update_fdt`), which is what GRUB's copy of the tree was for.

    stage_initrd(efi, dir, vars, e, staged, err)
}

/// The initrd files (those whose path is not empty after expansion), concatenated and published through LoadFile2.
fn stage_initrd(
    efi: &Efi,
    dir: &Dir<'_>,
    vars: &Vars<'_>,
    e: &Entry<'_>,
    staged: &mut Staged,
    err: &mut Line,
) -> Result<(), ()> {
    let mut fail = |args: fmt::Arguments<'_>| {
        let _ = err.write_fmt(args);
    };
    let mut path = [0u8; PATH_MAX];
    let mut names = [""; MAX_INITRDS];
    let mut n = 0;
    for &t in e.initrd.as_slice() {
        if !matches!(expand(t, vars, &mut path), Ok("")) {
            names[n] = t;
            n += 1;
        }
    }
    let files = &names[..n];
    if !files.is_empty() {
        let mut sizes = [0u64; MAX_INITRDS];
        for (s, &t) in sizes.iter_mut().zip(files) {
            let Ok(p) = expand(t, vars, &mut path) else {
                fail(format_args!("an initrd path is too long"));
                return Err(());
            };
            *s = dir
                .open(p)
                .and_then(|f| f.size())
                .map_err(|st| fail(format_args!("{p}: {}", status_name(st))))?;
        }
        let mut offsets = [0u64; MAX_INITRDS];
        let total = initrd_layout(&sizes[..n], &mut offsets[..n]).unwrap_or(u64::MAX);
        let buf = usize::try_from(total)
            .map_err(|_| efi::LOAD_ERROR)
            .and_then(|t| efi.allocate(t).map(|b| &mut b[..t]))
            .map_err(|s| fail(format_args!("initrd: {}", status_name(s))))?;
        for (i, &t) in files.iter().enumerate() {
            let p = expand(t, vars, &mut path).unwrap_or("");
            let (at, len) = (offsets[i] as usize, sizes[i] as usize);
            let r = dir
                .open(p)
                .and_then(|f| f.read_exact(&mut buf[at..at + len]));
            if let Err(s) = r {
                efi.free(buf);
                fail(format_args!("{p}: {}", status_name(s)));
                return Err(());
            }
        }
        match efi.install_initrd(buf) {
            Ok(installed) => staged.initrd = Some(installed),
            Err((s, buf)) => {
                efi.free(buf);
                fail(format_args!("initrd LoadFile2: {}", status_name(s)));
                return Err(());
            }
        }
    }
    Ok(())
}

fn wait_key(efi: &Efi) {
    loop {
        if efi.read_key().is_some() {
            return;
        }
        efi.stall_ms(TICK_MS);
    }
}

fn run(efi: &Efi) -> Fatal {
    let (dir, volume, mut cfg) = match load_menu(efi) {
        Ok(x) => x,
        Err(f) => return f,
    };
    choose_slot(efi, &dir, volume, &mut cfg);
    expand_titles(efi, &mut cfg);
    let vars = cfg.vars;
    let shown = cfg.resolve(|p| {
        let mut buf = [0u8; PATH_MAX];
        expand(p, &vars, &mut buf).is_ok_and(|p| dir.exists(p))
    });
    if shown.is_empty() {
        return Fatal::Empty;
    }
    let mut screen = screen(efi, &cfg);
    if let Screen::Text = screen {
        let _ = writeln!(
            Con(efi),
            "LucinaLoader {VERSION}: {MENU_FILE}, {} entries",
            shown.len()
        );
    }
    // grub.cfg's breadcrumb (`lucina_mark menu`); like GRUB, a failed write does not stop the menu.
    let _ = save_env(efi, &dir, &cfg, &cfg.vars, cfg.save.as_slice());
    // A key still pressed from the firmware's boot menu would stop the countdown.
    while efi.read_key().is_some() {}
    let mut m = Menu::new(shown.len(), shown.default, shown.timeout);
    let mut pick = m.immediate().then_some(m.selected);
    loop {
        let n = match pick.take() {
            Some(n) => n,
            None => choose(efi, &cfg, &shown, &mut screen, &mut m),
        };
        let e = &cfg.entries()[shown.entry(n)];
        let why = match start(efi, &dir, &cfg, e, &mut screen) {
            Outcome::Failed(why) => why,
            Outcome::Back => {
                m.key(Key::Other);
                continue;
            }
        };
        let mut head = Line::new();
        let _ = write!(head, "{}:", e.title);
        show_lines(
            efi,
            &cfg,
            &mut screen,
            &[
                head.as_str(),
                why.as_str(),
                "",
                "Press any key to return to the menu.",
            ],
        );
        wait_key(efi);
        // The countdown does not start again.
        m.key(Key::Other);
    }
}

#[unsafe(no_mangle)]
extern "efiapi" fn efi_main(image: Handle, st: *mut efi::SystemTable) -> Status {
    // SAFETY: the firmware passes our image handle and its live system table to the entry point.
    let efi = unsafe { Efi::new(image, st) };
    efi.disable_watchdog();
    let why = run(&efi);
    let _ = writeln!(Con(&efi), "LucinaLoader {VERSION}: {why}");
    efi.stall_ms(NO_MENU_WAIT_MS);
    efi::LOAD_ERROR
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    let _ = writeln!(efi::PanicConsole, "LucinaLoader: panic: {info}");
    loop {
        core::hint::spin_loop();
    }
}
