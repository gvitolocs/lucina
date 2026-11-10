//! When the generator counts as seeded, and who may have output: `_credit_init_bits`, `crng_reseed_interval`,
//! `sys_getrandom` and `try_to_generate_entropy` of Linux's `random.c`, as pure functions.
//!
//! Verus proves these in this very file, not a copy of it (`verification/juno-random/rules.rs`; the attributes are only seen
//! by Verus). What each promises is in its `ensures`. Not proved: that the kernel asks [`gate`] on every path, and how good
//! a source is, which is why a credit is a decision and the decisions are in `docs/research/juno-random.md`.
#![allow(unexpected_cfgs)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// What the pool's hash holds at most: `POOL_BITS`, BLAKE2s's output.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const POOL_BITS: u32 = 256;
/// The generator counts as seeded at this many credited bits.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const POOL_READY_BITS: u32 = 256;
/// Half of it: enough to stop handing out the bare pool, not enough to count as seeded.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const POOL_EARLY_BITS: u32 = 128;

#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Init {
    /// `CRNG_EMPTY`: under [`POOL_EARLY_BITS`] credited.
    Empty,
    /// `CRNG_EARLY`: at least [`POOL_EARLY_BITS`].
    Early,
    /// `CRNG_READY`: at least [`POOL_READY_BITS`], from now until the machine stops.
    Ready,
}

/// `input_pool.init_bits` and `crng_init`.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Seeding {
    pub bits: u32,
    pub init: Init,
}

/// What the generator must do after a credit.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Then {
    Nothing,
    /// Crossed [`POOL_EARLY_BITS`]: take a first key from the pool.
    FirstKey,
    /// Crossed [`POOL_READY_BITS`]: reseed, and wake everyone who waits.
    Reseed,
}

#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Credited {
    pub seeding: Seeding,
    pub then: Then,
}

/// Who asks for output.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    /// `getrandom` without flags, or with `GRND_RANDOM`; a `read` of `/dev/random`.
    Waits,
    /// `GRND_NONBLOCK`, or `/dev/random` opened with `O_NONBLOCK`.
    DoesNotWait,
    /// `GRND_INSECURE`, or a `read` of `/dev/urandom`: takes what there is.
    TakesAnything,
}

/// What it gets.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gate {
    Hand,
    Wait,
    /// `EAGAIN`.
    Again,
}

#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const GRND_NONBLOCK: u32 = 0x0001;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const GRND_RANDOM: u32 = 0x0002;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const GRND_INSECURE: u32 = 0x0004;

#[cfg(verus_keep_ghost)]
verus! {

/// `bits` and `init` always say the same thing.
pub open spec fn consistent(s: Seeding) -> bool {
    &&& s.bits <= POOL_BITS
    // 256 and not the constant, so that changing the constant is refused.
    &&& (s.init == Init::Ready) == (s.bits >= 256)
    &&& (s.init == Init::Empty) == (s.bits < POOL_EARLY_BITS)
}

} // verus!

/// A machine that has just started.
#[cfg_attr(verus_keep_ghost, verus_spec(s =>
    ensures
        consistent(s),
        s.bits == 0,
        s.init == Init::Empty,
))]
pub const fn start() -> Seeding {
    Seeding { bits: 0, init: Init::Empty }
}

/// `_credit_init_bits`. No source adds more than the pool holds, the count only grows, and only a crossing of a line has
/// anything done about it, once.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        consistent(s),
    ensures
        consistent(r.seeding),
        r.seeding.bits >= s.bits,
        // Ready only by a real sum: nothing here turns less than 256 credited bits into Ready.
        r.seeding.init == Init::Ready ==> s.bits as nat + bits as nat >= 256,
        // Ready stays.
        s.init == Init::Ready ==> r.seeding == s && r.then == Then::Nothing,
        // Each line is acted on when it is crossed, and only then.
        r.then == Then::Reseed <==> (s.init != Init::Ready && r.seeding.init == Init::Ready),
        r.then == Then::FirstKey <==> (s.init == Init::Empty && r.seeding.init == Init::Early),
))]
pub fn credit(s: Seeding, bits: u32) -> Credited {
    let add = if bits > POOL_BITS { POOL_BITS } else { bits };
    let new = if s.bits + add > POOL_BITS { POOL_BITS } else { s.bits + add };
    if new >= POOL_READY_BITS {
        if s.bits >= POOL_READY_BITS {
            Credited { seeding: s, then: Then::Nothing }
        } else {
            Credited { seeding: Seeding { bits: new, init: Init::Ready }, then: Then::Reseed }
        }
    } else if new >= POOL_EARLY_BITS {
        if s.bits >= POOL_EARLY_BITS {
            Credited { seeding: Seeding { bits: new, init: Init::Early }, then: Then::Nothing }
        } else {
            Credited { seeding: Seeding { bits: new, init: Init::Early }, then: Then::FirstKey }
        }
    } else {
        Credited { seeding: Seeding { bits: new, init: Init::Empty }, then: Then::Nothing }
    }
}

/// `sys_getrandom` and `random_read_iter`: **only a machine at Ready hands output to anyone who did not say that anything
/// will do.**
#[cfg_attr(verus_keep_ghost, verus_spec(g =>
    ensures
        g == Gate::Hand <==> (init == Init::Ready || request == Request::TakesAnything),
        g == Gate::Wait <==> (init != Init::Ready && request == Request::Waits),
        g == Gate::Again <==> (init != Init::Ready && request == Request::DoesNotWait),
))]
pub fn gate(init: Init, request: Request) -> Gate {
    match (init, request) {
        (Init::Ready, _) | (_, Request::TakesAnything) => Gate::Hand,
        (_, Request::Waits) => Gate::Wait,
        (_, Request::DoesNotWait) => Gate::Again,
    }
}

/// The flags of `getrandom`, or `None` for `EINVAL`: an unknown bit, or `GRND_INSECURE` with `GRND_RANDOM`.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        flags & !(GRND_NONBLOCK | GRND_RANDOM | GRND_INSECURE) != 0 ==> r is None,
        flags & GRND_INSECURE != 0 && flags & GRND_RANDOM != 0 ==> r is None,
        r == Some(Request::TakesAnything) ==> flags & GRND_INSECURE != 0,
        r == Some(Request::DoesNotWait) ==> flags & GRND_NONBLOCK != 0,
))]
pub fn request_from_flags(flags: u32) -> Option<Request> {
    if flags & !(GRND_NONBLOCK | GRND_RANDOM | GRND_INSECURE) != 0 {
        return None;
    }
    if flags & GRND_INSECURE != 0 && flags & GRND_RANDOM != 0 {
        return None;
    }
    if flags & GRND_INSECURE != 0 {
        Some(Request::TakesAnything)
    } else if flags & GRND_NONBLOCK != 0 {
        Some(Request::DoesNotWait)
    } else {
        Some(Request::Waits)
    }
}

/// `add_bootloader_randomness`: every byte of a seed counts as 8 bits, if the boot loader is trusted
/// (`random.trust_bootloader`, on by default as in Linux's distributions), and an all-zero seed is no seed, whoever sent it:
/// Linux has no such check, and a tree whose seed was wiped would otherwise count as seeded.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        !trusted ==> r == 0,
        all_zero ==> r == 0,
        r <= POOL_BITS,
        trusted && !all_zero && len >= 32 ==> r == POOL_BITS,
))]
pub fn bootloader_credit(len: usize, all_zero: bool, trusted: bool) -> u32 {
    if !trusted || all_zero {
        0
    } else if len >= 32 {
        POOL_BITS
    } else {
        len as u32 * 8
    }
}

/// A hardware source's rating that counts each bit it gives as a whole bit: Linux's `hwrng.quality` at its most.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const QUALITY_FULL: u32 = 1024;
/// What a read of a source sends at a time: 32 bytes.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const HARDWARE_READ_BITS: u32 = 256;

/// `hwrng_fillfn`: what a read of [`HARDWARE_READ_BITS`] counts for from a source rated `quality`, Linux's `hwrng.quality`
/// in 1024ths of a bit per bit. Linux's `rc * quality * 8 >> 10`, with the rating capped at [`QUALITY_FULL`] as Linux does.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r <= POOL_BITS,
        quality == 0 ==> r == 0,
        quality >= QUALITY_FULL ==> r == POOL_BITS,
        quality < QUALITY_FULL ==> r < POOL_BITS,
))]
pub fn hardware_credit(quality: u32) -> u32 {
    if quality >= QUALITY_FULL { POOL_BITS } else { quality / (QUALITY_FULL / HARDWARE_READ_BITS) }
}

/// How many reads at `per_read` bits each seed the generator: one, if they count for nothing, as a source that is mixed in
/// and not counted is still read once.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        1 <= r,
        per_read == 0 || per_read >= POOL_READY_BITS ==> r == 1,
))]
pub fn hardware_reads(per_read: u32) -> u32 {
    if per_read == 0 || per_read >= POOL_READY_BITS {
        return 1;
    }
    let n = (POOL_READY_BITS + per_read - 1) / per_read;
    if n == 0 { 1 } else { n }
}

/// `crng_reseed_interval`, in seconds: half the uptime early on, at least 1 s, then a minute.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        1 <= r <= 60,
        uptime_s >= 120 ==> r == 60,
))]
pub fn reseed_interval_s(uptime_s: u64) -> u64 {
    if uptime_s >= 120 {
        60
    } else if uptime_s < 2 {
        1
    } else {
        uptime_s / 2
    }
}

/// How often `try_to_generate_entropy` reads the counter to see how fast it moves.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const JITTER_TRIALS: u32 = 8192;
/// Above this many timer firings per bit, Linux gives up on a counter: `HZ / 15` at `HZ` 250 is 16.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const JITTER_MAX_SAMPLES_PER_BIT: u32 = 16;

/// `try_to_generate_entropy`: one bit per this many timer firings. A counter that did not move is `None`: a machine that
/// must not count anything from it.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        different < JITTER_TRIALS,
    ensures
        r matches Some(n) ==> 1 <= n <= JITTER_MAX_SAMPLES_PER_BIT,
        different == 0 ==> r is None,
))]
pub fn samples_per_bit(different: u32) -> Option<u32> {
    if different == 0 {
        return None;
    }
    let divisor = different + 1;
    let n = (JITTER_TRIALS + divisor - 1) / divisor;
    if n == 0 || n > JITTER_MAX_SAMPLES_PER_BIT { None } else { Some(n) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(steps: &[u32]) -> (Seeding, Vec<Then>) {
        let mut s = start();
        let mut then = Vec::new();
        for &bits in steps {
            let c = credit(s, bits);
            s = c.seeding;
            then.push(c.then);
        }
        (s, then)
    }

    #[test]
    fn a_whole_seed_makes_it_ready_at_once() {
        let (s, then) = run(&[256]);
        assert_eq!(s, Seeding { bits: 256, init: Init::Ready });
        assert_eq!(then, [Then::Reseed]);
    }

    #[test]
    fn bits_add_up_through_early_as_in_linux() {
        let (s, then) = run(&[100, 27, 1, 127, 1, 1]);
        assert_eq!(s.init, Init::Ready);
        assert_eq!(then, [Then::Nothing, Then::Nothing, Then::FirstKey, Then::Nothing, Then::Reseed, Then::Nothing]);
    }

    #[test]
    fn nothing_short_of_the_line_is_ready() {
        let (s, _) = run(&[255]);
        assert_eq!(s.init, Init::Early);
        for request in [Request::Waits, Request::DoesNotWait] {
            assert_ne!(gate(s.init, request), Gate::Hand);
        }
    }

    #[test]
    fn a_huge_credit_is_capped_and_does_not_wrap() {
        let c = credit(Seeding { bits: 255, init: Init::Early }, u32::MAX);
        assert_eq!(c.seeding.bits, 256);
    }

    #[test]
    fn getrandom_flags_as_in_linux() {
        assert_eq!(request_from_flags(0), Some(Request::Waits));
        assert_eq!(request_from_flags(GRND_RANDOM), Some(Request::Waits));
        assert_eq!(request_from_flags(GRND_NONBLOCK), Some(Request::DoesNotWait));
        assert_eq!(request_from_flags(GRND_NONBLOCK | GRND_RANDOM), Some(Request::DoesNotWait));
        assert_eq!(request_from_flags(GRND_INSECURE), Some(Request::TakesAnything));
        assert_eq!(request_from_flags(GRND_INSECURE | GRND_NONBLOCK), Some(Request::TakesAnything));
        assert_eq!(request_from_flags(GRND_INSECURE | GRND_RANDOM), None);
        assert_eq!(request_from_flags(0x8), None);
    }

    #[test]
    fn a_seed_counts_only_if_trusted_and_not_wiped() {
        assert_eq!(bootloader_credit(32, false, true), 256);
        assert_eq!(bootloader_credit(64, false, true), 256);
        assert_eq!(bootloader_credit(16, false, true), 128);
        assert_eq!(bootloader_credit(32, true, true), 0);
        assert_eq!(bootloader_credit(32, false, false), 0);
    }

    #[test]
    fn a_hardware_read_counts_as_linux_counts_it() {
        assert_eq!(hardware_credit(QUALITY_FULL), 256);
        assert_eq!(hardware_credit(u32::MAX), 256);
        assert_eq!(hardware_credit(0), 0);
        // imx-rngc's 19: 32 * 19 * 8 >> 10 in Linux, so 64 reads, 2 KiB, before the generator is seeded.
        assert_eq!(hardware_credit(19), 4);
        assert_eq!(hardware_reads(4), 64);
        assert_eq!(hardware_credit(3), 0);
        assert_eq!(hardware_reads(0), 1);
        assert_eq!(hardware_reads(256), 1);
        assert_eq!(hardware_reads(255), 2);
        assert_eq!(hardware_reads(1), 256);
    }

    #[test]
    fn the_reseed_interval_grows_with_uptime() {
        assert_eq!(reseed_interval_s(0), 1);
        assert_eq!(reseed_interval_s(10), 5);
        assert_eq!(reseed_interval_s(119), 59);
        assert_eq!(reseed_interval_s(120), 60);
    }

    #[test]
    fn a_counter_that_moves_is_good_and_a_stuck_one_is_not() {
        assert_eq!(samples_per_bit(8191), Some(1));
        assert_eq!(samples_per_bit(4095), Some(2));
        assert_eq!(samples_per_bit(100), None);
        assert_eq!(samples_per_bit(0), None);
    }
}
