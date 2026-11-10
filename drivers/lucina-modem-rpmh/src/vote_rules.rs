// The rules of the shared RPMh vote table (`crate::share`): what one ARC resource is sent when several drivers and the device
// tree's consumers hold votes on it, as Linux's genpd and `rpmhpd.c` aggregate them, and the XO clock's reference count. Verus
// checks this very file: `//@` lines hold the proof, see scripts/verify-shipped.sh (D-011). `crate::share` and the kernel call
// these functions and hold no copy of the rules.
//
// Derived from Linux 7.1 `drivers/pmdomain/qcom/rpmhpd.c` (`rpmhpd_aggregate_corner`, `rpmhpd_set_performance_state`,
// `rpmhpd_sync_state`, `rpmhpd_power_on`), `drivers/pmdomain/core.c` (`_genpd_reeval_performance_state`: a domain's state is
// the highest of its devices' and its subdomains' states) and `drivers/clk/clk.c` (`clk_core_prepare`: the provider's
// `prepare` runs on the first user, `unprepare` after the last). GPL-2.0.
//
// What is proved, for every client that holds a vote on a domain: the corner sent is at least as high as the one the client asked
// for (or the domain's top, when it asked for more than the top), and never above the top; before `sync_state` it is the top;
// after it, a domain nobody holds is sent 0 and one somebody holds is never sent below its first non-zero corner. A parent's
// state includes its child's, so `mx` is never below what `cx` asks of it. XO is on while any client holds it.
//
// Not proved: that every driver of the kernel votes through `crate::share` (the kernel's code does that), that the level table
// read from the command DB is the hardware's (it is the firmware's own), and what RPMh does with a vote.

/// How many clients a domain can have: the drivers that vote and one for the device tree's other consumers.
pub const CLIENTS: usize = 8;
/// `RPMH_ARC_MAX_LEVELS`.
pub const LEVELS: usize = 32;

/// One ARC resource's level table as `rpmhpd_update_level_mapping` leaves it: `level[i]` is the voltage level (`vlvl`) of
/// corner `i`, for `i < count`; `enable_corner` is the first corner whose level is not 0.
#[derive(Copy, Clone, Debug, PartialEq, Eq)] //@omit
pub struct Table {
    pub level: [u32; 32],
    pub count: usize,
    pub enable_corner: usize,
}

//@ pub open spec fn good(t: Table) -> bool {
//@     1 <= t.count <= 32 && t.enable_corner < t.count
//@     && forall|i: int, j: int| 0 <= i <= j < t.count ==> t.level@[i] <= t.level@[j]
//@ }

/// Whether a table can be used: 1 to 32 corners, `enable_corner` among them, levels never falling. The command DB's tables
/// rise (Linux maps a level to the first corner at least as high, which only means something on a rising table); a table that
/// does not is refused rather than voted on.
pub fn table_good(t: &Table) -> bool //@ret
//@ ensures ret ==> good(*t),
{
    if t.count == 0 || t.count > LEVELS || t.enable_corner >= t.count {
        return false;
    }
    let mut i: usize = 1;
    while i < t.count
    //@ invariant 1 <= i <= t.count, t.count <= 32, t.enable_corner < t.count,
    //@     forall|a: int, b: int| 0 <= a <= b < i ==> t.level@[a] <= t.level@[b],
    //@ decreases t.count - i,
    {
        if t.level[i - 1] > t.level[i] {
            return false;
        }
        //@ assert(forall|a: int| 0 <= a < i ==> #[trigger] t.level@[a] <= t.level@[i - 1]);
        i += 1;
    }
    true
}

/// `rpmhpd_set_performance_state`'s mapping: the first corner whose level is at least `state`, the top corner when none is.
pub fn corner_of(t: &Table, state: u32) -> usize //@ret
//@ requires good(*t),
//@ ensures ret < t.count,
//@     t.level@[ret as int] >= state || ret == t.count - 1,
//@     forall|i: int| 0 <= i < ret ==> t.level@[i] < state,
{
    let mut i: usize = 0;
    while i < t.count
    //@ invariant 0 <= i <= t.count, good(*t), forall|k: int| 0 <= k < i ==> t.level@[k] < state,
    //@ decreases t.count - i,
    {
        if t.level[i] >= state {
            return i;
        }
        i += 1;
    }
    t.count - 1
}

/// genpd's view of one domain: on when a client holds it or its child domain is on; its state the highest of the states the
/// clients that hold it ask for and, when the child is on, the child's (`_genpd_reeval_performance_state`, the parent link
/// carrying the state unchanged: `rpmhpd`'s shared OPP table has no `required-opps`). Returns `(on, state)`.
pub fn domain_state(on: &[bool; 8], want: &[u32; 8], child_on: bool, child_state: u32) -> (bool, u32) //@ret
//@ ensures
//@     ret.0 == (child_on || exists|c: int| 0 <= c < 8 && on@[c]),
//@     forall|c: int| 0 <= c < 8 && on@[c] ==> ret.1 >= want@[c],
//@     child_on ==> ret.1 >= child_state,
//@     !ret.0 ==> ret.1 == 0,
{
    let mut any = child_on;
    let mut state: u32 = if child_on { child_state } else { 0 };
    let mut c: usize = 0;
    while c < CLIENTS
    //@ invariant 0 <= c <= 8,
    //@     any == (child_on || exists|k: int| 0 <= k < c && on@[k]),
    //@     forall|k: int| 0 <= k < c && on@[k] ==> state >= want@[k],
    //@     child_on ==> state >= child_state,
    //@     !any ==> state == 0,
    //@ decreases 8 - c,
    {
        if on[c] {
            any = true;
            if want[c] > state {
                state = want[c];
            }
        }
        c += 1;
    }
    (any, state)
}

/// The corner `rpmhpd_aggregate_corner` sends for a domain without an enabled peer: the top corner until `sync_state`
/// (`state_synced` false); after it, `max(corner, enable_corner)` while the domain is on (`rpmhpd_power_on`,
/// `rpmhpd_sync_state`) and 0 when it is off (`rpmhpd_power_off`).
pub fn send_corner(t: &Table, synced: bool, on: bool, state: u32) -> usize //@ret
//@ requires good(*t),
//@ ensures ret < t.count,
//@     !synced ==> ret == t.count - 1,
//@     synced && !on ==> ret == 0,
//@     synced && on ==> ret >= t.enable_corner && (t.level@[ret as int] >= state || ret == t.count - 1),
{
    if !synced {
        return t.count - 1;
    }
    if !on {
        return 0;
    }
    let c = corner_of(t, state);
    if c >= t.enable_corner {
        c
    } else {
        //@ assert(t.level@[c as int] <= t.level@[t.enable_corner as int]);
        t.enable_corner
    }
}

/// What one domain is sent, from its clients' votes and its child's state: [`domain_state`], then [`send_corner`]. The rule the
/// table exists for: no client that holds the domain gets less than it asked for (or the top), and nobody holding it means 0 once
/// synced.
pub fn domain_corner(t: &Table, synced: bool, on: &[bool; 8], want: &[u32; 8], child_on: bool, child_state: u32) -> usize //@ret
//@ requires good(*t),
//@ ensures ret < t.count,
//@     !synced ==> ret == t.count - 1,
//@     forall|c: int| 0 <= c < 8 && on@[c] ==> t.level@[ret as int] >= want@[c] || ret == t.count - 1,
//@     child_on ==> t.level@[ret as int] >= child_state || ret == t.count - 1,
//@     synced && (child_on || exists|c: int| 0 <= c < 8 && on@[c]) ==> ret >= t.enable_corner,
//@     synced && !child_on && (forall|c: int| 0 <= c < 8 ==> !on@[c]) ==> ret == 0,
{
    let (any, state) = domain_state(on, want, child_on, child_state);
    send_corner(t, synced, any, state)
}

/// The XO clock: `clk_rpmh_prepare` runs for the first client to hold it and `clk_rpmh_unprepare` after the last lets go, so the
/// resource is on exactly while some client holds it.
pub fn xo_on(holds: &[bool; 8]) -> bool //@ret
//@ ensures ret == exists|c: int| 0 <= c < 8 && holds@[c],
{
    let mut c: usize = 0;
    while c < CLIENTS
    //@ invariant 0 <= c <= 8, forall|k: int| 0 <= k < c ==> !holds@[k],
    //@ decreases 8 - c,
    {
        if holds[c] {
            return true;
        }
        c += 1;
    }
    false
}

/// `rpmhpd_aggregate_corner`'s wait: "Wait for an ack only when we are increasing the perf state of the power domain".
pub fn wait_for_answer(corner: usize, last: usize) -> bool //@ret
//@ ensures ret == (corner > last),
{
    corner > last
}
