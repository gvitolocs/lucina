//! Runs updates on the panel: the rails, the controller's registers, the choice of a LUT, collisions, completion.
//!
//! One thread calls [`Engine::service`] again and again while [`Engine::is_busy`]. Everything else here reads the
//! controller's status, so nothing depends on an interrupt.
//!
//! The rails go up in the order logic, pads, clocks, high voltages, power good, VCOM. They come down in the opposite
//! order, and only with no LUT active: the waveform ends in neutral phases, and cutting it short leaves DC on the panel.

use alloc::{
    collections::{BTreeMap, VecDeque},
    vec::Vec,
};

use crate::{
    Error, Hw,
    panel::{self, HEIGHT, WIDTH},
    pmic::{Pmic, PmicBus},
    soc,
    waveform::Waveform,
};

const EPDC: usize = 0x020f_4000;
const CTRL: usize = EPDC;
const CTRL_SET: usize = EPDC + 0x004;
const CTRL_CLEAR: usize = EPDC + 0x008;
const WVADDR: usize = EPDC + 0x020;
const WB_ADDR: usize = EPDC + 0x030;
const RES: usize = EPDC + 0x040;
const FORMAT: usize = EPDC + 0x050;
const FIFOCTRL: usize = EPDC + 0x0a0;
const UPD_ADDR: usize = EPDC + 0x100;
const UPD_STRIDE: usize = EPDC + 0x110;
const UPD_CORD: usize = EPDC + 0x120;
const UPD_SIZE: usize = EPDC + 0x140;
const UPD_CTRL: usize = EPDC + 0x160;
const UPD_FIXED: usize = EPDC + 0x180;
const TEMP: usize = EPDC + 0x1a0;
const AUTOWV_LUT: usize = EPDC + 0x1c0;
const TCE_CTRL: usize = EPDC + 0x200;
const TCE_SDCFG: usize = EPDC + 0x220;
const TCE_GDCFG: usize = EPDC + 0x240;
const TCE_HSCAN1: usize = EPDC + 0x260;
const TCE_HSCAN2: usize = EPDC + 0x280;
const TCE_VSCAN: usize = EPDC + 0x2a0;
const TCE_OE: usize = EPDC + 0x2c0;
const TCE_POLARITY: usize = EPDC + 0x2e0;
const TCE_TIMING1: usize = EPDC + 0x300;
const TCE_TIMING2: usize = EPDC + 0x310;
const TCE_TIMING3: usize = EPDC + 0x320;
const IRQ_MASK1_SET: usize = EPDC + 0x3c4;
const IRQ_MASK1_CLEAR: usize = EPDC + 0x3c8;
const IRQ_MASK2_CLEAR: usize = EPDC + 0x3d8;
const IRQ1: usize = EPDC + 0x3e0;
const IRQ1_CLEAR: usize = EPDC + 0x3e8;
const IRQ_MASK: usize = EPDC + 0x400;
const IRQ: usize = EPDC + 0x420;
const IRQ_CLEAR: usize = EPDC + 0x428;
const STATUS_LUTS: usize = EPDC + 0x440;
const STATUS_COL: usize = EPDC + 0x480;
const STATUS: usize = EPDC + 0x4a0;
const UPD_COL_CORD: usize = EPDC + 0x4c0;
const UPD_COL_SIZE: usize = EPDC + 0x4e0;
const GPIO: usize = EPDC + 0x700;
const VERSION: usize = EPDC + 0x7f0;
const WB_ADDR_TCE: usize = EPDC + 0xc10;

const CTRL_SFTRST: u32 = 1 << 31;
const CTRL_CLKGATE: u32 = 1 << 30;
const FORMAT_BUF_P4N: u32 = 0x400;
const FORMAT_BUF_P5N: u32 = 0x500;
const UPD_CTRL_FULL: u32 = 1;
const UPD_CTRL_USE_FIXED: u32 = 1 << 31;
const UPD_FIXED_EN: u32 = 1 << 31;
const IRQ_WB_CMPLT: u32 = 1 << 16;
const IRQ_LUT_COL: u32 = 1 << 17;
const IRQ_TCE_UNDERRUN: u32 = 1 << 18;
const STATUS_WB_BUSY: u32 = 1 << 0;
const STATUS_UPD_VOID: u32 = 1 << 3;

/// With a 5-bit waveform only these LUTs exist.
const LUTS: usize = 16;
const LUT_MASK: u32 = (1 << LUTS) - 1;
/// When a pass or a scan counts as stuck: well over any waveform in the file, the longest being under 2 s.
const STUCK_NS: u64 = 8_000_000_000;
/// How long the temperature stays valid.
const TEMP_VALID_NS: u64 = 60_000_000_000;
/// The assumed temperature when the sensor cannot be read.
const DEFAULT_TEMP_C: i32 = 20;
/// The rails' limits.
const POWER_GOOD_TIMEOUT_MS: u32 = 250;
const POWER_DOWN_TIMEOUT_MS: u32 = 10;
/// The INIT waveform's fixed pixel: white.
const INIT_PIXEL: u8 = 0xff;
const FILE_MODE_INIT: u8 = 0;
/// The marker of the INIT: user space never gets 0.
const INIT_MARKER: u32 = 0;
/// How long a restart waits for a scan: the longest waveform in the file, at its coldest.
const RESTART_SCAN_WAIT_MS: u32 = 2000;
/// How many times a write that takes a rail down is tried, for each call.
const RAIL_TRIES: u32 = 3;
/// How many finished updates are kept for those who wait for them.
const FINISHED_KEPT: usize = 256;

/// The controller's buffers, by physical address. The kernel keeps them mapped uncached.
#[derive(Clone, Copy, Debug)]
pub struct Buffers {
    pub update: u32,
    pub working: u32,
    pub waveform: u32,
}

/// A rectangle in the panel's own coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    /// The whole panel.
    pub const FULL: Rect = Rect {
        x: 0,
        y: 0,
        w: WIDTH,
        h: HEIGHT,
    };
}

/// An update to run.
#[derive(Clone, Copy, Debug)]
pub struct Update {
    pub rect: Rect,
    /// The waveform file's mode.
    pub file_mode: u8,
    /// Whether every pixel is driven, not only the ones that change.
    pub full: bool,
    /// The temperature to use, else the panel's.
    pub temp_c: Option<i32>,
}

/// How an update ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Finished {
    pub marker: u32,
    pub file_mode: u8,
    pub collided: bool,
    pub cancelled: bool,
    pub queued_ns: u64,
    pub submitted_ns: u64,
    pub scanning_ns: u64,
    pub done_ns: u64,
}

/// An update waiting for its turn.
#[derive(Clone, Copy, Debug)]
struct Queued {
    marker: u32,
    update: Update,
    queued_ns: u64,
    /// Whether this is the collided part of an update that already ran.
    again: bool,
}

/// The update in its working-buffer pass.
#[derive(Clone, Copy, Debug)]
struct Pass {
    lut: usize,
    marker: u32,
    update: Update,
    seq: u64,
    since_ns: u64,
}

/// A LUT that is scanning.
#[derive(Clone, Copy, Debug)]
struct Slot {
    marker: u32,
    /// The order of submission: a collision with a newer update is no collision.
    seq: u64,
    since_ns: u64,
}

/// What is known of an update that has started.
#[derive(Clone, Copy, Debug)]
struct Track {
    file_mode: u8,
    collided: bool,
    queued_ns: u64,
    submitted_ns: u64,
    scanning_ns: u64,
    /// How many passes, LUTs and collided parts it still waits for.
    outstanding: u32,
    /// The order of submission, which a collided part keeps from its update.
    seq: u64,
}

/// The part of an update that collided: it goes when `waiting_for` is done.
#[derive(Clone, Copy, Debug)]
struct Blocked {
    marker: u32,
    update: Update,
    queued_ns: u64,
    waiting_for: u32,
    rect: Rect,
    since_ns: u64,
}

/// What the engine says about itself, for the log.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub power_cycles: u32,
    pub updates: u32,
    pub collisions: u32,
    pub underruns: u32,
    pub timeouts: u32,
    pub last_error: Option<Error>,
    pub temp_c: i32,
    pub temp_index: u32,
    pub version: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Rails {
    Off,
    On,
}

/// The panel.
pub struct Engine<H, B> {
    hw: H,
    pmic: Pmic<B>,
    buffers: Buffers,
    waveform: Waveform,
    rails: Rails,
    /// Whether the panel has had its INIT since this kernel started: the working buffer says nothing before that.
    initialized: bool,
    queue: VecDeque<Queued>,
    pass: Option<Pass>,
    tracks: BTreeMap<u32, Track>,
    /// Whether a stuck update was given up on, so that the rails come down regardless.
    gave_up: bool,
    idle_since: Option<u64>,
    luts: [Option<Slot>; LUTS],
    blocked: Vec<Blocked>,
    finished: VecDeque<Finished>,
    seq: u64,
    temp_read_ns: Option<u64>,
    temp_c: i32,
    stats: Stats,
}

impl<H: Hw, B: PmicBus> Engine<H, B> {
    /// Programs the controller, with no power on the panel. The working buffer must be zeroed already, and the waveform's
    /// data be in its buffer.
    pub fn new(mut hw: H, bus: B, buffers: Buffers, waveform: Waveform) -> Result<Self, Error> {
        soc::power_good_setup(&mut hw);
        soc::power_domain_on(&mut hw)?;
        let mut pmic = Pmic::new(bus);
        // A kernel before this one may have left them on, and the reset below would cut its scan.
        quiesce(&mut hw, &mut pmic)?;
        soc::clocks_on(&mut hw);
        let result = program(&mut hw, &buffers, &waveform);
        let version = hw.read32(VERSION);
        hw.write32(CTRL_SET, CTRL_CLKGATE);
        soc::clocks_off(&mut hw);
        result?;
        Ok(Self {
            hw,
            pmic,
            buffers,
            waveform,
            rails: Rails::Off,
            initialized: false,
            queue: VecDeque::new(),
            pass: None,
            tracks: BTreeMap::new(),
            gave_up: false,
            idle_since: None,
            luts: [None; LUTS],
            blocked: Vec::new(),
            finished: VecDeque::new(),
            seq: 0,
            temp_read_ns: None,
            temp_c: DEFAULT_TEMP_C,
            stats: Stats {
                version,
                temp_c: DEFAULT_TEMP_C,
                ..Stats::default()
            },
        })
    }

    /// What the engine is waiting for, for the log when an update does not end.
    pub fn describe(&mut self) -> alloc::string::String {
        let (status, luts, irq, irq1) = (
            self.hw.read32(STATUS),
            self.hw.read32(STATUS_LUTS),
            self.hw.read32(IRQ),
            self.hw.read32(IRQ1),
        );
        alloc::format!(
            "rails {:?}, queue {}, pass {:?}, luts {:?}, blocked {:?}, tracks {:?}, status {status:#x} luts {luts:#x} irq {irq:#x} irq1 {irq1:#x}, {:?}",
            self.rails,
            self.queue.len(),
            self.pass.map(|p| (p.marker, p.lut)),
            self.luts.iter().enumerate().filter_map(|(l, s)| s.map(|s| (l, s.marker))).collect::<alloc::vec::Vec<_>>(),
            self.blocked.iter().map(|b| (b.marker, b.waiting_for)).collect::<alloc::vec::Vec<_>>(),
            self.tracks.keys().collect::<alloc::vec::Vec<_>>(),
            self.stats,
        )
    }

    pub fn stats(&self) -> Stats {
        self.stats
    }

    pub fn waveform(&self) -> &Waveform {
        &self.waveform
    }

    /// The panel's temperature as last read.
    pub fn temp_c(&self) -> i32 {
        self.temp_c
    }

    /// Queues an update.
    pub fn enqueue(&mut self, marker: u32, update: Update, now_ns: u64) {
        self.queue.push_back(Queued {
            marker,
            update,
            queued_ns: now_ns,
            again: false,
        });
    }

    /// Whether [`Engine::service`] has work to do.
    /// Takes the panel's rails down for a sleep, if no update is running. Says whether they are down.
    pub fn power_down_now(&mut self) -> bool {
        if self.is_busy() {
            return false;
        }
        if matches!(self.rails, Rails::On) {
            self.power_off();
        }
        matches!(self.rails, Rails::Off)
    }

    pub fn is_busy(&self) -> bool {
        self.rails == Rails::On || !self.queue.is_empty()
    }

    /// Takes the working buffer as it is in place of the INIT: the caller has put back one saved after the last update
    /// of an earlier boot, and the panel has kept that picture since, as e-ink does without power. Only before the first
    /// update of this kernel: after it the buffer is this boot's.
    pub fn adopt_working_buffer(&mut self) -> Result<(), Error> {
        if self.initialized || self.pass.is_some() || !self.queue.is_empty() || self.rails == Rails::On {
            return Err(Error::PanelStarted);
        }
        self.initialized = true;
        Ok(())
    }

    /// Whether the working buffer says what the panel shows: after the INIT or an adopted buffer.
    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    /// Whether an update is queued or running. Between updates the working buffer is what the glass shows.
    pub fn is_drawing(&self) -> bool {
        !self.queue.is_empty()
            || self.pass.is_some()
            || !self.blocked.is_empty()
            || self.luts.iter().any(Option::is_some)
    }

    /// Takes the updates that have ended.
    pub fn take_finished(&mut self) -> impl Iterator<Item = Finished> + '_ {
        self.finished.drain(..)
    }

    /// Moves everything on as far as it goes now.
    pub fn service(&mut self, now_ns: u64) {
        if self.rails == Rails::Off && !self.queue.is_empty() && !self.power_on(now_ns) {
            return;
        }
        if self.rails == Rails::Off {
            return;
        }
        self.check_working_buffer(now_ns);
        self.check_luts(now_ns);
        self.release_blocked();
        self.submit_next(now_ns);
        self.check_timeouts(now_ns);
        if !self.is_idle() {
            self.idle_since = None;
            return;
        }
        // Nothing here is under way, but the rails stay up as long as the controller says a LUT is scanning.
        let since = *self.idle_since.get_or_insert(now_ns);
        let scanning = self.hw.read32(STATUS_LUTS) & LUT_MASK != 0;
        if !scanning || self.gave_up || now_ns.saturating_sub(since) > STUCK_NS {
            self.gave_up = false;
            self.idle_since = None;
            self.power_off();
        }
    }

    fn is_idle(&self) -> bool {
        self.queue.is_empty()
            && self.pass.is_none()
            && self.blocked.is_empty()
            && self.luts.iter().all(Option::is_none)
    }

    fn power_on(&mut self, now_ns: u64) -> bool {
        match self.try_power_on(now_ns) {
            Ok(()) => {
                self.rails = Rails::On;
                self.stats.power_cycles += 1;
                true
            }
            Err(err) => {
                self.stats.last_error = Some(err);
                self.cancel_all(now_ns);
                false
            }
        }
    }

    fn try_power_on(&mut self, now_ns: u64) -> Result<(), Error> {
        self.pmic.check_voltages()?;
        self.read_temperature(now_ns);
        self.pmic.logic(true)?;
        self.hw.delay_us(2000);
        soc::pads_on(&mut self.hw);
        soc::clocks_on(&mut self.hw);
        self.hw.write32(CTRL_CLEAR, CTRL_CLKGATE);
        let result = self.high_voltages_on();
        if result.is_err() && !self.shut_down() {
            self.rails = Rails::On;
        }
        result
    }

    /// With the logic on: the high voltages, power good, no fault, and only then VCOM.
    fn high_voltages_on(&mut self) -> Result<(), Error> {
        self.pmic.high_voltages(true)?;
        let mut good = false;
        for _ in 0..POWER_GOOD_TIMEOUT_MS {
            if soc::power_good(&mut self.hw) {
                good = true;
                break;
            }
            self.hw.delay_us(1000);
        }
        if !good {
            return Err(Error::PowerGoodTimeout);
        }
        if let Some(faults) = self.pmic.faults()? {
            return Err(Error::PanelFault(faults));
        }
        self.pmic.vcom(true)
    }

    fn power_off(&mut self) {
        if self.shut_down() {
            self.rails = Rails::Off;
        }
    }

    /// The rails, the opposite way. Says whether they really are off: if not, they stay this engine's to try again, as a
    /// rail left on is the worse fault.
    fn shut_down(&mut self) -> bool {
        match rails_down(&mut self.hw, &mut self.pmic) {
            Ok(()) => true,
            Err(err) => {
                self.stats.last_error = Some(err);
                false
            }
        }
    }

    fn read_temperature(&mut self, now_ns: u64) {
        let fresh = self
            .temp_read_ns
            .is_some_and(|read| now_ns.saturating_sub(read) < TEMP_VALID_NS);
        if fresh {
            return;
        }
        match self.pmic.temperature() {
            Ok(temp_c) => self.temp_c = temp_c,
            Err(err) => {
                self.stats.last_error = Some(err);
                self.temp_c = DEFAULT_TEMP_C;
            }
        }
        self.temp_read_ns = Some(now_ns);
        self.stats.temp_c = self.temp_c;
        self.stats.temp_index = self.waveform.temp_index(self.temp_c);
    }

    fn check_working_buffer(&mut self, now_ns: u64) {
        let Some(pass) = self.pass else {
            return;
        };
        let irq = self.hw.read32(IRQ);
        if irq & IRQ_TCE_UNDERRUN != 0 {
            self.stats.underruns += 1;
            self.hw.write32(IRQ_CLEAR, IRQ_TCE_UNDERRUN);
        }
        if irq & IRQ_WB_CMPLT == 0 {
            return;
        }
        // The collision's bits must be read before the clear: on this version of the controller it wipes them.
        let status = self.hw.read32(STATUS);
        let colliding = self.hw.read32(STATUS_COL) & LUT_MASK;
        let rect = self.collision_rect();
        // A LUT may have ended since the pass began, and its completion been handled: it will not end again.
        let still_running = self.hw.read32(STATUS_LUTS)
            | self.hw.read32(IRQ1)
            | self
                .luts
                .iter()
                .enumerate()
                .filter(|(_, slot)| slot.is_some())
                .fold(0, |mask, (lut, _)| mask | (1u32 << lut));
        self.hw.write32(IRQ_CLEAR, IRQ_WB_CMPLT | IRQ_LUT_COL);
        self.pass = None;
        if let Some(track) = self.tracks.get_mut(&pass.marker)
            && track.scanning_ns == 0
        {
            track.scanning_ns = now_ns;
        }

        // As Amazon's `lut_update_order[lut] >= update_order`: not again over a newer update, nor over this update's own
        // earlier pass, which is taking those pixels to the same place. Its part that goes again overlaps what its first
        // pass is scanning, so it would collide with that for ever.
        let in_the_way_of_newer = self.luts.iter().enumerate().any(|(lut, slot)| {
            colliding & (1 << lut) != 0 && slot.is_some_and(|slot| slot.seq >= pass.seq)
        });
        if irq & IRQ_LUT_COL != 0 && !in_the_way_of_newer && rect.w != 0 && rect.h != 0 {
            self.stats.collisions += 1;
            if let Some(track) = self.tracks.get_mut(&pass.marker) {
                track.collided = true;
                track.outstanding += 1;
            }
            self.blocked.push(Blocked {
                marker: pass.marker,
                update: pass.update,
                queued_ns: self
                    .tracks
                    .get(&pass.marker)
                    .map_or(now_ns, |track| track.queued_ns),
                waiting_for: colliding & still_running & !(1 << pass.lut),
                rect,
                since_ns: now_ns,
            });
        }
        if status & STATUS_UPD_VOID != 0 {
            // Nothing to scan: no LUT will finish.
            self.hw.write32(IRQ1_CLEAR, 1 << pass.lut);
            self.part_done(pass.marker, now_ns);
        } else {
            self.luts[pass.lut] = Some(Slot {
                marker: pass.marker,
                seq: pass.seq,
                since_ns: now_ns,
            });
        }
    }

    fn collision_rect(&mut self) -> Rect {
        let cord = self.hw.read32(UPD_COL_CORD);
        let size = self.hw.read32(UPD_COL_SIZE);
        Rect {
            x: cord & 0xffff,
            y: cord >> 16,
            w: size & 0xffff,
            h: size >> 16,
        }
    }

    fn check_luts(&mut self, now_ns: u64) {
        let done = self.hw.read32(IRQ1) & LUT_MASK;
        let busy = self.hw.read32(STATUS_LUTS) & LUT_MASK;
        for lut in 0..LUTS {
            let bit = 1 << lut;
            if done & bit == 0 || busy & bit != 0 {
                continue;
            }
            self.hw.write32(IRQ1_CLEAR, bit);
            for blocked in &mut self.blocked {
                blocked.waiting_for &= !bit;
            }
            if let Some(slot) = self.luts[lut].take() {
                self.part_done(slot.marker, now_ns);
            }
        }
    }

    /// One pass, LUT or collided part of an update ended. The update ends with the last of them.
    fn part_done(&mut self, marker: u32, now_ns: u64) {
        let Some(track) = self.tracks.get_mut(&marker) else {
            return;
        };
        track.outstanding -= 1;
        if track.outstanding == 0 {
            let track = *track;
            self.tracks.remove(&marker);
            self.record(marker, &track, now_ns, false);
        }
    }

    fn record(&mut self, marker: u32, track: &Track, now_ns: u64, cancelled: bool) {
        // The INIT of the panel is nobody's update.
        if marker == INIT_MARKER {
            return;
        }
        if self.finished.len() == FINISHED_KEPT {
            self.finished.pop_front();
        }
        self.finished.push_back(Finished {
            marker,
            file_mode: track.file_mode,
            collided: track.collided,
            cancelled,
            queued_ns: track.queued_ns,
            submitted_ns: track.submitted_ns,
            scanning_ns: track.scanning_ns,
            done_ns: now_ns,
        });
    }

    /// A collided update goes again, for the part that collided, when the LUTs in its way are done.
    fn release_blocked(&mut self) {
        let mut index = 0;
        while index < self.blocked.len() {
            if self.blocked[index].waiting_for != 0 {
                index += 1;
                continue;
            }
            let blocked = self.blocked.remove(index);
            let mut update = blocked.update;
            update.rect = blocked.rect;
            // Ahead of the queue: later updates must not paint under it.
            self.queue.push_front(Queued {
                marker: blocked.marker,
                update,
                queued_ns: blocked.queued_ns,
                again: true,
            });
        }
    }

    /// A free LUT, as the hardware's rule has it: above the highest one in use, else the lowest free. One whose
    /// completion has not been handled yet is not free.
    fn free_lut(&mut self) -> Option<usize> {
        let busy = self.hw.read32(STATUS_LUTS) & LUT_MASK;
        let unhandled = self.hw.read32(IRQ1) & LUT_MASK;
        let reserved = self
            .luts
            .iter()
            .enumerate()
            .filter(|(_, lut)| lut.is_some())
            .fold(0, |mask, (index, _)| mask | (1u32 << index));
        let used = busy | unhandled | reserved;
        if used == 0 {
            return Some(0);
        }
        let next = 32 - used.leading_zeros() as usize;
        if next < LUTS {
            return Some(next);
        }
        let lowest_free = (!used).trailing_zeros() as usize;
        (lowest_free < LUTS).then_some(lowest_free)
    }

    fn submit_next(&mut self, now_ns: u64) {
        if self.pass.is_some() || self.tracks.contains_key(&INIT_MARKER) {
            return;
        }
        if !self.initialized {
            // The whole panel to white, whatever it shows, so that the working buffer tells the truth.
            let Some(lut) = self.free_lut() else {
                return;
            };
            self.initialized = true;
            let update = Update {
                rect: Rect::FULL,
                file_mode: FILE_MODE_INIT,
                full: true,
                temp_c: None,
            };
            let queued = Queued {
                marker: INIT_MARKER,
                update,
                queued_ns: now_ns,
                again: false,
            };
            self.start(lut, queued, Some(INIT_PIXEL), now_ns);
            return;
        }
        if self.hw.read32(STATUS) & STATUS_WB_BUSY != 0 {
            return;
        }
        let Some(&queued) = self.queue.front() else {
            return;
        };
        let Some(lut) = self.free_lut() else {
            return;
        };
        self.queue.pop_front();
        self.start(lut, queued, None, now_ns);
    }

    fn start(&mut self, lut: usize, queued: Queued, fixed: Option<u8>, now_ns: u64) {
        let update = queued.update;
        let Rect { x, y, w, h } = update.rect;
        self.hw.write32(IRQ_CLEAR, IRQ_WB_CMPLT | IRQ_LUT_COL);
        self.hw.write32(IRQ1_CLEAR, 1 << lut);
        self.hw.write32(IRQ_MASK1_SET, 1 << lut);
        let temp_c = update.temp_c.unwrap_or(self.temp_c);
        self.hw.write32(TEMP, self.waveform.temp_index(temp_c));
        self.hw
            .write32(UPD_ADDR, self.buffers.update + y * WIDTH + x);
        self.hw.write32(UPD_STRIDE, WIDTH);
        self.hw.write32(UPD_CORD, (y << 16) | x);
        self.hw.write32(UPD_SIZE, (h << 16) | w);
        let mut ctrl = ((lut as u32) << 16) | (u32::from(update.file_mode) << 8);
        if update.full {
            ctrl |= UPD_CTRL_FULL;
        }
        match fixed {
            Some(pixel) => {
                self.hw
                    .write32(UPD_FIXED, UPD_FIXED_EN | (u32::from(pixel) << 8));
                ctrl |= UPD_CTRL_USE_FIXED;
            }
            None => self.hw.write32(UPD_FIXED, 0),
        }
        self.stats.updates += 1;
        let seq = if let Some(track) = self.tracks.get(&queued.marker).filter(|_| queued.again) {
            track.seq
        } else {
            self.seq += 1;
            self.seq
        };
        if !queued.again {
            self.tracks.insert(
                queued.marker,
                Track {
                    file_mode: update.file_mode,
                    collided: false,
                    queued_ns: queued.queued_ns,
                    submitted_ns: now_ns,
                    scanning_ns: 0,
                    outstanding: 1,
                    seq,
                },
            );
        }
        self.pass = Some(Pass {
            lut,
            marker: queued.marker,
            update,
            seq,
            since_ns: now_ns,
        });
        // The pixels are in memory that is not device memory, so this order needs saying.
        self.hw.barrier();
        // This write starts the update.
        self.hw.write32(UPD_CTRL, ctrl);
    }

    /// An update the hardware does not finish must not keep the rails up for ever. This fires long past any waveform in
    /// the file, so nothing is being scanned any more.
    fn check_timeouts(&mut self, now_ns: u64) {
        let stuck_pass = self
            .pass
            .is_some_and(|pass| now_ns.saturating_sub(pass.since_ns) > STUCK_NS);
        let stuck_scan = self
            .luts
            .iter()
            .flatten()
            .any(|slot| now_ns.saturating_sub(slot.since_ns) > STUCK_NS);
        let stuck_blocked = self
            .blocked
            .iter()
            .any(|blocked| now_ns.saturating_sub(blocked.since_ns) > STUCK_NS);
        if stuck_pass || stuck_scan || stuck_blocked {
            self.stats.timeouts += 1;
            self.gave_up = true;
            self.cancel_all(now_ns);
        }
    }

    /// Ends everything, for those waiting.
    fn cancel_all(&mut self, now_ns: u64) {
        while let Some(queued) = self.queue.pop_front() {
            if !queued.again {
                let track = Track {
                    file_mode: queued.update.file_mode,
                    collided: false,
                    queued_ns: queued.queued_ns,
                    submitted_ns: 0,
                    scanning_ns: 0,
                    outstanding: 0,
                    seq: 0,
                };
                self.record(queued.marker, &track, now_ns, true);
            }
        }
        self.pass = None;
        self.luts = [None; LUTS];
        self.blocked.clear();
        let tracks = core::mem::take(&mut self.tracks);
        for (marker, track) in &tracks {
            self.record(*marker, track, now_ns, true);
        }
    }
}

/// Takes the panel's rails down at a restart, whatever state the engine was in: the SoC's reset does not reach the PMIC, so
/// rails left on would stay on with nothing scanning. For when nothing else runs any more: it uses no lock and keeps no
/// state.
pub fn rails_off_now<H: Hw, B: PmicBus>(mut hw: H, bus: B) {
    let _ = quiesce(&mut hw, &mut Pmic::new(bus));
}

/// If the rails are on, waits for the scan under way, within reason, as cutting it short leaves DC on the panel, and takes
/// them down.
fn quiesce<H: Hw, B: PmicBus>(hw: &mut H, pmic: &mut Pmic<B>) -> Result<(), Error> {
    let (high, logic) = pmic.rails_on().unwrap_or((true, true));
    if !high && !logic {
        return Ok(());
    }
    if high && soc::clocks_are_on(hw) {
        for _ in 0..RESTART_SCAN_WAIT_MS {
            if hw.read32(STATUS_LUTS) & LUT_MASK == 0 {
                break;
            }
            hw.delay_us(1000);
        }
    }
    rails_down(hw, pmic)
}

/// VCOM, the high voltages, the controller's clocks, the pads, the logic, whichever are still on. A write that fails is
/// tried again, and what counts in the end is what the PMIC reads back.
fn rails_down<H: Hw, B: PmicBus>(hw: &mut H, pmic: &mut Pmic<B>) -> Result<(), Error> {
    let mut last = Ok(());
    for _ in 0..RAIL_TRIES {
        last = pmic.vcom(false).and_then(|()| pmic.high_voltages(false));
        if last.is_ok() {
            break;
        }
    }
    for _ in 0..POWER_DOWN_TIMEOUT_MS {
        if !soc::power_good(hw) {
            break;
        }
        hw.delay_us(1000);
    }
    if soc::clocks_are_on(hw) {
        hw.write32(CTRL_SET, CTRL_CLKGATE);
        soc::clocks_off(hw);
    }
    soc::pads_park(hw);
    for _ in 0..RAIL_TRIES {
        last = last.and(pmic.logic(false));
        if last.is_ok() {
            break;
        }
        last = Ok(());
    }
    last?;
    match pmic.rails_on()? {
        (false, false) => Ok(()),
        _ => Err(Error::RailsStillOn),
    }
}

/// The controller's reset and its values for this panel.
fn program<H: Hw>(hw: &mut H, buffers: &Buffers, waveform: &Waveform) -> Result<(), Error> {
    hw.write32(CTRL_SET, CTRL_SFTRST);
    if !poll(hw, |hw| hw.read32(CTRL) & CTRL_CLKGATE != 0) {
        return Err(Error::ControllerReset);
    }
    hw.write32(CTRL_CLEAR, CTRL_SFTRST);
    hw.write32(CTRL_CLEAR, CTRL_CLKGATE);
    if !poll(hw, |hw| hw.read32(CTRL) & (CTRL_SFTRST | CTRL_CLKGATE) == 0) {
        return Err(Error::ControllerReset);
    }
    hw.write32(
        FORMAT,
        if waveform.five_bit {
            FORMAT_BUF_P5N
        } else {
            FORMAT_BUF_P4N
        },
    );
    hw.write32(FIFOCTRL, panel::FIFOCTRL);
    hw.write32(TEMP, 0);
    hw.write32(RES, panel::RES);
    const AUTO_GC16: u32 = 2 << 16;
    for index in 0..8 {
        hw.write32(AUTOWV_LUT, AUTO_GC16 | index);
    }
    hw.write32(TCE_CTRL, panel::TCE_CTRL);
    hw.write32(TCE_HSCAN1, panel::TCE_HSCAN1);
    hw.write32(TCE_HSCAN2, panel::TCE_HSCAN2);
    hw.write32(TCE_VSCAN, panel::TCE_VSCAN);
    hw.write32(TCE_OE, panel::TCE_OE);
    hw.write32(TCE_TIMING1, 0);
    hw.write32(TCE_TIMING2, panel::TCE_TIMING2);
    hw.write32(TCE_TIMING3, panel::TCE_TIMING3);
    hw.write32(TCE_SDCFG, panel::TCE_SDCFG);
    hw.write32(TCE_GDCFG, panel::TCE_GDCFG);
    hw.write32(TCE_POLARITY, panel::TCE_POLARITY);
    // What is polled is unmasked: the status bits latch only so, as under Linux. The line stays off at the GIC.
    hw.write32(IRQ_MASK, IRQ_TCE_UNDERRUN | IRQ_WB_CMPLT);
    hw.write32(IRQ_MASK1_CLEAR, u32::MAX);
    hw.write32(IRQ_MASK2_CLEAR, u32::MAX);
    hw.write32(GPIO, 0);
    hw.write32(WVADDR, buffers.waveform);
    hw.write32(WB_ADDR, buffers.working);
    hw.write32(WB_ADDR_TCE, buffers.working);
    Ok(())
}

fn poll<H: Hw>(hw: &mut H, mut done: impl FnMut(&mut H) -> bool) -> bool {
    for _ in 0..1000 {
        if done(hw) {
            return true;
        }
        hw.delay_us(10);
    }
    done(hw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Event, Panel};

    const GC16: u8 = 2;
    const GL16: u8 = 3;

    fn update(rect: Rect, file_mode: u8, full: bool) -> Update {
        Update {
            rect,
            file_mode,
            full,
            temp_c: None,
        }
    }

    fn rect(x: u32, y: u32, w: u32, h: u32) -> Rect {
        Rect { x, y, w, h }
    }

    /// Runs until the panel has done its INIT, with nothing else queued but this first update.
    fn after_init(panel: &mut Panel) {
        panel.engine.enqueue(1, update(Rect::FULL, GC16, true), 0);
        panel.service(0);
        assert_eq!(panel.fixed_pixel(), Some(0xff));
        panel.complete_working_buffer(false);
        panel.service(10);
        panel.finish_lut(0);
        panel.service(20);
        panel.complete_working_buffer(false);
        panel.service(30);
        panel.finish_lut(panel.last_lut());
        panel.service(40);
        assert_eq!(panel.finished().len(), 1);
    }

    #[test]
    fn the_rails_come_up_in_order_and_vcom_only_after_power_good() {
        let mut panel = Panel::new();
        panel.engine.enqueue(1, update(Rect::FULL, GC16, true), 0);
        panel.service(0);
        panel.assert_order(&[
            Event::Logic(true),
            Event::Pads(true),
            Event::Clocks(true),
            Event::HighVoltages(true),
            Event::Vcom(true),
            Event::UpdateStarted,
        ]);
    }

    #[test]
    fn the_rails_come_down_the_opposite_way_and_only_after_the_last_lut() {
        let mut panel = Panel::new();
        after_init(&mut panel);
        panel.assert_order(&[
            Event::Vcom(false),
            Event::HighVoltages(false),
            Event::Clocks(false),
            Event::Pads(false),
            Event::Logic(false),
        ]);
        assert!(!panel.engine.is_busy());
        panel.assert_never_down_with_a_lut_active();
    }

    #[test]
    fn nothing_goes_down_while_a_lut_scans() {
        let mut panel = Panel::new();
        panel.engine.enqueue(1, update(Rect::FULL, GC16, true), 0);
        panel.service(0);
        panel.complete_working_buffer(false);
        for time in 1..50 {
            panel.service(time);
        }
        assert!(!panel.has_event(Event::Vcom(false)));
    }

    #[test]
    fn the_first_update_of_a_boot_is_an_init_of_the_whole_panel_to_white() {
        let mut panel = Panel::new();
        panel
            .engine
            .enqueue(1, update(rect(10, 20, 30, 40), GL16, false), 0);
        panel.service(0);
        assert_eq!(panel.started_mode(), 0);
        assert!(panel.started_full());
        assert_eq!(panel.started_rect(), Rect::FULL);
        assert_eq!(panel.fixed_pixel(), Some(0xff));
    }

    #[test]
    fn an_adopted_working_buffer_takes_the_place_of_the_init() {
        let mut panel = Panel::new();
        panel.engine.adopt_working_buffer().unwrap();
        panel
            .engine
            .enqueue(1, update(rect(10, 20, 30, 40), GL16, false), 0);
        panel.service(0);
        assert_eq!(panel.started_rect(), rect(10, 20, 30, 40));
        assert_eq!(panel.started_mode(), 3);
        assert!(!panel.started_full());
        assert_eq!(panel.fixed_pixel(), None);
    }

    #[test]
    fn a_working_buffer_cannot_be_adopted_once_the_panel_has_started() {
        let mut panel = Panel::new();
        panel.engine.enqueue(1, update(Rect::FULL, GC16, true), 0);
        assert_eq!(panel.engine.adopt_working_buffer(), Err(Error::PanelStarted));
        let mut panel = Panel::new();
        after_init(&mut panel);
        assert_eq!(panel.engine.adopt_working_buffer(), Err(Error::PanelStarted));
    }

    #[test]
    fn an_update_writes_the_registers_for_its_rectangle() {
        let mut panel = Panel::new();
        after_init(&mut panel);
        panel
            .engine
            .enqueue(2, update(rect(100, 200, 300, 400), GL16, false), 50);
        panel.service(50);
        assert_eq!(panel.started_rect(), rect(100, 200, 300, 400));
        assert_eq!(panel.started_mode(), 3);
        assert!(!panel.started_full());
        assert_eq!(panel.fixed_pixel(), None);
        assert_eq!(
            panel.get(UPD_ADDR),
            Panel::UPDATE_BUFFER + 200 * WIDTH + 100
        );
        assert_eq!(panel.get(UPD_STRIDE), WIDTH);
    }

    #[test]
    fn a_finished_update_has_its_times_in_order() {
        let mut panel = Panel::new();
        after_init(&mut panel);
        panel.engine.enqueue(2, update(Rect::FULL, GC16, true), 100);
        panel.service(110);
        panel.complete_working_buffer(false);
        panel.service(120);
        panel.finish_lut(panel.last_lut());
        panel.service(800);
        let finished = panel.finished();
        let last = finished.last().unwrap();
        assert_eq!(last.marker, 2);
        assert_eq!(
            (
                last.queued_ns,
                last.submitted_ns,
                last.scanning_ns,
                last.done_ns
            ),
            (100, 110, 120, 800)
        );
        assert!(!last.collided);
        assert!(!last.cancelled);
    }

    #[test]
    fn updates_run_side_by_side_on_their_own_luts() {
        let mut panel = Panel::new();
        after_init(&mut panel);
        panel
            .engine
            .enqueue(2, update(rect(0, 0, 10, 10), GL16, false), 50);
        panel
            .engine
            .enqueue(3, update(rect(20, 0, 10, 10), GL16, false), 50);
        panel.service(50);
        let first = panel.last_lut();
        panel.complete_working_buffer(false);
        panel.service(60);
        let second = panel.last_lut();
        assert_ne!(first, second);
        panel.complete_working_buffer(false);
        panel.service(70);
        panel.finish_lut(second);
        panel.service(80);
        assert_eq!(panel.finished().iter().filter(|f| f.marker == 3).count(), 1);
        assert_eq!(panel.finished().iter().filter(|f| f.marker == 2).count(), 0);
        assert!(!panel.has_event_after_init(Event::Vcom(false)));
        panel.finish_lut(first);
        panel.service(90);
        assert_eq!(panel.finished().iter().filter(|f| f.marker == 2).count(), 1);
        assert!(panel.has_event_after_init(Event::Vcom(false)));
    }

    #[test]
    fn a_collision_runs_the_collided_part_again_after_the_lut_in_its_way() {
        let mut panel = Panel::new();
        after_init(&mut panel);
        panel
            .engine
            .enqueue(2, update(rect(0, 0, 100, 100), GL16, false), 50);
        panel.service(50);
        let first = panel.last_lut();
        panel.complete_working_buffer(false);
        panel.service(60);
        panel
            .engine
            .enqueue(3, update(rect(50, 50, 100, 100), GL16, false), 60);
        panel.service(60);
        let second = panel.last_lut();
        panel.collide_with(first, rect(50, 50, 50, 50));
        panel.complete_working_buffer(true);
        panel.service(70);
        panel.finish_lut(second);
        panel.service(80);
        // Still waiting: its collided part has not run.
        assert!(panel.finished().iter().all(|f| f.marker != 3));
        panel.finish_lut(first);
        panel.service(90);
        assert_eq!(panel.started_rect(), rect(50, 50, 50, 50));
        panel.complete_working_buffer(false);
        panel.service(100);
        panel.finish_lut(panel.last_lut());
        panel.service(110);
        let finished = panel.finished();
        let last = finished.iter().find(|f| f.marker == 3).unwrap();
        assert!(last.collided);
        assert_eq!(panel.engine.stats().collisions, 1);
    }

    /// Seen on the Kindle: the panel stopped finishing an update, under page turns in quick succession.
    #[test]
    fn a_collided_part_does_not_collide_for_ever_with_its_own_first_pass() {
        let mut panel = Panel::new();
        after_init(&mut panel);
        panel
            .engine
            .enqueue(2, update(rect(0, 0, 100, 100), GL16, false), 50);
        panel.service(50);
        let older = panel.last_lut();
        panel.complete_working_buffer(false);
        panel.service(60);
        panel
            .engine
            .enqueue(3, update(rect(50, 50, 100, 100), GL16, false), 60);
        panel.service(60);
        let own_first = panel.last_lut();
        panel.collide_with(older, rect(50, 50, 50, 50));
        panel.complete_working_buffer(true);
        panel.service(70);
        panel.finish_lut(older);
        panel.service(80);
        // It goes again, and now collides with what its own first pass is scanning.
        let own_second = panel.last_lut();
        assert_ne!(own_second, own_first);
        panel.collide_with(own_first, rect(50, 50, 50, 50));
        panel.complete_working_buffer(true);
        panel.service(90);
        assert_eq!(panel.engine.stats().collisions, 1);
        panel.finish_lut(own_first);
        panel.service(100);
        panel.finish_lut(own_second);
        panel.service(110);
        assert!(panel.finished().iter().any(|f| f.marker == 3));
    }

    #[test]
    fn an_update_with_nothing_to_draw_ends_without_a_lut() {
        let mut panel = Panel::new();
        after_init(&mut panel);
        panel
            .engine
            .enqueue(2, update(rect(0, 0, 10, 10), GL16, false), 50);
        panel.service(50);
        panel.void_update();
        panel.complete_working_buffer(false);
        panel.service(60);
        assert!(panel.finished().iter().any(|f| f.marker == 2));
        assert!(!panel.engine.is_busy());
    }

    #[test]
    fn a_fault_on_a_rail_takes_everything_down_and_ends_the_updates() {
        let mut panel = Panel::new();
        panel.fault_on_high_voltages(0x04);
        panel.engine.enqueue(1, update(Rect::FULL, GC16, true), 0);
        panel.service(0);
        assert!(!panel.has_event(Event::Vcom(true)));
        panel.assert_all_off();
        assert_eq!(
            panel.engine.stats().last_error,
            Some(Error::PanelFault(0x04))
        );
        assert!(
            panel
                .finished()
                .iter()
                .any(|f| f.marker == 1 && f.cancelled)
        );
        assert!(!panel.engine.is_busy());
    }

    #[test]
    fn rails_that_never_come_good_take_everything_down_without_vcom() {
        let mut panel = Panel::new();
        panel.no_power_good();
        panel.engine.enqueue(1, update(Rect::FULL, GC16, true), 0);
        panel.service(0);
        assert!(!panel.has_event(Event::Vcom(true)));
        panel.assert_all_off();
        assert_eq!(
            panel.engine.stats().last_error,
            Some(Error::PowerGoodTimeout)
        );
    }

    #[test]
    fn wrong_voltages_are_set_right_before_the_rails_go_up() {
        let mut panel = Panel::new();
        panel.set_pmic(0x64, 100);
        panel.set_pmic(0x68, 15);
        panel.engine.enqueue(1, update(Rect::FULL, GC16, true), 0);
        panel.service(0);
        assert_eq!(panel.pmic(0x64), 244);
        assert_eq!(panel.pmic(0x68), 21);
        panel.assert_voltages_before_rails();
    }

    #[test]
    fn a_scan_that_never_ends_comes_down_in_the_end() {
        let mut panel = Panel::new();
        panel.engine.enqueue(1, update(Rect::FULL, GC16, true), 0);
        panel.service(0);
        panel.complete_working_buffer(false);
        panel.service(1);
        panel.service(STUCK_NS + 2);
        panel.assert_all_off();
        assert_eq!(panel.engine.stats().timeouts, 1);
        assert!(
            panel
                .finished()
                .iter()
                .any(|f| f.marker == 1 && f.cancelled)
        );
    }

    #[test]
    fn the_temperature_comes_from_the_sensor_and_stays_for_a_minute() {
        let mut panel = Panel::new();
        panel.set_temperature_codes(0x2a0, 0x00b);
        after_init(&mut panel);
        assert_eq!(panel.engine.temp_c(), 25);
        let reads = panel.adc_reads();
        panel
            .engine
            .enqueue(2, update(Rect::FULL, GC16, true), 1_000);
        panel.service(1_000);
        assert_eq!(panel.adc_reads(), reads);
        assert_eq!(panel.get(TEMP), 8);
    }

    #[test]
    fn a_broken_sensor_means_20_degrees() {
        let mut panel = Panel::new();
        panel.stuck_adc();
        panel.engine.enqueue(1, update(Rect::FULL, GC16, true), 0);
        panel.service(0);
        assert_eq!(panel.engine.temp_c(), 20);
        assert_eq!(panel.get(TEMP), 6);
    }

    #[test]
    fn the_controller_gets_the_values_that_ran_on_this_device() {
        let panel = Panel::new();
        assert_eq!(panel.get(FORMAT), 0x500);
        assert_eq!(panel.get(RES), 0x0430_05a8);
        assert_eq!(panel.get(TCE_HSCAN2), 0x0010_0068);
        assert_eq!(panel.get(WB_ADDR), Panel::WORKING_BUFFER);
        assert_eq!(panel.get(WB_ADDR_TCE), Panel::WORKING_BUFFER);
        assert_eq!(panel.get(WVADDR), Panel::WAVEFORM_BUFFER);
    }

    #[test]
    fn a_restart_takes_the_rails_down_in_order() {
        let mut panel = Panel::new();
        panel.engine.enqueue(1, update(Rect::FULL, GC16, true), 0);
        panel.service(0);
        panel.finish_lut(panel.last_lut());
        rails_off_now(panel.hw(), panel.bus());
        panel.assert_order(&[
            Event::Vcom(false),
            Event::HighVoltages(false),
            Event::Clocks(false),
            Event::Pads(false),
            Event::Logic(false),
        ]);
        panel.assert_all_off();
    }

    #[test]
    fn a_restart_with_the_rails_down_touches_nothing() {
        let panel = Panel::new();
        let writes = panel.pmic_writes();
        rails_off_now(panel.hw(), panel.bus());
        assert_eq!(panel.pmic_writes(), writes);
    }

    #[test]
    fn a_restart_during_a_scan_that_never_ends_still_takes_the_rails_down() {
        let mut panel = Panel::new();
        panel.engine.enqueue(1, update(Rect::FULL, GC16, true), 0);
        panel.service(0);
        rails_off_now(panel.hw(), panel.bus());
        panel.assert_all_off();
    }

    #[test]
    fn a_collision_with_a_lut_that_has_ended_goes_again_at_once() {
        let mut panel = Panel::new();
        after_init(&mut panel);
        panel
            .engine
            .enqueue(2, update(rect(0, 0, 100, 100), GL16, false), 50);
        panel.service(50);
        let first = panel.last_lut();
        panel.complete_working_buffer(false);
        panel.service(60);
        panel
            .engine
            .enqueue(3, update(rect(50, 50, 100, 100), GL16, false), 60);
        panel.service(60);
        let second = panel.last_lut();
        // The first ends and is handled while the second is in its pass, and only then does the pass report the collision.
        panel.finish_lut(first);
        panel.service(70);
        panel.collide_with(first, rect(50, 50, 50, 50));
        panel.complete_working_buffer(true);
        panel.service(80);
        assert_eq!(panel.started_rect(), rect(50, 50, 50, 50));
        panel.complete_working_buffer(false);
        panel.service(90);
        panel.finish_lut(second);
        panel.finish_lut(panel.last_lut());
        panel.service(100);
        assert!(panel.finished().iter().any(|f| f.marker == 3));
        assert!(!panel.engine.is_busy());
        panel.assert_all_off();
    }

    #[test]
    fn a_collided_part_that_never_goes_again_comes_down_in_the_end() {
        let mut panel = Panel::new();
        after_init(&mut panel);
        panel
            .engine
            .enqueue(2, update(rect(0, 0, 100, 100), GL16, false), 50);
        panel.service(50);
        let first = panel.last_lut();
        panel.complete_working_buffer(false);
        panel.service(60);
        panel
            .engine
            .enqueue(3, update(rect(50, 50, 100, 100), GL16, false), 60);
        panel.service(60);
        panel.collide_with(first, rect(50, 50, 50, 50));
        panel.complete_working_buffer(true);
        panel.service(70);
        panel.service(70 + STUCK_NS + 1);
        panel.assert_all_off();
        assert_eq!(panel.engine.stats().timeouts, 1);
    }

    #[test]
    fn a_collided_part_keeps_its_place_so_a_newer_update_wins_over_it() {
        let mut panel = Panel::new();
        after_init(&mut panel);
        panel
            .engine
            .enqueue(2, update(rect(0, 0, 100, 100), GL16, false), 50);
        panel.service(50);
        let first = panel.last_lut();
        panel.complete_working_buffer(false);
        panel.service(60);
        panel
            .engine
            .enqueue(3, update(rect(50, 50, 100, 100), GL16, false), 60);
        panel.service(60);
        let second = panel.last_lut();
        panel.collide_with(first, rect(50, 50, 50, 50));
        panel.complete_working_buffer(true);
        panel.service(70);
        // Marker 4 is newer than the collided part of marker 3.
        panel
            .engine
            .enqueue(4, update(rect(50, 50, 100, 100), GL16, false), 70);
        panel.finish_lut(first);
        panel.service(80);
        // The collided part of 3 goes first, ahead of the queue.
        assert_eq!(panel.started_rect(), rect(50, 50, 50, 50));
        let third = panel.last_lut();
        panel.complete_working_buffer(false);
        panel.service(90);
        let fourth = panel.last_lut();
        assert_ne!(third, fourth);
        panel.collide_with(second, rect(60, 60, 10, 10));
        panel.complete_working_buffer(true);
        panel.service(100);
        // 4 collided with 3's part, which is older: 4 goes again for that.
        panel.finish_lut(second);
        panel.finish_lut(third);
        panel.service(110);
        assert_eq!(panel.started_rect(), rect(60, 60, 10, 10));
    }

    #[test]
    fn a_write_that_fails_on_the_way_down_is_tried_again() {
        let mut panel = Panel::new();
        panel.engine.enqueue(1, update(Rect::FULL, GC16, true), 0);
        panel.service(0);
        panel.complete_working_buffer(false);
        panel.service(1);
        panel.finish_lut(panel.last_lut());
        panel.service(2);
        panel.complete_working_buffer(false);
        panel.service(3);
        panel.fail_pmic_writes(1);
        panel.finish_lut(panel.last_lut());
        panel.service(4);
        panel.assert_all_off();
        assert!(!panel.engine.is_busy());
    }

    #[test]
    fn rails_that_will_not_go_down_stay_the_engines_to_try_again() {
        let mut panel = Panel::new();
        panel.engine.enqueue(1, update(Rect::FULL, GC16, true), 0);
        panel.service(0);
        panel.complete_working_buffer(false);
        panel.service(1);
        panel.finish_lut(panel.last_lut());
        panel.service(2);
        panel.complete_working_buffer(false);
        panel.service(3);
        panel.fail_pmic_writes(1000);
        panel.finish_lut(panel.last_lut());
        panel.service(4);
        assert!(panel.engine.is_busy());
        assert_ne!(panel.pmic(0x60) & 0xc0, 0);
        panel.fail_pmic_writes(0);
        panel.service(5);
        panel.assert_all_off();
        assert!(!panel.engine.is_busy());
    }

    #[test]
    fn a_sleep_waits_for_the_update_and_finds_the_rails_down() {
        let mut panel = Panel::new();
        assert!(panel.engine.power_down_now());
        panel.engine.enqueue(1, update(Rect::FULL, GC16, true), 0);
        assert!(!panel.engine.power_down_now());
        panel.service(0);
        panel.complete_working_buffer(false);
        panel.service(1);
        assert!(!panel.engine.power_down_now());
        assert_ne!(panel.pmic(0x60) & 0xc0, 0);
    }

    #[test]
    fn rails_left_on_by_the_kernel_before_come_down_before_the_controller_is_reset() {
        let panel = Panel::started_with_rails_on();
        panel.assert_all_off();
        panel.assert_order(&[
            Event::Vcom(false),
            Event::HighVoltages(false),
            Event::Logic(false),
            Event::ControllerReset,
        ]);
    }

    #[test]
    fn no_more_than_16_luts_are_used_with_a_5_bit_waveform() {
        let mut panel = Panel::new();
        after_init(&mut panel);
        for marker in 2..30 {
            panel
                .engine
                .enqueue(marker, update(rect(marker * 2, 0, 1, 1), GL16, false), 50);
        }
        for time in 50..90 {
            panel.service(time);
            if panel.in_wb() {
                panel.complete_working_buffer(false);
            }
        }
        assert!(panel.max_lut_used() < 16);
        assert_eq!(panel.active_luts(), 16);
    }
}
