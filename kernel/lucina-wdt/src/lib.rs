//! The logic of Juno's watchdogs, without the machine: the kernel's `device/watchdog.rs` has the threads and the MMIO.
//!
//! - [`qcom`]: the APSS watchdog of Qualcomm SoCs, behaviour from Linux's `drivers/watchdog/qcom-wdt.c`.
//! - [`Lockup`]: when a CPU counts as stuck, behaviour from Linux's `kernel/watchdog.c`.
//! - [`UserPromise`]: what `/dev/watchdog` promises, behaviour from Linux's `drivers/watchdog/watchdog_dev.c`.
//! - [`rules::bark_target`]: which CPU takes the bark, never the boot CPU that pets (patch 1185, proved by Verus).
#![no_std]
#![forbid(unsafe_code)]

pub mod rules;

pub mod qcom {
    //! The registers of `qcom,kpss-wdt`, which `qcom,apss-wdt-sc7180` is.

    /// Writing 1 restarts the count: a pet.
    pub const RST: usize = 0x4;
    /// Bit 0 enables it.
    pub const EN: usize = 0x8;
    /// Bit 0 says that the last reset came from this watchdog.
    pub const STS: usize = 0xc;
    /// The count at which it raises its interrupt.
    pub const BARK_TIME: usize = 0x10;
    /// The count at which it resets the machine.
    pub const BITE_TIME: usize = 0x14;

    pub const ENABLE: u32 = 1;
    /// The widest count the registers take.
    pub const MAX_TICKS: u32 = 0xf_ffff;
    /// The rate of the QEMU stand-in's clock (`scripts/qemu-crash.sh`, `examples/qemu-model.rs`). The laptop's device tree
    /// gives its sleep clock as 32764 Hz; with no rate in the tree the kernel leaves the watchdog alone, as Linux's probe fails.
    pub const DEFAULT_RATE: u32 = 32768;
    /// The bark's lead on the bite. Linux's is its `pretimeout`, 1 s, for a panic that writes nothing. Ours is 5 s, ours: the
    /// bark ends in the crash code, which builds the record, draws the screen and saves the log before its first pet, and a
    /// bite in that time loses the record. Proven on the laptop with 8 CPUs on 2026-11-02 (`docs/research/crash-recovery.md`).
    pub const BARK_LEAD_S: u32 = 5;

    /// Linux's `qcom_wdt_probe`: a clock rate of 0, or one above the widest count, is refused, and there is no watchdog.
    pub fn rate_ok(rate: u32) -> bool {
        rate != 0 && rate <= MAX_TICKS
    }

    /// A register's value in one line.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct State {
        pub en: u32,
        pub sts: u32,
        pub bark: u32,
        pub bite: u32,
    }

    impl State {
        /// Whether it counts at the moment. Then somebody must pet it, and it was not us.
        pub fn running(&self) -> bool {
            self.en & ENABLE != 0
        }

        /// Whether the last reset came from this watchdog.
        pub fn caused_last_reset(&self) -> bool {
            self.sts & 1 != 0
        }
    }

    /// The counts for a clock rate. The bite as Linux's probe sets the timeout, `min(max_timeout, 30)` seconds with
    /// `max_timeout = MAX_TICKS / rate` (at least 2 s here, so that a bark fits before it); the bark [`BARK_LEAD_S`] before
    /// it. At 32764 Hz the widest count is just under 32 s.
    pub fn counts(rate: u32) -> (u32, u32) {
        let rate = rate.max(1);
        let bite_s = (MAX_TICKS / rate).clamp(2, 30);
        let bark_s = bite_s.saturating_sub(BARK_LEAD_S).max(1);
        let ticks = |seconds: u32| seconds.saturating_mul(rate).min(MAX_TICKS);
        (ticks(bark_s), ticks(bite_s))
    }

    /// Register writes to start it, in order, as Linux's `qcom_wdt_start`.
    pub fn start_sequence(rate: u32) -> [(usize, u32); 5] {
        let (bark, bite) = counts(rate);
        [
            (EN, 0),
            (RST, 1),
            (BARK_TIME, bark),
            (BITE_TIME, bite),
            (EN, ENABLE),
        ]
    }
}

/// How long a CPU's watchdog thread may stay away.
#[derive(Clone, Copy, Debug)]
pub struct Lockup {
    /// Counter ticks after which a CPU counts as stuck.
    pub threshold: u64,
    /// A gap between two timer interrupts of a CPU beyond this many counter ticks is not a lockup of the others: the
    /// clock jumped, the virtual machine was paused, or this CPU itself had interrupts off.
    pub clock_jump: u64,
}

/// What a timer interrupt found.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Verdict {
    Fine,
    /// The time was not running as it should, for every CPU (a suspend, a paused virtual machine): start counting again
    /// from now, for all.
    StartOver,
    /// Only this CPU's timer came late, and the others were checked and are fine: its own thread's time is no evidence
    /// (it could not run either), so only its entry starts over (patch 1183).
    ResetOwn,
    /// This CPU, by index, has been stuck for this many counter ticks.
    Stuck {
        cpu: usize,
        for_ticks: u64,
    },
}

impl Lockup {
    /// Decides on a timer interrupt of CPU `this_cpu`. `last_tick` is its previous interrupt (0 for none), `others_latest`
    /// the latest interrupt of any other CPU (0 for none), `touched` the time each CPU's thread last ran (0 for not yet).
    ///
    /// A late timer on this CPU wipes every CPU's time only if no other CPU ticked within `clock_jump` either: time did
    /// not run (a suspend). If the others did tick, only this CPU was deaf, and the others are still checked. Wiping all
    /// on one CPU's late timer hid a stuck CPU 0 until the hardware watchdog barked (boot 138, 2026-11-08; juno-concurrency's
    /// TLA+ model, Witness_all_late; the rule here is its Fixed_global_late and Fixed_global_suspend).
    pub fn check(
        &self,
        now: u64,
        last_tick: u64,
        others_latest: u64,
        this_cpu: usize,
        touched: impl Iterator<Item = u64>,
    ) -> Verdict {
        let mut own_late = false;
        if last_tick != 0 && now.saturating_sub(last_tick) > self.clock_jump {
            if others_latest == 0 || now.saturating_sub(others_latest) > self.clock_jump {
                return Verdict::StartOver;
            }
            own_late = true;
        }
        for (cpu, time) in touched.enumerate() {
            if own_late && cpu == this_cpu {
                continue;
            }
            let away = now.saturating_sub(time);
            if time != 0 && away > self.threshold {
                return Verdict::Stuck {
                    cpu,
                    for_ticks: away,
                };
            }
        }
        if own_late { Verdict::ResetOwn } else { Verdict::Fine }
    }
}

/// The promise of a program that opened `/dev/watchdog`: to write to it in time. Times are in milliseconds. The kernel asks
/// [`UserPromise::expired`] regularly and pets the hardware meanwhile, since the hardware's own limit is much shorter than a
/// program's.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UserPromise {
    timeout_ms: u64,
    open: bool,
    /// Armed: there is a promise, also after a close that did not give it back.
    armed: bool,
    magic_seen: bool,
    last_ping_ms: u64,
}

/// `/dev/watchdog` is open already.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Busy;

impl UserPromise {
    pub const DEFAULT_TIMEOUT_S: u32 = 60;

    pub const fn new() -> Self {
        Self {
            timeout_ms: Self::DEFAULT_TIMEOUT_S as u64 * 1000,
            open: false,
            armed: false,
            magic_seen: false,
            last_ping_ms: 0,
        }
    }

    /// One opener at a time, as on Linux. Opening counts as a ping.
    pub fn open(&mut self, now_ms: u64) -> Result<(), Busy> {
        if self.open {
            return Err(Busy);
        }
        self.open = true;
        self.armed = true;
        self.magic_seen = false;
        self.last_ping_ms = now_ms;
        Ok(())
    }

    /// A write pings. A `V` in it means that the next close ends the promise; a later write without one takes that back,
    /// as on Linux.
    pub fn write(&mut self, data: &[u8], now_ms: u64) {
        self.magic_seen = data.contains(&b'V');
        self.last_ping_ms = now_ms;
    }

    pub fn ping(&mut self, now_ms: u64) {
        self.last_ping_ms = now_ms;
    }

    /// Without the `V` the promise stays: a program that died must not end it by dying. Returns whether it did.
    pub fn close(&mut self) -> bool {
        self.open = false;
        if self.magic_seen {
            self.armed = false;
        }
        !self.armed
    }

    /// Sets the time limit in seconds, as `WDIOC_SETTIMEOUT`, which also pings.
    pub fn set_timeout(&mut self, seconds: u32, now_ms: u64) {
        self.timeout_ms = u64::from(seconds.max(1)) * 1000;
        self.last_ping_ms = now_ms;
    }

    pub fn timeout_s(&self) -> u32 {
        (self.timeout_ms / 1000) as u32
    }

    /// Whether the promise was broken.
    pub fn expired(&self, now_ms: u64) -> bool {
        self.armed && now_ms.saturating_sub(self.last_ping_ms) > self.timeout_ms
    }
}

impl Default for UserPromise {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{qcom::*, *};

    #[test]
    fn the_bark_never_goes_to_the_boot_cpu_and_stays_put_while_it_can() {
        use super::rules::bark_target;
        // Boot CPU 0; CPU 1 ran at 90, CPU 2 at 100, CPU 3 never.
        let touched = [100, 90, 100, 0];
        // No previous target: the freshest other than 0.
        assert_eq!(bark_target(0, usize::MAX, &touched, 100, 20), Some(2));
        // A previous target that is still fresh stays, though another ran later.
        assert_eq!(bark_target(0, 1, &touched, 100, 20), Some(1));
        // The boot CPU is never kept, nor one that never ran.
        assert_eq!(bark_target(0, 0, &touched, 100, 20), Some(2));
        assert_eq!(bark_target(0, 3, &touched, 100, 20), Some(2));
        // Too old: CPU 1 is out, CPU 2 is in.
        assert_eq!(bark_target(0, 1, &touched, 115, 15), Some(2));
        // Nobody else ran lately: stay where it was.
        assert_eq!(bark_target(0, 1, &touched, 200, 10), None);
        assert_eq!(bark_target(0, usize::MAX, &[5], 5, 10), None);
        // A thread that ran just after `now` was read (another CPU's counter) is fresh.
        assert_eq!(bark_target(0, usize::MAX, &[0, 101], 100, 1), Some(1));
    }

    #[test]
    fn rates_are_refused_as_linux_refuses_them() {
        // qcom_wdt_probe: `if (!wdt->rate || wdt->rate > data->max_tick_count)`.
        assert!(!rate_ok(0));
        assert!(rate_ok(1));
        assert!(rate_ok(32764));
        assert!(rate_ok(MAX_TICKS));
        assert!(!rate_ok(MAX_TICKS + 1));
        assert!(!rate_ok(19_200_000));
    }

    #[test]
    fn counts_fit_the_registers_at_real_clock_rates() {
        // sc7180's sleep clock is 32764 or 32768 Hz.
        let (bark, bite) = counts(32768);
        assert_eq!((bark / 32768, bite / 32768), (25, 30));
        assert!(bite <= MAX_TICKS);
        let (bark, bite) = counts(32764);
        assert_eq!((bark / 32764, bite / 32764), (25, 30));
        // A faster clock: the widest count is the limit.
        let (bark, bite) = counts(100_000);
        assert_eq!(bite / 100_000, 10);
        assert_eq!(bark / 100_000, 5);
        assert!(bite <= MAX_TICKS);
        // A clock too fast for whole seconds still gives counts within the registers.
        let (bark, bite) = counts(19_200_000);
        assert!(bark <= bite && bite <= MAX_TICKS);
        // A clock of nothing does not divide by zero.
        assert!(counts(0).1 > 0);
    }

    #[test]
    fn start_sequence_is_linuxs() {
        let (bark, bite) = counts(32768);
        assert_eq!(
            start_sequence(32768),
            [(0x8, 0), (0x4, 1), (0x10, bark), (0x14, bite), (0x8, 1)]
        );
    }

    #[test]
    fn state_reads() {
        let state = State {
            en: 1,
            sts: 1,
            bark: 0,
            bite: 0,
        };
        assert!(state.running() && state.caused_last_reset());
        let state = State {
            en: 0,
            sts: 0,
            bark: 0,
            bite: 0,
        };
        assert!(!state.running() && !state.caused_last_reset());
    }

    const LOCKUP: Lockup = Lockup {
        threshold: 20_000,
        clock_jump: 5_000,
    };

    #[test]
    fn a_cpu_whose_thread_ran_is_fine() {
        assert_eq!(
            LOCKUP.check(30_000, 29_999, 29_999, 0, [15_000u64].into_iter()),
            Verdict::Fine
        );
    }

    #[test]
    fn a_cpu_whose_thread_stays_away_is_stuck() {
        assert_eq!(
            LOCKUP.check(30_000, 29_999, 29_999, 0, [5_000u64].into_iter()),
            Verdict::Stuck {
                cpu: 0,
                for_ticks: 25_000
            }
        );
    }

    #[test]
    fn another_cpu_is_seen_stuck() {
        assert_eq!(
            LOCKUP.check(30_000, 29_999, 29_999, 0, [29_000u64, 1_000].into_iter()),
            Verdict::Stuck {
                cpu: 1,
                for_ticks: 29_000
            }
        );
    }

    #[test]
    fn a_thread_not_started_yet_does_not_count() {
        assert_eq!(
            LOCKUP.check(30_000, 29_999, 29_999, 0, [0u64].into_iter()),
            Verdict::Fine
        );
    }

    #[test]
    fn a_clock_jump_starts_over() {
        assert_eq!(
            LOCKUP.check(90_000, 30_000, 30_000, 0, [29_000u64].into_iter()),
            Verdict::StartOver
        );
        // The first tick has no gap to measure.
        assert_eq!(
            LOCKUP.check(90_000, 0, 0, 0, [80_000u64].into_iter()),
            Verdict::Fine
        );
    }

    /// Boot 138: CPU 0 stuck, CPU 1's own timer 7 s late while CPU 2 ticks on. CPU 1's check must not wipe CPU 0's time.
    #[test]
    fn one_late_timer_does_not_hide_a_stuck_cpu() {
        // CPU 1 checks at 30 s: its last tick 23 s (7 s late), CPU 2 ticked at 29.9 s, CPU 0's thread last ran at 5 s.
        assert_eq!(
            LOCKUP.check(30_000, 23_000, 29_900, 1, [5_000u64, 23_000, 29_000].into_iter()),
            Verdict::Stuck { cpu: 0, for_ticks: 25_000 }
        );
        // Nobody else stuck: only this CPU's entry starts over.
        assert_eq!(
            LOCKUP.check(30_000, 23_000, 29_900, 1, [29_000u64, 23_000, 29_000].into_iter()),
            Verdict::ResetOwn
        );
        // Its own stale time is not counted against it then.
        assert_eq!(
            LOCKUP.check(30_000, 4_000, 29_900, 1, [29_000u64, 4_000, 29_000].into_iter()),
            Verdict::ResetOwn
        );
    }

    /// A whole-machine time jump (a suspend): every CPU's timer is late, so all start over, and nobody is reported.
    #[test]
    fn a_suspend_still_starts_everyone_over() {
        assert_eq!(
            LOCKUP.check(90_000, 30_000, 29_900, 1, [29_000u64, 30_000].into_iter()),
            Verdict::StartOver
        );
    }

    #[test]
    fn a_counter_that_goes_back_is_no_lockup() {
        assert_eq!(LOCKUP.check(10, 20, 20, 0, [30u64].into_iter()), Verdict::Fine);
    }

    #[test]
    fn a_promise_holds_while_pinged() {
        let mut p = UserPromise::new();
        assert!(!p.expired(1_000_000), "no promise before an open");
        p.open(0).unwrap();
        assert!(!p.expired(60_000));
        assert!(p.expired(60_001));
        p.ping(60_001);
        assert!(!p.expired(120_000));
        p.write(b"x", 120_000);
        assert!(!p.expired(180_000));
    }

    #[test]
    fn one_opener_at_a_time() {
        let mut p = UserPromise::new();
        p.open(0).unwrap();
        assert_eq!(p.open(0), Err(Busy));
        p.close();
        p.open(0).unwrap();
    }

    #[test]
    fn a_close_without_the_magic_keeps_the_promise() {
        let mut p = UserPromise::new();
        p.open(0).unwrap();
        p.write(b"ping", 10);
        assert!(!p.close());
        assert!(p.expired(61_000), "the program died and nobody pings");
        // A new program can take it over.
        p.open(61_000).unwrap();
        assert!(!p.expired(61_001));
    }

    #[test]
    fn the_magic_ends_the_promise() {
        let mut p = UserPromise::new();
        p.open(0).unwrap();
        p.write(b"V", 10);
        assert!(p.close());
        assert!(!p.expired(1_000_000));
    }

    #[test]
    fn a_write_after_the_magic_takes_it_back() {
        let mut p = UserPromise::new();
        p.open(0).unwrap();
        p.write(b"V", 10);
        p.write(b"x", 20);
        assert!(!p.close());
        assert!(p.expired(61_000));
    }

    #[test]
    fn the_timeout_can_be_set_and_pings() {
        let mut p = UserPromise::new();
        p.open(0).unwrap();
        p.set_timeout(5, 59_000);
        assert_eq!(p.timeout_s(), 5);
        assert!(!p.expired(64_000));
        assert!(p.expired(64_001));
        p.set_timeout(0, 64_001);
        assert_eq!(p.timeout_s(), 1);
    }
}
