//! LucinaOS ARMv7-A kernel, Phase 0: the arch layer an OSTD `armv7` port needs, booted on the Kindle
//! Paperwhite 3 (i.MX6SL, Cortex-A9) through lucina-kexec and on QEMU `sabrelite` (i.MX6Q, Cortex-A9) for CI.
//!
//! It brings up the MMU (short descriptors, 3G/1G split), the exception vectors, the GIC, the GPT tick and the
//! watchdog, runs self-tests of each, and then proves it is alive: on the PW3 it draws "Lucina" on the e-ink
//! panel through a minimal EPDC + MAX77696 path; on QEMU it renders the same screen into RAM and prints it.
//! Afterwards it resets the SoC through the watchdog (`lucina.reset=<s>` on the command line, 0 = stay).

#![no_std]
#![no_main]

mod arch;
mod drivers;
mod gfx;

use arch::{cpu, gic, mm, serial, timer, trap};
use core::sync::atomic::{AtomicU32, AtomicU8, Ordering};
use drivers::{ccm, epdc, i2c::I2c, max77696::Pmic, wdog};
use lucina_fdt::Fdt;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
enum Board {
    /// Kindle Paperwhite 3 (Amazon "wario" base board, i.MX6SL, DRAM at 0x8000_0000).
    Pw3 = 1,
    /// QEMU `-M sabrelite` (i.MX6Q, DRAM at 0x1000_0000).
    Sabrelite = 2,
}

static BOARD: AtomicU8 = AtomicU8::new(0);
static RESET_SECS: AtomicU32 = AtomicU32::new(0);

const DTB_MAX: usize = 128 << 10;
static mut DTB_COPY: [u8; DTB_MAX] = [0; DTB_MAX];

const DEFAULT_RESET_PW3: u32 = 90;
const DEFAULT_RESET_QEMU: u32 = 1;

fn board() -> Board {
    if BOARD.load(Ordering::Relaxed) == Board::Pw3 as u8 { Board::Pw3 } else { Board::Sabrelite }
}

/// Stop: report, keep the watchdog fed for twice the reset delay (so a failure is told apart from a success by
/// how long the device stays away), then reset. QEMU resets at once.
pub fn fatal(why: &str) -> ! {
    cpu::disable_irq();
    println!("LUCINA-ARMV7: FAIL {}", why);
    let secs = match board() {
        Board::Pw3 => RESET_SECS.load(Ordering::Relaxed).saturating_mul(2),
        Board::Sabrelite => 0,
    };
    if secs == 0 && board() == Board::Pw3 {
        cpu::halt();
    }
    for _ in 0..secs * 10 {
        timer::delay_ms(100);
        wdog::service();
    }
    wdog::reset()
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("panic: {}", info.message());
    if let Some(l) = info.location() {
        println!("    at {}:{}", l.file(), l.line());
    }
    fatal("panic")
}

/// Copy the DTB out of the way (QEMU and kexec put it in memory this kernel will use) and parse it.
fn take_dtb(dtb_pa: usize) -> Option<Fdt<'static>> {
    if !mm::is_linear(dtb_pa, 40) {
        return None;
    }
    let src = mm::phys_to_virt(dtb_pa) as *const u8;
    // SAFETY: inside the linear map (checked above); the header is 40 bytes.
    let head = unsafe { core::slice::from_raw_parts(src, 40) };
    if head[..4] != lucina_fdt::MAGIC.to_be_bytes() {
        return None;
    }
    let size = u32::from_be_bytes([head[4], head[5], head[6], head[7]]) as usize;
    if size > DTB_MAX || !mm::is_linear(dtb_pa, size) {
        println!("dtb: {} bytes, too big to keep", size);
        return None;
    }
    // SAFETY: DTB_COPY is written once, here, before anything reads it.
    let dst = unsafe { &mut *core::ptr::addr_of_mut!(DTB_COPY) };
    // SAFETY: source range checked above.
    dst[..size].copy_from_slice(unsafe { core::slice::from_raw_parts(src, size) });
    match Fdt::new(&dst[..size]) {
        Ok(f) => Some(f),
        Err(e) => {
            println!("dtb: invalid ({:?})", e);
            None
        }
    }
}

fn detect(fdt: Option<&Fdt>, dram: usize) -> Board {
    if let Some(f) = fdt {
        if let Ok(root) = f.root() {
            if let Ok(Some(c)) = root.property("compatible") {
                for s in c.strings().flatten() {
                    match s {
                        "amazon,wario" | "fsl,imx6sl" => return Board::Pw3,
                        "fsl,imx6q-sabrelite" | "fsl,imx6q" => return Board::Sabrelite,
                        _ => {}
                    }
                }
            }
        }
    }
    if dram == 0x8000_0000 { Board::Pw3 } else { Board::Sabrelite }
}

fn bootarg_u32(fdt: Option<&Fdt>, key: &str) -> Option<u32> {
    let args = fdt?.chosen().ok()??.bootargs?;
    args.split_ascii_whitespace()
        .find_map(|w| w.strip_prefix(key)?.strip_prefix('='))
        .and_then(|v| v.parse().ok())
}

/// The blob lucina-kexec (`--initrd`) or QEMU (`-initrd`) left in memory, from `/chosen`.
fn initrd(fdt: Option<&Fdt>) -> Option<&'static [u8]> {
    let (s, e) = fdt?.chosen().ok()??.initrd?;
    let (s, e) = (s as usize, e as usize);
    if e <= s || !mm::is_linear(s, e - s) {
        return None;
    }
    // SAFETY: inside the linear map and not used by this kernel (above the DMA pool on both boards).
    Some(unsafe { core::slice::from_raw_parts(mm::phys_to_virt(s) as *const u8, e - s) })
}

#[unsafe(no_mangle)]
extern "C" fn kernel_main(dtb_pa: usize, dram: usize, load_pa: usize) -> ! {
    serial::init();
    println!();
    println!("Lucina Minerva (LucinaOS), ARMv7 phase 0");
    println!("  midr {:#010x} cbar {:#010x} sctlr {:#010x} cpsr {:#x}", cpu::midr(), cpu::cbar(), cpu::sctlr(), cpu::cpsr());
    println!("  dram {:#010x}, loaded at {:#010x}, r2 {:#010x}", dram, load_pa, dtb_pa);

    mm::init(dram);
    let fdt = take_dtb(dtb_pa);
    let b = detect(fdt.as_ref(), dram);
    BOARD.store(b as u8, Ordering::Relaxed);
    let reset = bootarg_u32(fdt.as_ref(), "lucina.reset").unwrap_or(match b {
        Board::Pw3 => DEFAULT_RESET_PW3,
        Board::Sabrelite => DEFAULT_RESET_QEMU,
    });
    RESET_SECS.store(reset, Ordering::Relaxed);
    let model = fdt
        .as_ref()
        .and_then(|f| f.property("/", "model").ok().flatten())
        .and_then(|p| p.as_str().ok())
        .unwrap_or("(no device tree)");
    println!("  board {:?}: {}; reset after {} s", b, model, reset);

    // Gates for what this kernel drives (the previous OS may have left them off).
    ccm::ungate(ccm::CCGR1, 20); // gpt
    ccm::ungate(ccm::CCGR1, 22); // gpt_serial
    ccm::ungate(ccm::CCGR5, 24); // uart
    ccm::ungate(ccm::CCGR5, 26); // uart_serial

    // The boot tables were written with caches off, so a live PL310 could hold stale lines for them. Linux's
    // single-CPU kexec (`_soft_restart`) and U-Boot leave it disabled; refuse to go on otherwise.
    const PL310_CTRL: usize = 0x00a0_2100;
    let l2_on = arch::Mmio::at(PL310_CTRL).read(0) & 1 != 0;
    println!("l2c: PL310 {}", if l2_on { "ENABLED" } else { "off" });
    if l2_on {
        fatal("PL310 L2 cache left enabled by the previous kernel");
    }

    match mm::self_test() {
        Ok(()) => println!("mm: short-descriptor L1+L2 walk ok (linear {:#x}, vmalloc page {:#x})",
            mm::section_attrs(mm::KERNEL_VBASE), mm::VMALLOC_BASE),
        Err(e) => fatal(e),
    }
    if !trap::self_test() {
        fatal("undefined-instruction trap did not resume");
    }
    println!("trap: undefined instruction caught and resumed");

    let periph = gic::init();
    println!("gic: periphbase {:#010x}, {} lines", periph, gic::lines());
    let wd = wdog::init();
    println!("wdog: {}", if wd { "running, serviced from the tick" } else { "not started" });
    timer::init();
    cpu::enable_irq();
    let ok = timer::poll_until(1000, || timer::ticks() >= 20);
    println!("timer: {} ticks in {} counts (32768 Hz), {}", timer::ticks(), timer::now(), if ok { "ok" } else { "NO IRQ" });
    if !ok {
        fatal("no timer interrupt");
    }

    match b {
        Board::Pw3 => pw3(fdt.as_ref()),
        Board::Sabrelite => qemu(fdt.as_ref()),
    }

    println!("LUCINA-ARMV7: PASS");
    if reset == 0 {
        println!("staying up (lucina.reset=0)");
        loop {
            cpu::wfi();
        }
    }
    let deadline = timer::ticks() + reset * timer::HZ;
    while timer::ticks() < deadline {
        cpu::wfi();
    }
    println!("reset through the watchdog");
    wdog::reset()
}

/// The screen both boards render.
fn draw_screen(c: &mut gfx::Canvas, title_y: usize) {
    c.fill(gfx::WHITE);
    let (lw, lh) = c.logical_size();
    c.rect(40, 40, lw - 80, 8, gfx::BLACK);
    c.rect(40, lh - 48, lw - 80, 8, gfx::BLACK);
    c.text_centred(title_y, 24, "Lucina", gfx::BLACK);
    c.text_centred(title_y + 300, 8, "Lucina Minerva", gfx::BLACK);
    c.text_centred(title_y + 420, 8, "Phase 0 - PW3", gfx::BLACK);
}

const TITLE_Y: usize = 400;
/// A multiple of 8: the clock line is a partial update, and its native x (= logical y) stays 8-pixel aligned.
const CLOCK_Y: usize = 1096;

fn clock_line(c: &mut gfx::Canvas, secs: u32) -> (usize, usize, usize, usize) {
    let (lw, _) = c.logical_size();
    let (x, y, w, h) = (40, CLOCK_Y, lw - 80, 7 * 8);
    c.rect(x, y, w, h, gfx::WHITE);
    let mut d = [0u8; 10];
    let n = gfx::fmt_u32(secs, &mut d);
    let mut line = [0u8; 32];
    let parts = ["t = ", n, " s"];
    let mut len = 0;
    for p in parts {
        line[len..len + p.len()].copy_from_slice(p.as_bytes());
        len += p.len();
    }
    c.text_centred(y, 8, core::str::from_utf8(&line[..len]).unwrap_or(""), gfx::BLACK);
    c.native_rect(x, y, w, h)
}

fn pw3(fdt: Option<&Fdt>) {
    let clk = ccm::epdc_on();
    println!("ccm: epdc axi_sel {} pix_sel {}, parents {}, DISPLAY domain {}",
        clk.axi_sel, clk.pix_sel, if clk.parents_ok { "on" } else { "NOT LOCKED" },
        if clk.domain_ok { "up" } else { "TIMEOUT" });
    ccm::ungate(ccm::CCGR2, 6); // i2c1
    let bus = I2c::i2c1();
    bus.init();
    let pmic = Pmic { bus: &bus };
    match pmic.epdcnfg() {
        Ok(v) => println!("max77696: EPDCNFG {:#04x}", v),
        Err(e) => {
            println!("max77696: {:?}", e);
            fatal("PMIC not answering on I2C1");
        }
    }
    let Some(fw) = initrd(fdt) else { fatal("no waveform: pass epdc.fw with lucina-kexec --initrd") };
    let e = match epdc::Epdc::init(fw, 20) {
        Ok(e) => e,
        Err(err) => {
            println!("epdc: {:?}", err);
            fatal("EPDC init")
        }
    };
    println!("epdc: rev {}, waveform {} bytes, fw[16] {:#x}, {} temperature ranges, index {}, {}",
        e.rev, e.info.data_len, e.info.mode_version, e.info.trt_entries, e.info.temp_index,
        if e.info.five_bit { "5-bit" } else { "4-bit" });

    let mut c = gfx::Canvas { buf: e.update_buffer(), w: epdc::WIDTH, h: epdc::HEIGHT, portrait: true };
    draw_screen(&mut c, TITLE_Y);
    clock_line(&mut c, timer::ticks() / timer::HZ);

    let st = match pmic.epd_on() {
        Ok(s) => s,
        Err(err) => {
            println!("max77696: {:?}", err);
            fatal("EPD power-up")
        }
    };
    println!("max77696: vcom code {}{}, power good {}, faults {:#04x}", st.vcom_code,
        if st.vcom_written { " (written)" } else { "" }, st.power_good, st.faults);
    for (name, mode, fixed) in [("INIT", epdc::MODE_INIT, Some(0xff)), ("GC16", epdc::MODE_GC16, None)] {
        match e.update(0, 0, epdc::WIDTH, epdc::HEIGHT, mode, true, fixed) {
            Ok(t) => println!("epdc: {} full screen, wb {} us, done {} us{}", name, t.wb_us, t.total_us,
                if t.collision { ", collision" } else { "" }),
            Err(err) => {
                let _ = pmic.epd_off();
                println!("epdc: {} {:?}", name, err);
                fatal("EPDC update")
            }
        }
    }
    let _ = pmic.epd_off();

    // A second, partial update a little later shows the tick kept running on the device.
    let until = timer::ticks() + 10 * timer::HZ;
    while timer::ticks() < until {
        cpu::wfi();
    }
    let (x, y, w, h) = clock_line(&mut c, timer::ticks() / timer::HZ);
    if pmic.epd_on().is_ok() {
        match e.update(x, y, w, h, epdc::MODE_GC16, false, None) {
            Ok(t) => println!("epdc: clock line, done {} us", t.total_us),
            Err(err) => println!("epdc: clock line {:?}", err),
        }
        let _ = pmic.epd_off();
    }
    e.clock_gate();
    ccm::epdc_off();
}

fn qemu(fdt: Option<&Fdt>) {
    // The waveform parser runs on whatever -initrd the test passed (a synthetic header, never Amazon's file).
    if let Some(fw) = initrd(fdt) {
        match epdc::parse_waveform(fw, 20) {
            Ok(i) => println!("waveform: {} ranges, index {} at 20 C, {}, data {} bytes", i.trt_entries,
                i.temp_index, if i.five_bit { "5-bit" } else { "4-bit" }, i.data_len),
            Err(e) => println!("waveform: {:?}", e),
        }
    }
    let buf = epdc::Buffers::new().update_buffer();
    let mut c = gfx::Canvas { buf, w: epdc::WIDTH, h: epdc::HEIGHT, portrait: true };
    draw_screen(&mut c, TITLE_Y);
    clock_line(&mut c, 0);
    // Sample the title back through the rotation: one character per font pixel.
    let s = 24;
    let tw = gfx::Canvas::text_width("Lucina", s);
    let x0 = (c.logical_size().0 - tw) / 2;
    for row in 0..7 {
        let y = TITLE_Y + row * s + s / 2;
        let mut line = [b' '; 40];
        for (i, ch) in line.iter_mut().enumerate().take(tw / s + 1) {
            let x = x0 + i * s + s / 2;
            let (nx, ny) = (y, c.h - 1 - x);
            *ch = if c.buf[ny * c.w + nx] == gfx::BLACK { b'#' } else { b'.' };
        }
        println!("  {}", core::str::from_utf8(&line[..tw / s + 1]).unwrap_or(""));
    }
    println!("canvas: {}x{} portrait on {}x{} native, fnv1a {:#010x}", c.logical_size().0, c.logical_size().1,
        c.w, c.h, gfx::checksum(c.buf));
}
