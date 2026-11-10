//! i.MX6SL EPDC, the minimal polled path: one LUT, full-screen INIT then GC16 and region updates.
//!
//! Register programming follows our Linux driver (`drivers/gpu/drm/mxc-epdc/epdc_hw.c`, `epdc_update.c`,
//! `epdc_waveform.c`, register map `epdc_regs.h`) with the PW3 panel values from `imx6sl-amazon-wario.dts`
//! (Amazon PANEL_MODE_ED060TC1_3CE_CARTA_1_2). The waveform is Amazon's `epdc.fw` from the running system; it is
//! handed over by lucina-kexec as the "initrd" and never stored in this repository.

use crate::arch::{mm, timer, Mmio};

const EPDC: Mmio = Mmio::at(0x020f_4000);

const CTRL: usize = 0x000;
const CTRL_SET: usize = 0x004;
const CTRL_CLEAR: usize = 0x008;
const WB_ADDR_TCE_V3: usize = 0x010;
const WVADDR: usize = 0x020;
const WB_ADDR: usize = 0x030;
const RES: usize = 0x040;
const FORMAT: usize = 0x050;
const WB_FIELD1: usize = 0x070;
const WB_FIELD2: usize = 0x080;
const FIFOCTRL: usize = 0x0a0;
const UPD_ADDR: usize = 0x100;
const UPD_STRIDE: usize = 0x110;
const UPD_CORD: usize = 0x120;
const UPD_SIZE: usize = 0x140;
const UPD_CTRL: usize = 0x160;
const UPD_FIXED: usize = 0x180;
const TEMP: usize = 0x1a0;
const AUTOWV_LUT: usize = 0x1c0;
const TCE_CTRL: usize = 0x200;
const TCE_SDCFG: usize = 0x220;
const TCE_GDCFG: usize = 0x240;
const TCE_HSCAN1: usize = 0x260;
const TCE_HSCAN2: usize = 0x280;
const TCE_VSCAN: usize = 0x2a0;
const TCE_OE: usize = 0x2c0;
const TCE_POLARITY: usize = 0x2e0;
const TCE_TIMING1: usize = 0x300;
const TCE_TIMING2: usize = 0x310;
const TCE_TIMING3: usize = 0x320;
const IRQ_MASK1_SET: usize = 0x3c4;
const IRQ_MASK1_CLEAR: usize = 0x3c8;
const IRQ_MASK2_CLEAR: usize = 0x3d8;
const IRQ1: usize = 0x3e0;
const IRQ1_CLEAR: usize = 0x3e8;
const IRQ_MASK: usize = 0x400;
const IRQ: usize = 0x420;
const IRQ_CLEAR: usize = 0x428;
const STATUS_LUTS: usize = 0x440;
const STATUS: usize = 0x4a0;
const GPIO: usize = 0x700;
const VERSION: usize = 0x7f0;
const WB_ADDR_TCE: usize = 0xc10;

const CTRL_SFTRST: u32 = 0x8000_0000;
const CTRL_CLKGATE: u32 = 0x4000_0000;
const FORMAT_BUF_P4N: u32 = 0x400;
const FORMAT_BUF_P5N: u32 = 0x500;
const UPD_CTRL_USE_FIXED: u32 = 0x8000_0000;
const UPD_CTRL_FULL: u32 = 0x1;
const UPD_FIXED_FIXNP_EN: u32 = 0x8000_0000;
const TCE_CTRL_PIXELS_PER_SDCLK_4: u32 = 2;
const TCE_SDCFG_SDCLK_HOLD: u32 = 0x20_0000;
const TCE_SDCFG_SDSHR: u32 = 0x10_0000;
const TCE_SDCFG_FLIP_PIXELS: u32 = 0x4000;
const TCE_GDCFG_GDRL: u32 = 0x10;
const POL_SDLE_HIGH: u32 = 0x2;
const POL_SDOE_HIGH: u32 = 0x4;
const POL_GDOE_HIGH: u32 = 0x8;
const IRQ_WB_CMPLT: u32 = 0x1_0000;
const IRQ_LUT_COL: u32 = 0x2_0000;
const IRQ_TCE_UNDERRUN: u32 = 0x4_0000;
const STATUS_WB_BUSY: u32 = 0x1;

/// Native scan size of the ED060TC1 Carta 1.2 panel (scanned landscape).
pub const WIDTH: usize = 1448;
pub const HEIGHT: usize = 1072;

/// Panel timing from the device tree (`&epdc` in imx6sl-amazon-wario.dts).
struct Panel {
    hfront: u32,
    hback: u32,
    hsync: u32,
    vfront: u32,
    vback: u32,
    vsync: u32,
    vscan_holdoff: u32,
    sdoed_width: u32,
    sdoed_delay: u32,
    sdoez_width: u32,
    sdoez_delay: u32,
    gdclk_hp_offs: u32,
    gdsp_offs: u32,
    gdoe_offs: u32,
    gdclk_offs: u32,
    num_ce: u32,
}

const PW3_PANEL: Panel = Panel {
    hfront: 104,
    hback: 16,
    hsync: 26,
    vfront: 4,
    vback: 4,
    vsync: 2,
    vscan_holdoff: 4,
    sdoed_width: 10,
    sdoed_delay: 20,
    sdoez_width: 10,
    sdoez_delay: 20,
    gdclk_hp_offs: 562,
    gdsp_offs: 662,
    gdoe_offs: 0,
    gdclk_offs: 225,
    num_ce: 3,
};

/// Logical waveform modes in Amazon's table 25 (PW3 `epdc.fw`, mode_version 0x19).
pub const MODE_INIT: u32 = 0;
pub const MODE_GC16: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    WaveformTooShort,
    WaveformTooBig,
    WbTimeout,
    LutTimeout,
}

/// What the waveform header says (layout: `struct waveform_data_header`, 48 bytes, then the temperature range
/// table of `trc + 1` bytes and one checksum byte, then the waveform data the EPDC reads).
pub struct WaveformInfo {
    pub mode_version: u8,
    pub trt_entries: usize,
    pub temp_index: u32,
    pub five_bit: bool,
    pub data_offset: usize,
    pub data_len: usize,
}

pub fn parse_waveform(fw: &[u8], temp_c: i32) -> Result<WaveformInfo, Error> {
    if fw.len() < 64 {
        return Err(Error::WaveformTooShort);
    }
    let mode_version = fw[16]; // wi4 low byte: the waveform mode version
    let luts = fw[36];
    let trc = fw[38] as usize;
    let trt_entries = trc + 1;
    let data_offset = 48 + trt_entries + 1;
    if fw.len() <= data_offset {
        return Err(Error::WaveformTooShort);
    }
    let bounds = &fw[48..48 + trt_entries];
    // mxc_epdc_fb_get_temp_index: the range [b[i], b[i+1]) holding the temperature, else clamp.
    let t = temp_c.clamp(0, 255);
    let mut temp_index = 0u32;
    let mut found = false;
    for i in 0..trt_entries.saturating_sub(1) {
        if t >= bounds[i] as i32 && t < bounds[i + 1] as i32 {
            temp_index = i as u32;
            found = true;
            break;
        }
    }
    if !found && t >= bounds[trt_entries - 1] as i32 {
        temp_index = trt_entries as u32 - 1;
    }
    Ok(WaveformInfo {
        mode_version,
        trt_entries,
        temp_index,
        five_bit: luts & 0xc == 0x4,
        data_offset,
        data_len: fw.len() - data_offset,
    })
}

/// Buffers in the uncached DMA pool: update buffer (8 bpp), working buffer (16 bpp), waveform.
pub struct Buffers {
    pub upd_pa: usize,
    pub wb_pa: usize,
    pub wv_pa: usize,
    pub wv_cap: usize,
}

impl Buffers {
    pub fn new() -> Self {
        let (pool, size) = mm::dma_pool();
        let upd_pa = pool;
        let wb_pa = pool + (2 << 20);
        let wv_pa = pool + (6 << 20);
        Buffers { upd_pa, wb_pa, wv_pa, wv_cap: pool + size - wv_pa }
    }

    pub fn update_buffer(&self) -> &'static mut [u8] {
        // SAFETY: the pool is mapped uncached in the linear map and used by nothing else.
        unsafe { core::slice::from_raw_parts_mut(mm::phys_to_virt(self.upd_pa) as *mut u8, WIDTH * HEIGHT) }
    }
}

pub struct Epdc {
    pub rev: u32,
    buf: Buffers,
    pub info: WaveformInfo,
}

pub fn version() -> u32 {
    let v = EPDC.read(VERSION);
    ((v >> 24) & 0xff) * 10 + ((v >> 16) & 0xff)
}

impl Epdc {
    /// Copy the waveform into the DMA pool and program the controller (epdc_init_settings). Clocks must be on.
    pub fn init(fw: &[u8], temp_c: i32) -> Result<Epdc, Error> {
        let info = parse_waveform(fw, temp_c)?;
        let buf = Buffers::new();
        if info.data_len > buf.wv_cap {
            return Err(Error::WaveformTooBig);
        }
        // SAFETY: the destination is the waveform slot of the uncached pool (bounds checked above).
        unsafe {
            core::ptr::copy_nonoverlapping(
                fw.as_ptr().add(info.data_offset),
                mm::phys_to_virt(buf.wv_pa) as *mut u8,
                info.data_len,
            );
            core::ptr::write_bytes(mm::phys_to_virt(buf.wb_pa) as *mut u8, 0, WIDTH * HEIGHT * 2);
        }
        let rev = version();
        let e = Epdc { rev, buf, info };
        e.program(&PW3_PANEL);
        Ok(e)
    }

    pub fn update_buffer(&self) -> &'static mut [u8] {
        self.buf.update_buffer()
    }

    fn program(&self, p: &Panel) {
        EPDC.write(CTRL_SET, CTRL_SFTRST);
        let _ = timer::poll_until(10, || EPDC.read(CTRL) & CTRL_CLKGATE != 0);
        EPDC.write(CTRL_CLEAR, CTRL_SFTRST);
        EPDC.write(CTRL_CLEAR, CTRL_CLKGATE);
        let _ = timer::poll_until(10, || EPDC.read(CTRL) & (CTRL_SFTRST | CTRL_CLKGATE) == 0);

        let pix = if self.info.five_bit { FORMAT_BUF_P5N } else { FORMAT_BUF_P4N };
        EPDC.write(FORMAT, pix); // TFT 2-bit, default TFT pixel 0
        if self.rev >= 30 {
            let (f2, f1) = if self.info.five_bit { (0xc554, 0xa004) } else { (0xc443, 0xa003) };
            EPDC.write(WB_FIELD2, f2);
            EPDC.write(WB_FIELD1, f1);
        }
        EPDC.write(FIFOCTRL, (100 << 16) | (200 << 8) | 100);
        EPDC.write(TEMP, self.info.temp_index);
        EPDC.write(RES, ((HEIGHT as u32) << 16) | WIDTH as u32);
        for i in 0..8 {
            EPDC.write(AUTOWV_LUT, (MODE_GC16 << 16) | i);
        }
        EPDC.write(TCE_CTRL, (p.vscan_holdoff << 16) | TCE_CTRL_PIXELS_PER_SDCLK_4);
        // epdc_set_horizontal_timing(hsync_start - hdisplay, htotal - hsync_end, hsync_len, hsync_len)
        EPDC.write(TCE_HSCAN1, (p.hsync << 16) | p.hsync);
        EPDC.write(TCE_HSCAN2, (p.hback << 16) | p.hfront);
        // epdc_set_vertical_timing(vsync_start - vdisplay, vtotal - vsync_end, vsync_len)
        EPDC.write(TCE_VSCAN, (p.vback << 16) | (p.vfront << 8) | p.vsync);
        EPDC.write(
            TCE_OE,
            (p.sdoed_width << 24) | (p.sdoed_delay << 16) | (p.sdoez_width << 8) | p.sdoez_delay,
        );
        EPDC.write(TCE_TIMING1, 0);
        EPDC.write(TCE_TIMING2, (p.gdclk_hp_offs << 16) | p.gdsp_offs);
        EPDC.write(TCE_TIMING3, (p.gdoe_offs << 16) | p.gdclk_offs);
        EPDC.write(
            TCE_SDCFG,
            TCE_SDCFG_SDCLK_HOLD
                | TCE_SDCFG_SDSHR
                | (p.num_ce << 16)
                | TCE_SDCFG_FLIP_PIXELS
                | ((WIDTH as u32 / p.num_ce) & 0x1fff),
        );
        EPDC.write(TCE_GDCFG, TCE_GDCFG_GDRL);
        EPDC.write(TCE_POLARITY, POL_SDLE_HIGH | POL_SDOE_HIGH | POL_GDOE_HIGH);
        // Unmask what update() polls, as Linux does before it waits (the EPDC line stays off at the GIC).
        let lut0_legacy = if self.rev < 20 { 1 } else { 0 };
        EPDC.write(IRQ_MASK, IRQ_TCE_UNDERRUN | IRQ_WB_CMPLT | lut0_legacy);
        EPDC.write(IRQ_MASK1_CLEAR, 0xffff_ffff);
        EPDC.write(IRQ_MASK2_CLEAR, 0xffff_ffff);
        if self.rev >= 20 {
            EPDC.write(IRQ_MASK1_SET, 1);
        }
        EPDC.write(GPIO, 0);
        EPDC.write(WVADDR, self.buf.wv_pa as u32);
        EPDC.write(WB_ADDR, self.buf.wb_pa as u32);
        if self.rev >= 30 {
            EPDC.write(WB_ADDR_TCE_V3, self.buf.wb_pa as u32);
        } else {
            EPDC.write(WB_ADDR_TCE, self.buf.wb_pa as u32);
        }
    }

    fn lut0_done(&self) -> bool {
        if self.rev < 20 { EPDC.read(IRQ) & 1 != 0 } else { EPDC.read(IRQ1) & 1 != 0 }
    }

    fn clear_lut0(&self) {
        if self.rev < 20 {
            EPDC.write(IRQ_CLEAR, 1);
        } else {
            EPDC.write(IRQ1_CLEAR, 1);
        }
    }

    /// Submit one update on LUT 0 and wait for it (poll; the working buffer pass takes ~6 ms, the scan up to
    /// ~0.7 s for GC16 and ~2.5 s for INIT on this panel). `fixed` = paint every pixel this value (test mode).
    pub fn update(
        &self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        mode: u32,
        full: bool,
        fixed: Option<u8>,
    ) -> Result<UpdateTiming, Error> {
        let _ = timer::poll_until(1000, || EPDC.read(STATUS) & STATUS_WB_BUSY == 0);
        EPDC.write(IRQ_CLEAR, IRQ_WB_CMPLT | IRQ_LUT_COL);
        self.clear_lut0();
        let addr = self.buf.upd_pa + y * WIDTH + x;
        EPDC.write(UPD_ADDR, addr as u32);
        EPDC.write(UPD_CORD, ((y as u32) << 16) | x as u32);
        EPDC.write(UPD_SIZE, ((h as u32) << 16) | w as u32);
        EPDC.write(UPD_STRIDE, if w == WIDTH { 0 } else { WIDTH as u32 });
        let mut ctrl = ((mode << 8) & 0xff00) | if full { UPD_CTRL_FULL } else { 0 };
        match fixed {
            Some(np) => {
                EPDC.write(UPD_FIXED, UPD_FIXED_FIXNP_EN | ((np as u32) << 8));
                ctrl |= UPD_CTRL_USE_FIXED;
            }
            None => EPDC.write(UPD_FIXED, 0),
        }
        let t0 = timer::now();
        EPDC.write(UPD_CTRL, ctrl); // LUT_SEL = 0
        if !timer::poll_until(1000, || EPDC.read(IRQ) & IRQ_WB_CMPLT != 0) {
            return Err(Error::WbTimeout);
        }
        let t_wb = timer::now();
        if !timer::poll_until(6000, || self.lut0_done() && EPDC.read(STATUS_LUTS) & 1 == 0) {
            return Err(Error::LutTimeout);
        }
        let t_done = timer::now();
        self.clear_lut0();
        EPDC.write(IRQ_CLEAR, IRQ_WB_CMPLT);
        Ok(UpdateTiming {
            wb_us: counts_to_us(t_wb.wrapping_sub(t0)),
            total_us: counts_to_us(t_done.wrapping_sub(t0)),
            collision: EPDC.read(IRQ) & IRQ_LUT_COL != 0,
        })
    }

    pub fn clock_gate(&self) {
        EPDC.write(CTRL_SET, CTRL_CLKGATE);
    }
}

pub struct UpdateTiming {
    pub wb_us: u32,
    pub total_us: u32,
    pub collision: bool,
}

fn counts_to_us(c: u32) -> u32 {
    (c as u64 * 1_000_000 / timer::FREQ as u64) as u32
}
