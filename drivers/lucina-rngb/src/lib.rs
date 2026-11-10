//! The RNGB, the random number block of the i.MX6 SoloLite, with the behaviour of Linux 7.1's `drivers/char/hw_random/imx-rngc.c`
//! and of Amazon's older `fsl-rngc.c` for the same silicon, written again rather than translated (SecretaryIOS
//! `docs/porting-guidelines.md`; GPL, so behaviour only). The kernel's side, the mapping and the crediting, is
//! `kernel/core/src/device/imx_rngb.rs`. Why it is rated as it is: `docs/research/juno-random.md`.
//!
//! Where it differs from Linux, on purpose:
//! - It polls where Linux waits for the interrupt, since this runs before anything has the interrupt. The block is unmasked
//!   during a command as in Linux, so that what the status says is what Linux's handler sees.
//! - Every wait has a limit, and so has the retry of a seed. Linux's loops have none, and a block that stays busy must not
//!   hold up a boot.
//! - As Amazon's driver, a block that already reports bad keys or a dead oscillator is refused before any command.
//! - As the Qualcomm driver in this kernel, a word of zero or the same word twice in a row counts as a failure.
#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

use core::fmt;

pub const VER_ID: usize = 0x00;
pub const COMMAND: usize = 0x04;
pub const CONTROL: usize = 0x08;
pub const STATUS: usize = 0x0c;
pub const ERROR: usize = 0x10;
pub const FIFO: usize = 0x14;

const TYPE_SHIFT: u32 = 28;
const TYPE_RNGB: u32 = 1;
const TYPE_RNGC: u32 = 2;

const CMD_CLR_ERR: u32 = 1 << 5;
const CMD_CLR_INT: u32 = 1 << 4;
const CMD_SEED: u32 = 1 << 1;
const CMD_SELF_TEST: u32 = 1 << 0;

const CTRL_MASK_ERROR: u32 = 1 << 6;
const CTRL_MASK_DONE: u32 = 1 << 5;
const CTRL_AUTO_SEED: u32 = 1 << 4;

const STATUS_ERROR: u32 = 1 << 16;
const STATUS_FIFO_LEVEL: u32 = 0xf << 8;
const STATUS_SEED_DONE: u32 = 1 << 5;
const STATUS_ST_DONE: u32 = 1 << 4;

const ERROR_BAD_KEY: u32 = 1 << 6;
const ERROR_STAT: u32 = 1 << 3;
const ERROR_OSC: u32 = 1 << 1;

/// Linux's `RNGC_SELFTEST_TIMEOUT` is 2500 us, rounded up to a jiffy by `usecs_to_jiffies`: 10 ms at its usual tick.
pub const SELF_TEST_TIMEOUT_US: u64 = 10_000;
/// `RNGC_SEED_TIMEOUT`.
pub const SEED_TIMEOUT_US: u64 = 200_000;
/// A word comes within a seed's time, even while the block makes a new seed by itself.
pub const WORD_TIMEOUT_US: u64 = SEED_TIMEOUT_US;
/// Linux repeats a seed for as long as there is a statistical error.
pub const SEED_TRIES: u32 = 8;

/// `rng.quality` of Linux's driver, in 1024ths of a bit per bit read. Its author's estimate: about 20,000 samples of the
/// true random source go into a seed, which then serves 2^20 words, and 20,000 * 1024 / 2^20 is about 19.
pub const LINUX_QUALITY: u32 = 19;

/// The registers and a clock.
pub trait Block {
    fn read(&mut self, offset: usize) -> u32;
    fn write(&mut self, offset: usize, value: u32);
    fn now_us(&mut self) -> u64;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// `VER_ID` says this is neither an RNGB nor an RNGC.
    NotThisBlock { ver_id: u32 },
    BadKeys,
    DeadOscillator,
    SelfTestTimeout,
    SelfTestFailed { error: u32 },
    SeedTimeout,
    SeedFailed { error: u32 },
    /// A statistical error on every try.
    SeedNeverClean,
    /// The status has its error bit up.
    Fault { error: u32 },
    FifoEmpty { status: u32 },
    ZeroWord,
    RepeatedWord,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotThisBlock { ver_id } => write!(f, "version {ver_id:#x} is not an RNGB or RNGC"),
            Error::BadKeys => write!(f, "it reports bad keys"),
            Error::DeadOscillator => write!(f, "its oscillator is dead"),
            Error::SelfTestTimeout => write!(f, "no self test result within {SELF_TEST_TIMEOUT_US} us"),
            Error::SelfTestFailed { error } => write!(f, "self test failed, error {error:#x}"),
            Error::SeedTimeout => write!(f, "no seed within {SEED_TIMEOUT_US} us"),
            Error::SeedFailed { error } => write!(f, "seed failed, error {error:#x}"),
            Error::SeedNeverClean => write!(f, "a statistical error on all {SEED_TRIES} seeds"),
            Error::Fault { error } => write!(f, "error {error:#x}"),
            Error::FifoEmpty { status } => write!(f, "no word within {WORD_TIMEOUT_US} us, status {status:#x}"),
            Error::ZeroWord => write!(f, "a word of zero"),
            Error::RepeatedWord => write!(f, "the same word twice in a row"),
        }
    }
}

/// What a start found, for the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Started {
    pub ver_id: u32,
    pub status_before: u32,
    pub error_before: u32,
    pub self_test_us: u64,
    pub seed_us: u64,
    pub seed_tries: u32,
}

/// `imx_rngc_irq_mask_clear`.
fn mask_and_clear<B: Block>(b: &mut B) {
    let control = b.read(CONTROL);
    b.write(CONTROL, control | CTRL_MASK_DONE | CTRL_MASK_ERROR);
    let command = b.read(COMMAND);
    b.write(COMMAND, command | CMD_CLR_INT | CMD_CLR_ERR);
}

fn unmask<B: Block>(b: &mut B) {
    let control = b.read(CONTROL);
    b.write(CONTROL, control & !(CTRL_MASK_DONE | CTRL_MASK_ERROR));
}

/// Gives a command and waits for its bit. Then, as Linux's handler: the error register first, since clearing clears it.
fn command<B: Block>(b: &mut B, bit: u32, done: u32, timeout_us: u64) -> Option<(u32, u64)> {
    unmask(b);
    let value = b.read(COMMAND);
    let started = b.now_us();
    b.write(COMMAND, value | bit);
    loop {
        if b.read(STATUS) & done != 0 {
            let took_us = b.now_us().wrapping_sub(started);
            let error = b.read(ERROR);
            mask_and_clear(b);
            return Some((error, took_us));
        }
        if b.now_us().wrapping_sub(started) > timeout_us {
            mask_and_clear(b);
            return None;
        }
        core::hint::spin_loop();
    }
}

/// `imx_rngc_probe` and `imx_rngc_init`: is it the block, self test, a seed (again after a statistical error), automatic
/// seeds from then on. Nothing is read after a wrong version.
pub fn start<B: Block>(b: &mut B) -> Result<Started, Error> {
    let ver_id = b.read(VER_ID);
    if !matches!(ver_id >> TYPE_SHIFT, TYPE_RNGB | TYPE_RNGC) {
        return Err(Error::NotThisBlock { ver_id });
    }
    let status_before = b.read(STATUS);
    let error_before = b.read(ERROR);
    if status_before & STATUS_ERROR != 0 && error_before & ERROR_BAD_KEY != 0 {
        return Err(Error::BadKeys);
    }
    if error_before & ERROR_OSC != 0 {
        return Err(Error::DeadOscillator);
    }
    mask_and_clear(b);

    let (error, self_test_us) =
        command(b, CMD_SELF_TEST, STATUS_ST_DONE, SELF_TEST_TIMEOUT_US).ok_or(Error::SelfTestTimeout)?;
    if error != 0 {
        return Err(Error::SelfTestFailed { error });
    }

    let mut seed_us = 0;
    let mut seed_tries = 0;
    loop {
        seed_tries += 1;
        let (error, took_us) = command(b, CMD_SEED, STATUS_SEED_DONE, SEED_TIMEOUT_US).ok_or(Error::SeedTimeout)?;
        seed_us += took_us;
        if error == 0 {
            break;
        }
        if error != ERROR_STAT {
            return Err(Error::SeedFailed { error });
        }
        if seed_tries == SEED_TRIES {
            return Err(Error::SeedNeverClean);
        }
    }

    let control = b.read(CONTROL);
    b.write(CONTROL, control | CTRL_AUTO_SEED);
    Ok(Started { ver_id, status_before, error_before, self_test_us, seed_us, seed_tries })
}

/// `imx_rngc_read`, with a limit on the wait. `last` is the word before, kept from one call to the next. An error after the
/// last word counts too, as in Amazon's driver: what came with it is not used.
pub fn fill<B: Block>(b: &mut B, out: &mut [u8], last: &mut u32) -> Result<(), Error> {
    for chunk in out.chunks_mut(4) {
        let word = next_word(b)?;
        if word == 0 {
            return Err(Error::ZeroWord);
        }
        if word == *last {
            return Err(Error::RepeatedWord);
        }
        *last = word;
        chunk.copy_from_slice(&word.to_ne_bytes()[..chunk.len()]);
    }
    if b.read(STATUS) & STATUS_ERROR != 0 {
        return Err(Error::Fault { error: b.read(ERROR) });
    }
    Ok(())
}

fn next_word<B: Block>(b: &mut B) -> Result<u32, Error> {
    let started = b.now_us();
    loop {
        let status = b.read(STATUS);
        if status & STATUS_ERROR != 0 {
            return Err(Error::Fault { error: b.read(ERROR) });
        }
        if status & STATUS_FIFO_LEVEL != 0 {
            return Ok(b.read(FIFO));
        }
        if b.now_us().wrapping_sub(started) > WORD_TIMEOUT_US {
            return Err(Error::FifoEmpty { status });
        }
        core::hint::spin_loop();
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;

    /// The block as the two drivers describe it. A read past the FIFO or of an empty FIFO fails the test: on the Kindle the
    /// first has hung the bus.
    struct Fake {
        ver_id: u32,
        control: u32,
        status: u32,
        error: u32,
        fifo: VecDeque<u32>,
        now: u64,
        /// What a self test leaves in the error register, or `None` if it never ends.
        self_test: Option<u32>,
        /// What each seed leaves there, the last one again and again; empty if a seed never ends.
        seeds: VecDeque<u32>,
        commands: Vec<u32>,
        unmasked_during_commands: bool,
        reads: u32,
        /// Words arrive in the FIFO only after this time.
        fifo_from: u64,
    }

    impl Fake {
        fn new() -> Self {
            Self {
                ver_id: (TYPE_RNGB << TYPE_SHIFT) | 0x0204,
                control: CTRL_MASK_DONE | CTRL_MASK_ERROR,
                status: 0,
                error: 0,
                fifo: VecDeque::new(),
                now: 1000,
                self_test: Some(0),
                seeds: VecDeque::from([0]),
                commands: Vec::new(),
                unmasked_during_commands: true,
                reads: 0,
                fifo_from: 0,
            }
        }

        fn status_now(&self) -> u32 {
            let visible = if self.now >= self.fifo_from { self.fifo.len().min(15) as u32 } else { 0 };
            self.status | (visible << 8)
        }
    }

    impl Block for Fake {
        fn read(&mut self, offset: usize) -> u32 {
            self.reads += 1;
            self.now += 1;
            match offset {
                VER_ID => self.ver_id,
                COMMAND => 0,
                CONTROL => self.control,
                STATUS => self.status_now(),
                ERROR => self.error,
                FIFO => {
                    assert!(self.status_now() & STATUS_FIFO_LEVEL != 0, "read of an empty FIFO");
                    self.fifo.pop_front().unwrap()
                }
                _ => panic!("read past the registers: {offset:#x}"),
            }
        }

        fn write(&mut self, offset: usize, value: u32) {
            self.now += 1;
            match offset {
                CONTROL => self.control = value,
                COMMAND => {
                    self.commands.push(value);
                    if value & (CMD_SELF_TEST | CMD_SEED) != 0 && self.control & (CTRL_MASK_DONE | CTRL_MASK_ERROR) != 0 {
                        self.unmasked_during_commands = false;
                    }
                    if value & CMD_CLR_ERR != 0 {
                        self.error = 0;
                        self.status &= !STATUS_ERROR;
                    }
                    if value & CMD_CLR_INT != 0 {
                        self.status &= !(STATUS_ST_DONE | STATUS_SEED_DONE);
                    }
                    if value & CMD_SELF_TEST != 0 {
                        if let Some(error) = self.self_test {
                            self.status |= STATUS_ST_DONE;
                            self.error = error;
                        }
                    }
                    if value & CMD_SEED != 0 {
                        if let Some(&error) = self.seeds.front() {
                            if self.seeds.len() > 1 {
                                self.seeds.pop_front();
                            }
                            self.status |= STATUS_SEED_DONE;
                            self.error = error;
                        }
                    }
                }
                _ => panic!("write to {offset:#x}"),
            }
        }

        fn now_us(&mut self) -> u64 {
            self.now += 10;
            self.now
        }
    }

    #[test]
    fn a_good_block_tests_itself_seeds_and_then_seeds_by_itself() {
        let mut b = Fake::new();
        let started = start(&mut b).unwrap();
        assert_eq!(started.seed_tries, 1);
        assert!(b.unmasked_during_commands);
        let first = |bit| b.commands.iter().position(|command| command & bit != 0).unwrap();
        assert!(first(CMD_SELF_TEST) < first(CMD_SEED));
        assert_ne!(b.control & CTRL_AUTO_SEED, 0);
        assert_eq!(b.control & (CTRL_MASK_DONE | CTRL_MASK_ERROR), CTRL_MASK_DONE | CTRL_MASK_ERROR);
        assert_eq!(b.status & (STATUS_ST_DONE | STATUS_SEED_DONE), 0);
    }

    #[test]
    fn an_rngc_counts_and_anything_else_is_left_alone_after_one_read() {
        let mut b = Fake::new();
        b.ver_id = TYPE_RNGC << TYPE_SHIFT;
        assert!(start(&mut b).is_ok());
        for ver_id in [0, 3 << TYPE_SHIFT, 0xffff_ffff] {
            let mut b = Fake::new();
            b.ver_id = ver_id;
            assert_eq!(start(&mut b), Err(Error::NotThisBlock { ver_id }));
            assert_eq!(b.reads, 1);
            assert!(b.commands.is_empty());
        }
    }

    #[test]
    fn bad_keys_and_a_dead_oscillator_stop_it_before_any_command() {
        let mut b = Fake::new();
        b.status = STATUS_ERROR;
        b.error = ERROR_BAD_KEY;
        assert_eq!(start(&mut b), Err(Error::BadKeys));
        assert!(b.commands.is_empty());
        let mut b = Fake::new();
        b.error = ERROR_OSC;
        assert_eq!(start(&mut b), Err(Error::DeadOscillator));
        assert!(b.commands.is_empty());
    }

    #[test]
    fn an_old_error_that_is_neither_is_cleared_and_does_not_count() {
        let mut b = Fake::new();
        b.status = STATUS_ERROR;
        b.error = 0x10;
        let started = start(&mut b).unwrap();
        assert_eq!(started.error_before, 0x10);
    }

    #[test]
    fn a_self_test_that_fails_or_never_ends_stops_it_without_a_seed() {
        let mut b = Fake::new();
        b.self_test = Some(4);
        assert_eq!(start(&mut b), Err(Error::SelfTestFailed { error: 4 }));
        assert!(!b.commands.iter().any(|command| command & CMD_SEED != 0));
        let mut b = Fake::new();
        b.self_test = None;
        assert_eq!(start(&mut b), Err(Error::SelfTestTimeout));
        assert!(!b.commands.iter().any(|command| command & CMD_SEED != 0));
        assert_ne!(b.control & CTRL_MASK_DONE, 0);
    }

    #[test]
    fn a_statistical_error_means_another_seed_and_any_other_error_the_end() {
        let mut b = Fake::new();
        b.seeds = VecDeque::from([ERROR_STAT, ERROR_STAT, 0]);
        assert_eq!(start(&mut b).unwrap().seed_tries, 3);
        assert_ne!(b.control & CTRL_AUTO_SEED, 0);
        assert!(b.unmasked_during_commands);

        let mut b = Fake::new();
        b.seeds = VecDeque::from([ERROR_STAT, 0x20]);
        assert_eq!(start(&mut b), Err(Error::SeedFailed { error: 0x20 }));
        assert_eq!(b.control & CTRL_AUTO_SEED, 0);
    }

    #[test]
    fn a_seed_that_is_never_clean_or_never_ends_gives_up() {
        let mut b = Fake::new();
        b.seeds = VecDeque::from([ERROR_STAT]);
        assert_eq!(start(&mut b), Err(Error::SeedNeverClean));
        assert_eq!(b.commands.iter().filter(|command| *command & CMD_SEED != 0).count() as u32, SEED_TRIES);
        assert_eq!(b.control & CTRL_AUTO_SEED, 0);

        let mut b = Fake::new();
        b.seeds = VecDeque::new();
        assert_eq!(start(&mut b), Err(Error::SeedTimeout));
        assert_eq!(b.control & CTRL_AUTO_SEED, 0);
    }

    #[test]
    fn the_fifo_gives_its_words_in_order_and_a_short_tail() {
        let mut b = Fake::new();
        b.fifo = VecDeque::from([0x0403_0201, 0x0807_0605, 0x0c0b_0a09]);
        let mut out = [0u8; 10];
        let mut last = 0;
        fill(&mut b, &mut out, &mut last).unwrap();
        assert_eq!(out, [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
        assert_eq!(last, 0x0c0b_0a09);
    }

    #[test]
    fn a_fifo_that_fills_late_is_waited_for_and_one_that_never_does_is_not() {
        let mut b = Fake::new();
        b.fifo = VecDeque::from([1]);
        b.fifo_from = b.now + 500;
        let mut out = [0u8; 4];
        fill(&mut b, &mut out, &mut 0).unwrap();

        let mut b = Fake::new();
        assert!(matches!(fill(&mut b, &mut out, &mut 0), Err(Error::FifoEmpty { .. })));
        assert!(b.now > WORD_TIMEOUT_US);
    }

    #[test]
    fn an_error_stops_a_read_before_the_fifo_and_after_the_last_word() {
        let mut b = Fake::new();
        b.fifo = VecDeque::from([1]);
        b.status = STATUS_ERROR;
        b.error = 0x10;
        let mut out = [0u8; 4];
        assert_eq!(fill(&mut b, &mut out, &mut 0), Err(Error::Fault { error: 0x10 }));
        assert_eq!(b.fifo.len(), 1);

        struct LateError(Fake);
        impl Block for LateError {
            fn read(&mut self, offset: usize) -> u32 {
                let value = self.0.read(offset);
                if offset == FIFO {
                    self.0.status |= STATUS_ERROR;
                    self.0.error = 0x20;
                }
                value
            }
            fn write(&mut self, offset: usize, value: u32) {
                self.0.write(offset, value);
            }
            fn now_us(&mut self) -> u64 {
                self.0.now_us()
            }
        }
        let mut b = LateError(Fake::new());
        b.0.fifo = VecDeque::from([1]);
        assert_eq!(fill(&mut b, &mut out, &mut 0), Err(Error::Fault { error: 0x20 }));
    }

    #[test]
    fn a_stuck_or_zero_word_fails_also_across_two_reads() {
        let mut out = [0u8; 8];
        let mut b = Fake::new();
        b.fifo = VecDeque::from([5, 5]);
        assert_eq!(fill(&mut b, &mut out, &mut 0), Err(Error::RepeatedWord));

        let mut b = Fake::new();
        b.fifo = VecDeque::from([0, 5]);
        assert_eq!(fill(&mut b, &mut out, &mut 0), Err(Error::ZeroWord));

        let mut b = Fake::new();
        b.fifo = VecDeque::from([1, 2, 2, 3]);
        let mut last = 0;
        let mut half = [0u8; 8];
        fill(&mut b, &mut half, &mut last).unwrap();
        assert_eq!(fill(&mut b, &mut half, &mut last), Err(Error::RepeatedWord));
    }
}
