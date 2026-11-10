// The out-of-memory killer's rules that a test could miss: whose score is what, who is chosen, when a user page fault may take
// frames, and what oom_score_adj a program gets at exec. Verus checks this very file: `//@` lines hold the proof, see
// scripts/verify-shipped.sh (D-011); verification/juno-oom/mutants.sh plants bugs that Verus must refuse. lib.rs calls these
// functions and holds no copy of the rules.
//
// After Linux mm/oom_kill.c: oom_badness (rss, adjusted by oom_score_adj scaled to the machine, never for -1000) and
// oom_evaluate_task (the highest score wins; an equal score later in the walk replaces the one before).

/// `OOM_SCORE_ADJ_MIN`: never chosen.
pub const ADJ_MIN: i16 = -1000;
/// `OOM_SCORE_ADJ_MAX`.
pub const ADJ_MAX: i16 = 1000;
/// Pages a score works with at most (4 PiB of 4 KiB pages): far beyond any machine, small enough that no sum overflows.
pub const LIMIT: u64 = 1_099_511_627_776; // 2^40

/// A process's score: its resident pages plus `adj` thousandths of the machine's pages, or `None` for `ADJ_MIN`.
pub fn points(rss: u64, adj: i16, total: u64) -> Option<i64> //@ret
//@ requires rss <= LIMIT, total <= LIMIT, ADJ_MIN <= adj <= ADJ_MAX,
//@ ensures
//@     adj == ADJ_MIN ==> ret.is_none(),
//@     adj != ADJ_MIN ==> ret == Some((rss as int + adj as int * (total as int / 1000)) as i64),
{
    if adj == ADJ_MIN {
        return None;
    }
    let scale = (total / 1000) as i64;
    //@ assert(total / 1000 <= 1_099_511_627) by(nonlinear_arith) requires total <= 1_099_511_627_776;
    //@ assert(0 <= scale as int <= 1_099_511_627);
    //@ assert(-1000 * (scale as int) <= (adj as int) * (scale as int) <= 1000 * (scale as int)) by(nonlinear_arith)
    //@     requires -1000 <= adj as int <= 1000, 0 <= scale as int;
    Some(rss as i64 + adj as i64 * scale)
}

/// The victim: the index of the highest score, the last of equal ones; `None` when nobody may be chosen.
pub fn choose(scores: &[Option<i64>]) -> Option<usize> //@ret
//@ ensures
//@     match ret {
//@         None => forall|j: int| 0 <= j < scores@.len() ==> scores@[j].is_none(),
//@         Some(i) => i < scores@.len() && scores@[i as int].is_some()
//@             && (forall|j: int| 0 <= j < scores@.len() && scores@[j].is_some() ==> scores@[j].unwrap() <= scores@[i as int].unwrap())
//@             && (forall|j: int| i < j < scores@.len() && scores@[j].is_some() ==> scores@[j].unwrap() < scores@[i as int].unwrap()),
//@     },
{
    let mut best: Option<usize> = None;
    let mut best_score: i64 = 0;
    let mut k: usize = 0;
    while k < scores.len()
    //@ invariant
    //@     k <= scores@.len(),
    //@     match best {
    //@         None => forall|j: int| 0 <= j < k ==> scores@[j].is_none(),
    //@         Some(i) => i < k && scores@[i as int] == Some(best_score)
    //@             && (forall|j: int| 0 <= j < k && scores@[j].is_some() ==> scores@[j].unwrap() <= best_score)
    //@             && (forall|j: int| i < j < k && scores@[j].is_some() ==> scores@[j].unwrap() < best_score),
    //@     },
    //@ decreases scores@.len() - k,
    {
        if let Some(s) = scores[k] {
            if best.is_none() || s >= best_score {
                best = Some(k);
                best_score = s;
            }
        }
        k += 1;
    }
    best
}

/// Whether a user page fault may take frames: never the last `reserve`, which page-table nodes and the kernel's own
/// allocations keep for themselves (a page-table node that finds no frame panics the kernel).
pub fn user_may_take(free: u64, reserve: u64) -> bool //@ret
//@ ensures ret == (free > reserve),
{
    free > reserve
}

/// The `oom_score_adj` a process has after exec: `ADJ_MIN` for a protected program; 0 for any other program that inherited
/// `ADJ_MIN` (from a protected parent: an SSH session, an app the shell opened); what it had otherwise.
pub fn adj_at_exec(inherited: i16, protected: bool) -> i16 //@ret
//@ ensures
//@     protected ==> ret == ADJ_MIN,
//@     !protected && inherited == ADJ_MIN ==> ret == 0,
//@     !protected && inherited != ADJ_MIN ==> ret == inherited,
{
    if protected {
        ADJ_MIN
    } else if inherited == ADJ_MIN {
        0
    } else {
        inherited
    }
}
