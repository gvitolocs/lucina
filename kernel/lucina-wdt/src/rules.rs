// Where the hardware watchdog's bark goes (patch 1185). Verus checks this very file: `//@` lines hold the proof, see
// scripts/verify-shipped.sh (D-011); verification/juno-wdt/mutants.sh plants bugs that Verus must refuse. The kernel calls
// these functions and holds no copy of the rules.
//
// The bark is a shared interrupt, and the GIC sends it to one CPU. It went to the CPU that mapped it, the boot CPU, which is
// also the CPU whose thread pets the hardware: a boot CPU that stopped taking interrupts (boots 138 and 225, 2026-11-08)
// stopped the pets and then could not take the bark either, so the bite came with no record. The bark goes instead to
// another CPU whose watchdog thread ran lately, as Linux's buddy lockup detector watches a CPU from another one.

/// Whether a CPU whose watchdog thread last ran at `touched` (0 for never) ran within `fresh` counter ticks of `now`.
//@ pub open spec fn is_fresh(touched: u64, now: u64, fresh: u64) -> bool {
//@     touched != 0 && (now < touched || now - touched <= fresh)
//@ }

/// `touched` is fresh: it ran within `fresh` of `now`. A time after `now` (another CPU's counter read a little later) is fresh.
pub fn fresh_at(touched: u64, now: u64, fresh: u64) -> bool //@ret
//@ ensures ret == is_fresh(touched, now, fresh),
{
    touched != 0 && now.saturating_sub(touched) <= fresh
}

/// Whether CPU `cpu` may take the bark: it exists, it is not the boot CPU `bsp`, and its thread ran lately.
//@ pub open spec fn usable(cpu: int, bsp: usize, touched: Seq<u64>, now: u64, fresh: u64) -> bool {
//@     0 <= cpu < touched.len() && cpu != bsp as int && is_fresh(touched[cpu], now, fresh)
//@ }

/// The CPU the bark goes to, by index: `previous` while it may still take it, so that the GIC is not written each time;
/// otherwise the CPU other than `bsp` whose thread ran last. `None` when no other CPU ran within `fresh` (one CPU, or all
/// the others stuck): the bark then stays where it was.
pub fn bark_target(bsp: usize, previous: usize, touched: &[u64], now: u64, fresh: u64) -> Option<usize> //@ret
//@ ensures
//@     match ret {
//@         None => forall|j: int| 0 <= j < touched@.len() ==> !usable(j, bsp, touched@, now, fresh),
//@         Some(i) => usable(i as int, bsp, touched@, now, fresh)
//@             && (usable(previous as int, bsp, touched@, now, fresh) ==> i == previous)
//@             && (!usable(previous as int, bsp, touched@, now, fresh) ==>
//@                 forall|j: int| 0 <= j < touched@.len() && usable(j, bsp, touched@, now, fresh)
//@                     ==> touched@[j] <= touched@[i as int]),
//@     },
{
    if previous < touched.len() && previous != bsp && fresh_at(touched[previous], now, fresh) {
        return Some(previous);
    }
    let mut best: Option<usize> = None;
    let mut k: usize = 0;
    while k < touched.len()
    //@ invariant
    //@     k <= touched@.len(),
    //@     !usable(previous as int, bsp, touched@, now, fresh),
    //@     match best {
    //@         None => forall|j: int| 0 <= j < k ==> !usable(j, bsp, touched@, now, fresh),
    //@         Some(i) => i < k && usable(i as int, bsp, touched@, now, fresh)
    //@             && (forall|j: int| 0 <= j < k && usable(j, bsp, touched@, now, fresh)
    //@                 ==> touched@[j] <= touched@[i as int]),
    //@     },
    //@ decreases touched@.len() - k,
    {
        if k != bsp && fresh_at(touched[k], now, fresh) {
            let better = match best {
                None => true,
                Some(i) => touched[k] > touched[i],
            };
            if better {
                best = Some(k);
            }
        }
        k += 1;
    }
    best
}
