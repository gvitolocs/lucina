//! Where the bark goes, by `lucina.wdt.bark=route|fixed:<mpidr>|boot` (patch 1312). Plain code with host tests.
//!
//! Linux's `qcom-wdt.c` routes nothing: it requests the bark with no affinity, and GICv3 sends it to the first online
//! CPU. Ours must not go to a CPU that cannot take it: the boot CPU pets, so a boot CPU that stops taking interrupts stops
//! the pets and cannot take the bark either (patch 1185). Patch 1312 adds that the target must have taken an interrupt
//! within the pet period: a CPU that sits with interrupts off would leave the bite with no record just the same.

/// What `lucina.wdt.bark=` asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The default: off the boot CPU, to a CPU that took an interrupt within the pet period ([`next`]).
    Route,
    /// Pinned to the CPU whose GIC affinity (Aff3.Aff2.Aff1.Aff0, as MPIDR 0x600 reads 0x600) is this.
    Fixed(u32),
    /// On the boot CPU, which maps it, as before patch 1185.
    Boot,
}

/// The value of `lucina.wdt.bark=` as a mode, with no argument as [`Mode::Route`]; `None` for a value it does not take.
/// `fixed:` takes hexadecimal with `0x` or decimal.
pub fn mode_of(value: Option<&str>) -> Option<Mode> {
    let Some(value) = value else {
        return Some(Mode::Route);
    };
    match value {
        "route" => Some(Mode::Route),
        "boot" => Some(Mode::Boot),
        other => {
            let number = other.strip_prefix("fixed:")?;
            let affinity = match number
                .strip_prefix("0x")
                .or_else(|| number.strip_prefix("0X"))
            {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => number.parse().ok()?,
            };
            Some(Mode::Fixed(affinity))
        }
    }
}

/// Why the bark moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    /// It was on the boot CPU, which pets.
    OffThePetter,
    /// Its CPU took no interrupt within the pet period; the counter ticks since its last one.
    Stale(u64),
    /// No CPU but the boot CPU took an interrupt within the pet period: the boot CPU, which runs this, is the one left.
    NoOtherFresh,
}

/// A move of the bark to CPU `to` (an index).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Move {
    pub to: usize,
    pub why: Why,
}

/// CPU `cpu`, whose last interrupt came at counter value `last_irq[cpu]` (0 for never), took one within `period` ticks of
/// `now`. A time after `now` (another CPU's counter read a little later) counts as within.
pub fn took_one(last_irq: &[u64], cpu: usize, now: u64, period: u64) -> bool {
    match last_irq.get(cpu) {
        Some(&at) => at != 0 && now.saturating_sub(at) <= period,
        None => false,
    }
}

/// Where the bark goes next in [`Mode::Route`], called by the boot CPU `bsp` after each pet: `None` to leave it on
/// `current`, otherwise the move.
///
/// - It stays on `current` while that is not `bsp` and took an interrupt within `period`, so the GIC is not written each time.
/// - Otherwise it goes to the CPU other than `bsp` whose last interrupt is the newest, among those within `period` (the
///   lowest index of equals).
/// - With none, it goes to `bsp`, which is running this: never left on a CPU that took no interrupt within `period`. It
///   stays on `bsp` until another CPU qualifies.
pub fn next(bsp: usize, current: usize, last_irq: &[u64], now: u64, period: u64) -> Option<Move> {
    if current != bsp && took_one(last_irq, current, now, period) {
        return None;
    }
    let mut best: Option<usize> = None;
    for cpu in 0..last_irq.len() {
        if cpu == bsp || !took_one(last_irq, cpu, now, period) {
            continue;
        }
        if best.is_none_or(|b| last_irq[cpu] > last_irq[b]) {
            best = Some(cpu);
        }
    }
    let why_from = || {
        if current == bsp {
            Why::OffThePetter
        } else {
            let at = last_irq.get(current).copied().unwrap_or(0);
            Why::Stale(if at == 0 {
                u64::MAX
            } else {
                now.saturating_sub(at)
            })
        }
    };
    match best {
        Some(to) => Some(Move {
            to,
            why: why_from(),
        }),
        None if current == bsp => None,
        None => Some(Move {
            to: bsp,
            why: Why::NoOtherFresh,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: u64 = 4_000;

    #[test]
    fn the_argument() {
        assert_eq!(mode_of(None), Some(Mode::Route));
        assert_eq!(mode_of(Some("route")), Some(Mode::Route));
        assert_eq!(mode_of(Some("boot")), Some(Mode::Boot));
        assert_eq!(mode_of(Some("fixed:0x600")), Some(Mode::Fixed(0x600)));
        assert_eq!(mode_of(Some("fixed:0X700")), Some(Mode::Fixed(0x700)));
        assert_eq!(mode_of(Some("fixed:1")), Some(Mode::Fixed(1)));
        assert_eq!(mode_of(Some("fixed:")), None);
        assert_eq!(mode_of(Some("fixed:0xzz")), None);
        assert_eq!(mode_of(Some("fixed")), None);
        assert_eq!(mode_of(Some("")), None);
        assert_eq!(mode_of(Some("Route")), None);
    }

    #[test]
    fn it_leaves_the_boot_cpu_for_the_freshest_other() {
        // Mapped on CPU 0, which pets: CPU 2's interrupt is the newest.
        let last = [10_000, 9_000, 9_500, 0];
        assert_eq!(
            next(0, 0, &last, 10_000, P),
            Some(Move {
                to: 2,
                why: Why::OffThePetter
            })
        );
    }

    #[test]
    fn it_stays_while_its_cpu_takes_interrupts() {
        let last = [10_000, 9_000, 9_900];
        assert_eq!(next(0, 1, &last, 10_000, P), None);
        // At the period's edge it still stays.
        assert_eq!(next(0, 1, &[10_000, 6_000, 9_900], 10_000, P), None);
    }

    #[test]
    fn it_moves_off_a_cpu_whose_last_interrupt_is_older_than_the_period() {
        let last = [10_000, 5_999, 9_900, 9_950];
        assert_eq!(
            next(0, 1, &last, 10_000, P),
            Some(Move {
                to: 3,
                why: Why::Stale(4_001)
            })
        );
        // Never one: the age is unknown.
        assert_eq!(
            next(0, 1, &[10_000, 0, 9_900], 10_000, P),
            Some(Move {
                to: 2,
                why: Why::Stale(u64::MAX)
            })
        );
    }

    #[test]
    fn with_no_other_cpu_fresh_it_goes_to_the_boot_cpu_and_stays_there() {
        let last = [10_000, 1_000, 2_000];
        assert_eq!(
            next(0, 1, &last, 10_000, P),
            Some(Move {
                to: 0,
                why: Why::NoOtherFresh
            })
        );
        assert_eq!(next(0, 0, &last, 10_000, P), None);
        // One CPU only.
        assert_eq!(next(0, 0, &[10_000], 10_000, P), None);
        // Back off the boot CPU once another CPU takes interrupts again.
        assert_eq!(
            next(0, 0, &[10_000, 9_999, 2_000], 10_000, P),
            Some(Move {
                to: 1,
                why: Why::OffThePetter
            })
        );
    }

    #[test]
    fn a_time_after_now_counts_and_equals_go_to_the_lowest_index() {
        assert_eq!(
            next(0, 0, &[10_000, 10_050, 10_050], 10_000, P),
            Some(Move {
                to: 1,
                why: Why::OffThePetter
            })
        );
    }

    #[test]
    fn a_current_cpu_beyond_the_list_or_a_boot_cpu_not_first() {
        // An index past the end (the bark where it was mapped is passed as the boot CPU by the kernel; this is the rule's
        // own guard).
        assert_eq!(
            next(0, 9, &[10_000, 9_000], 10_000, P),
            Some(Move {
                to: 1,
                why: Why::Stale(u64::MAX)
            })
        );
        // The boot CPU is index 1: index 0 may take the bark.
        assert_eq!(
            next(1, 1, &[9_000, 10_000], 10_000, P),
            Some(Move {
                to: 0,
                why: Why::OffThePetter
            })
        );
    }
}
