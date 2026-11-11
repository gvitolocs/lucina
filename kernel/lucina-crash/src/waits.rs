// The wait-chain reader (patch 1213): from what a vanished boot's register trace says each CPU was doing, which CPUs could
// hold the spin lock the others waited on. Verus checks this very file: `//@` lines hold the proof, see
// scripts/verify-shipped.sh (D-011); verification/juno-crash-layout/mutants.sh plants bugs that Verus must refuse.
// mmio.rs calls these functions and holds no copy of the rules.
//
// Spinwatch (patch 1214) notes a CPU's wait once it has lasted about a second; nothing is noted when a lock is taken, so the
// holder is never recorded and is found afterwards, as Linux's lockup reports leave it to the reader: a CPU that waits on
// nothing and does not sit in WFI (the idle loop holds no spin lock) is a candidate, among the CPUs that left any trace.
// For each CPU, `waiting` holds the address of the lock it waited on, or 0.

// CPU `cpu` may hold the lock: it left a trace, waits on nothing, and is not in WFI.
//@ pub open spec fn may_hold_spec(cpu: int, waiting: Seq<u64>, in_wfi: Seq<bool>, seen: Seq<bool>) -> bool {
//@     0 <= cpu < waiting.len() && cpu < in_wfi.len() && cpu < seen.len()
//@         && seen[cpu] && waiting[cpu] == 0 && !in_wfi[cpu]
//@ }

/// Whether CPU `cpu` may hold the lock the others wait on.
pub fn may_hold(cpu: usize, waiting: &[u64], in_wfi: &[bool], seen: &[bool]) -> bool //@ret
//@ ensures ret == may_hold_spec(cpu as int, waiting@, in_wfi@, seen@),
{
    cpu < waiting.len()
        && cpu < in_wfi.len()
        && cpu < seen.len()
        && seen[cpu]
        && waiting[cpu] == 0
        && !in_wfi[cpu]
}

/// The first CPU from `from` on that may hold the lock, or `None` if none does: called again from the one after it, it
/// lists them all.
pub fn next_holder(from: usize, waiting: &[u64], in_wfi: &[bool], seen: &[bool]) -> Option<usize> //@ret
//@ ensures
//@     match ret {
//@         None => forall|j: int| from <= j < waiting@.len() ==> !may_hold_spec(j, waiting@, in_wfi@, seen@),
//@         Some(i) => from <= i && may_hold_spec(i as int, waiting@, in_wfi@, seen@)
//@             && forall|j: int| from <= j < i ==> !may_hold_spec(j, waiting@, in_wfi@, seen@),
//@     },
{
    if from >= waiting.len() {
        return None;
    }
    let mut k: usize = from;
    while k < waiting.len()
    //@ invariant
    //@     from <= k <= waiting@.len(),
    //@     forall|j: int| from <= j < k ==> !may_hold_spec(j, waiting@, in_wfi@, seen@),
    //@ decreases waiting@.len() - k,
    {
        if may_hold(k, waiting, in_wfi, seen) {
            return Some(k);
        }
        k += 1;
    }
    None
}

// Every CPU that waits waits on `lock`.
//@ pub open spec fn all_on(waiting: Seq<u64>, lock: u64) -> bool {
//@     forall|j: int| 0 <= j < waiting.len() ==> waiting[j] == 0 || waiting[j] == lock
//@ }

/// The lock every waiting CPU waits on, if at least one waits and they all wait on the same one; `None` when none waits
/// or two wait on different locks (then the waits are not one pile-up, or the holder of one waits on the other).
pub fn same_lock(waiting: &[u64]) -> Option<u64> //@ret
//@ ensures
//@     match ret {
//@         None => (forall|j: int| 0 <= j < waiting@.len() ==> waiting@[j] == 0)
//@             || exists|p: int, q: int| 0 <= p < waiting@.len() && 0 <= q < waiting@.len()
//@                 && waiting@[p] != 0 && waiting@[q] != 0 && waiting@[p] != waiting@[q],
//@         Some(l) => l != 0 && all_on(waiting@, l) && exists|p: int| 0 <= p < waiting@.len() && waiting@[p] == l,
//@     },
{
    let mut found: u64 = 0;
    let mut first: usize = 0;
    let mut k: usize = 0;
    while k < waiting.len()
    //@ invariant
    //@     k <= waiting@.len(),
    //@     found == 0 ==> forall|j: int| 0 <= j < k ==> waiting@[j] == 0,
    //@     found != 0 ==> first < k && waiting@[first as int] == found
    //@         && forall|j: int| 0 <= j < k ==> waiting@[j] == 0 || waiting@[j] == found,
    //@ decreases waiting@.len() - k,
    {
        let lock = waiting[k];
        if lock != 0 {
            if found == 0 {
                found = lock;
                first = k;
            } else if lock != found {
                //@ assert(waiting@[first as int] != 0 && waiting@[k as int] != 0 && waiting@[first as int] != waiting@[k as int]);
                return None;
            }
        }
        k += 1;
    }
    if found == 0 {
        None
    } else {
        Some(waiting[first])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn holders_are_the_traced_cpus_that_neither_wait_nor_idle() {
        let waiting = [0, 0x40, 0x40, 0, 0, 0];
        let in_wfi = [true, false, false, false, false, false];
        let seen = [true, true, true, true, false, true];
        let mut out = [usize::MAX; 6];
        let (mut from, mut n) = (0, 0);
        while let Some(cpu) = next_holder(from, &waiting, &in_wfi, &seen) {
            out[n] = cpu;
            n += 1;
            from = cpu + 1;
        }
        assert_eq!(&out[..n], &[3, 5]);
        assert_eq!(next_holder(6, &waiting, &in_wfi, &seen), None);
        assert_eq!(next_holder(usize::MAX, &waiting, &in_wfi, &seen), None);
        // Shorter flag arrays: CPUs past them are not candidates.
        assert!(!may_hold(5, &waiting, &in_wfi[..5], &seen));
    }

    #[test]
    fn one_lock_or_none() {
        assert_eq!(same_lock(&[0, 0x40, 0, 0x40]), Some(0x40));
        assert_eq!(same_lock(&[0, 0, 0]), None);
        assert_eq!(same_lock(&[]), None);
        assert_eq!(same_lock(&[0x40, 0, 0x80]), None);
    }
}
