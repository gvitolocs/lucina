// SPDX-License-Identifier: MPL-2.0

//! The display's scanout switched off for a sleep and back on for the wake, without its registers: the timing engine of
//! the DPU's DSI interface, which is what makes the panel read the framebuffer.
//!
//! The Galaxy Book Go's pipeline is programmed by the firmware and the kernel only keeps the picture in memory, so the one
//! thing a sleep can do about the scanout is stop the engine that reads it and start it again afterwards. Linux does the
//! same in its video-mode encoder, and this is that order: the disable write, then the wait for the frame that is in
//! flight to finish, then - on the way back - the enable write with no wait, because the configuration it scans from never
//! changed.
//!
//! The state here is only "this hook wrote the disable and has not written the enable since". It is what makes both steps
//! safe to call twice: a resume that comes from a half-done sleep and a resume that comes after the same work was already
//! done in an earlier phase both end in one enable write, or in none at all.

/// `INTF_1`'s offset in the DPU's `mdp` region and the length of its page, as our own scan code maps them
/// (`kernel/core/src/device/qcom_gpu/scan.rs:27-28`, Linux's `catalog/dpu_6_2_sc7180.h:143-144`: `intf_1 .base = 0x6a800`,
/// `.len = 0x2c0`). The length is the one already mapped, and mapping the same range twice is the point: it is the only way
/// a second reader of this block gets a window, since `IoMem::acquire` refuses a range that overlaps one it handed out
/// before.
pub const INTF_1: usize = 0x6a800;
pub const INTF_LEN: usize = 0x300;
/// `INTF_TIMING_ENGINE_EN` (`dpu_hw_intf.c:17`): the engine runs while it is not zero, and the write is all that
/// `dpu_hw_intf_enable_timing_engine` does (`dpu_hw_intf.c:256-263`).
pub const TIMING_ENGINE_EN: u32 = 0x000;
/// `INTF_STATUS` (`dpu_hw_intf.c:71`) and its bit 0, the running flag `dpu_hw_intf_get_status` reads on DPU 5 and later
/// (`dpu_hw_intf.c:307-315`, and our `qcom_gpu/scan.rs:34-35`).
pub const INTF_STATUS: u32 = 0x26c;
pub const STATUS_RUNNING: u32 = 1 << 0;
/// How long the engine gets to latch a change: Linux's `KICKOFF_TIMEOUT_MS`, 84 ms, which is a bit over five frames of
/// this panel and is what `wait_for_tx_complete` waits with (`dpu_encoder_phys.h:27`, `dpu_encoder_phys_vid.c:497-522`).
pub const LATCH_LIMIT_US: u64 = 84_000;
/// How the wait gives the CPU away between two reads of the status. Linux waits on an interrupt; this is a poll, and the
/// panel's own frame is 16 ms.
pub const POLL_US: u64 = 1_000;

/// The engine's page, read and written as words.
pub trait Regs {
    fn read(&mut self, offset: u32) -> u32;
    fn write(&mut self, offset: u32, value: u32);
}

/// Time, so that a test watches a wait instead of sleeping through it.
pub trait Pace {
    fn now_us(&mut self) -> u64;

    /// Give the CPU away for about `us` microseconds.
    fn rest(&mut self, us: u64);
}

/// What the way down did to the engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// The status did not say the engine was scanning: nothing read beyond that, nothing written, and nothing for the
    /// wake to put back.
    WasOff,
    /// This hook stopped it already and the enable has not been written since.
    AlreadyStopped,
    /// The disable is written and the frame in flight finished: the status says stopped.
    Stopped { waited_us: u64 },
    /// The disable is written but the status still says running after [`LATCH_LIMIT_US`]: Linux logs the same and goes on
    /// (`dpu_encoder_phys_vid.c:612-617`). The enable is ours to write back either way.
    StillRunning { waited_us: u64 },
}

/// What the way back did to the engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Start {
    /// This hook never stopped it, or an earlier phase already started it: nothing written.
    Untouched,
    /// The enable is written and read back as `enable`; `running` is what the status said, reached in `waited_us`.
    Back {
        enable: u32,
        running: bool,
        waited_us: u64,
    },
}

/// The engine page, with the one bit of state that says whether this hook owns the disable.
pub struct Engine<R, P> {
    regs: R,
    pace: P,
    stopped: bool,
}

impl<R: Regs, P: Pace> Engine<R, P> {
    pub fn new(regs: R, pace: P) -> Self {
        Self {
            regs,
            pace,
            stopped: false,
        }
    }

    /// Whether a [`start`] still has an enable to write.
    pub fn stopped(&self) -> bool {
        self.stopped
    }

    /// Whether the status says the panel is being scanned out (`dpu_hw_intf.c:315`).
    pub fn running(&mut self) -> bool {
        self.regs.read(INTF_STATUS) & STATUS_RUNNING != 0
    }

    /// Stop the scanout: `dpu_encoder_phys_vid_disable` (`dpu_encoder_phys_vid.c:571-642`) for one engine that the
    /// firmware set up and that nothing else here has touched.
    pub fn stop(&mut self) -> Stop {
        if self.stopped {
            // Linux refuses it the same way, from its own `enable_state` (`dpu_encoder_phys_vid.c:591-594`), and the
            // same reason holds: a second disable is a write whose effect nobody is waiting for.
            return Stop::AlreadyStopped;
        }
        if !self.running() {
            return Stop::WasOff;
        }
        // `intf->ops.enable_timing(intf, 0)`, `dpu_encoder_phys_vid.c:597`.
        self.regs.write(TIMING_ENGINE_EN, 0);
        self.stopped = true;
        // Then the wait for a vsync, so that the disable latches before anything that depends on the frame goes away
        // (`dpu_encoder_phys_vid.c:602-618`), the status read (`:620-621`) and the second wait if it still says running
        // (`:627-638`). Linux waits on the vsync interrupt; that line belongs to the scan code of 1343, so the same
        // condition is polled from the same register here.
        let (running, waited_us) = self.settle(false);
        if running {
            Stop::StillRunning { waited_us }
        } else {
            Stop::Stopped { waited_us }
        }
    }

    /// Start it again, as `dpu_encoder_phys_vid_handle_post_kickoff` does (`dpu_encoder_phys_vid.c:644-661`, the write
    /// at `:657`). Linux does not wait there: the enable lands with the next frame it kicks. Here the frame in memory is
    /// the one that was there before the sleep, so the picture returns as it was; the status is read anyway, because a
    /// wake that left the engine off is a black screen and the log line should say so.
    pub fn start(&mut self) -> Start {
        if !self.stopped {
            return Start::Untouched;
        }
        self.regs.write(TIMING_ENGINE_EN, 1);
        self.stopped = false;
        let (running, waited_us) = self.settle(true);
        Start::Back {
            enable: self.regs.read(TIMING_ENGINE_EN),
            running,
            waited_us,
        }
    }

    /// Read the status until it says `want`, resting [`POLL_US`] between two reads and never longer than
    /// [`LATCH_LIMIT_US`]. Returns whether it got there, and how long that took.
    fn settle(&mut self, want: bool) -> (bool, u64) {
        let started = self.pace.now_us();
        loop {
            let running = self.running();
            if running == want {
                return (running, self.pace.now_us() - started);
            }
            let waited = self.pace.now_us() - started;
            if waited >= LATCH_LIMIT_US {
                return (running, waited);
            }
            self.pace.rest(POLL_US.min(LATCH_LIMIT_US - waited));
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use std::vec::Vec;

    use super::*;

    /// The engine page as the hardware answers. The enable takes effect straight away, but the status bit only follows
    /// it [`latch_delay`](Hw::latch_delay) reads later, which is what a frame that is in flight does. `stubborn` is a
    /// block that never answers at all.
    struct Hw {
        en: u32,
        status_running: bool,
        latch_delay: u32,
        /// The state an enable write will land in, and the reads left before it does.
        pending: Option<bool>,
        latch_reads: u32,
        stubborn: bool,
        writes: Vec<(u32, u32)>,
        reads: Vec<u32>,
    }

    impl Hw {
        fn scanning() -> Self {
            Self {
                en: 1,
                status_running: true,
                latch_delay: 2,
                pending: None,
                latch_reads: 0,
                stubborn: false,
                writes: Vec::new(),
                reads: Vec::new(),
            }
        }

        /// A block whose status never changes: what a write that does not latch reads like.
        fn stubborn() -> Self {
            let mut hw = Self::scanning();
            hw.stubborn = true;
            hw
        }

        fn engine(self) -> Engine<Self, Time> {
            Engine::new(self, Time::default())
        }
    }

    impl Regs for Hw {
        fn read(&mut self, offset: u32) -> u32 {
            self.reads.push(offset);
            if offset != INTF_STATUS {
                return self.en;
            }
            if let Some(target) = self.pending {
                self.latch_reads -= 1;
                if self.latch_reads == 0 {
                    self.pending = None;
                    self.status_running = target;
                }
            }
            u32::from(self.status_running)
        }

        fn write(&mut self, offset: u32, value: u32) {
            self.writes.push((offset, value));
            if offset != TIMING_ENGINE_EN || self.stubborn {
                return;
            }
            self.en = value;
            self.pending = Some(value != 0);
            self.latch_reads = self.latch_delay;
        }
    }

    /// A clock that only moves when the wait does, so a wait is counted in rests taken.
    #[derive(Default)]
    struct Time {
        now: u64,
        rests: Vec<u64>,
    }

    impl Pace for Time {
        fn now_us(&mut self) -> u64 {
            self.now
        }

        fn rest(&mut self, us: u64) {
            self.rests.push(us);
            self.now += us;
        }
    }

    #[test]
    fn a_scanning_engine_is_stopped_by_one_write_to_the_enable() {
        let mut engine = Hw::scanning().engine();
        assert_eq!(engine.stop(), Stop::Stopped { waited_us: 1_000 });
        assert!(engine.stopped());
        assert_eq!(engine.regs.writes, [(TIMING_ENGINE_EN, 0)]);
    }

    #[test]
    fn an_engine_that_is_not_scanning_is_left_alone() {
        let mut hw = Hw::scanning();
        hw.status_running = false;
        let mut engine = hw.engine();
        assert_eq!(engine.stop(), Stop::WasOff);
        assert!(!engine.stopped());
        assert!(engine.regs.writes.is_empty());
        // and the wake has nothing to put back
        assert_eq!(engine.start(), Start::Untouched);
        assert!(engine.regs.writes.is_empty());
    }

    #[test]
    fn a_disable_that_never_latches_gives_up_at_the_frame_limit() {
        let mut engine = Hw::stubborn().engine();
        // The status says running and stays running: the wait ends at the limit, not before.
        let stop = engine.stop();
        assert!(matches!(stop, Stop::StillRunning { waited_us } if waited_us >= LATCH_LIMIT_US));
        assert_eq!(engine.regs.writes, [(TIMING_ENGINE_EN, 0)]);
        assert!(engine.stopped());
        assert!(engine.pace.rests.len() >= (LATCH_LIMIT_US / POLL_US) as usize - 1);
    }

    #[test]
    fn stopping_twice_writes_the_disable_once() {
        let mut engine = Hw::scanning().engine();
        engine.stop();
        assert_eq!(engine.stop(), Stop::AlreadyStopped);
        assert_eq!(engine.regs.writes, [(TIMING_ENGINE_EN, 0)]);
    }

    #[test]
    fn the_enable_is_written_and_read_back_on_the_way_up() {
        let mut engine = Hw::scanning().engine();
        engine.stop();
        let writes = engine.regs.writes.len();
        assert_eq!(
            engine.start(),
            Start::Back {
                enable: 1,
                running: true,
                waited_us: 1_000
            }
        );
        assert!(!engine.stopped());
        assert_eq!(engine.regs.writes[writes..], [(TIMING_ENGINE_EN, 1)]);
    }

    #[test]
    fn a_second_resume_phase_writes_nothing() {
        // The framework can call the enable from two phases; the second one must be the `Untouched` one.
        let mut engine = Hw::scanning().engine();
        engine.stop();
        assert!(matches!(engine.start(), Start::Back { .. }));
        assert_eq!(engine.start(), Start::Untouched);
        assert_eq!(
            engine.regs.writes,
            [(TIMING_ENGINE_EN, 0), (TIMING_ENGINE_EN, 1)]
        );
    }

    #[test]
    fn nothing_but_the_enable_is_ever_written() {
        let mut engine = Hw::scanning().engine();
        engine.stop();
        engine.start();
        engine.stop();
        engine.start();
        assert!(
            engine
                .regs
                .writes
                .iter()
                .all(|&(offset, value)| offset == TIMING_ENGINE_EN && value < 2)
        );
        assert!(
            engine
                .regs
                .reads
                .iter()
                .all(|&offset| offset == INTF_STATUS || offset == TIMING_ENGINE_EN)
        );
    }
}
