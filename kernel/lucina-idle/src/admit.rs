//! What every governed choice passes before the CPU enters it (verified with Verus).
//!
//! Linux's teo keeps a state's exit latency within the PM QoS limit and its target residency within the sleep length in
//! the common case, but not in all of them: with state 0 disabled, or one state left enabled, it returns that state
//! whatever its latency or the next timer (`teo_select`'s `idx == idx0` and `teo_find_shallower_state` fall-backs). Here
//! the kernel passes teo's answer through [`admit`], which makes both bounds hold for every state but 0 (WFI: one
//! microsecond each way, the state an idle CPU cannot go below), and then through [`gate`], which lets a state that stops
//! the CPU's timer through only after the CPU's wake-up is on the broadcast timer. Where teo already keeps the bounds,
//! both return its answer unchanged, so the teo replay against Linux is unaffected; elsewhere this is stricter than Linux.

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// What the governor and these checks need to know about one idle state.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct State {
    /// The shortest stay for which entering pays off.
    pub target_residency_ns: i64,
    /// The worst-case time from the wake-up event to running again.
    pub exit_latency_ns: i64,
}

#[cfg(verus_keep_ghost)]
verus! {
/// `state` wakes within the latency budget and pays off before the next timer.
pub open spec fn fits_spec(state: State, latency_ns: i64, length_ns: i64) -> bool {
    state.exit_latency_ns <= latency_ns && state.target_residency_ns <= length_ns
}
}

/// Whether `state` wakes within `latency_ns` and pays off within `length_ns` (the time to the next timer).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures r == fits_spec(state, latency_ns, length_ns),
))]
pub fn fits(state: State, latency_ns: i64, length_ns: i64) -> bool {
    let wakes_in_time = state.exit_latency_ns <= latency_ns;
    let pays_off = state.target_residency_ns <= length_ns;
    wakes_in_time && pays_off
}

/// The state to enter for the governor's `choice`: `choice` itself if it is enabled and [`fits`], else the deepest
/// shallower one that is, else 0. Never deeper than `choice`.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        choice < states.len(),
        states.len() == disabled.len(),
    ensures
        r <= choice,
        r == 0 || (!disabled@[r as int] && fits_spec(states@[r as int], latency_ns, length_ns)),
        forall|j: int| r < j <= choice ==> disabled@[j] || !fits_spec(states@[j], latency_ns, length_ns),
))]
pub fn admit(states: &[State], disabled: &[bool], choice: usize, latency_ns: i64, length_ns: i64) -> usize {
    let mut i = choice;
    #[cfg_attr(verus_keep_ghost, verus_spec(
        invariant
            i <= choice,
            choice < states.len(),
            states.len() == disabled.len(),
            forall|j: int| i < j <= choice ==> disabled@[j] || !fits_spec(states@[j], latency_ns, length_ns),
        decreases i
    ))]
    while i > 0 {
        if !disabled[i] && fits(states[i], latency_ns, length_ns) {
            return i;
        }
        i -= 1;
    }
    0
}

/// After the hand-over: `choice` (an [`admit`]ted state) if it keeps the CPU's timer or `on_broadcast` says the CPU's
/// wake-up is now on the broadcast timer; else the deepest shallower state that is enabled, [`fits`] and keeps the
/// timer, else 0. State 0 (WFI) keeps the timer. A state that stops the timer is never returned without the hand-over.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        choice < states.len(),
        states.len() == disabled.len(),
        states.len() == timer_stop.len(),
        !timer_stop@[0],
        choice == 0 || (!disabled@[choice as int] && fits_spec(states@[choice as int], latency_ns, length_ns)),
    ensures
        r <= choice,
        timer_stop@[r as int] ==> on_broadcast,
        r == 0 || (!disabled@[r as int] && fits_spec(states@[r as int], latency_ns, length_ns)),
        (!timer_stop@[choice as int] || on_broadcast) ==> r == choice,
))]
pub fn gate(
    states: &[State],
    timer_stop: &[bool],
    disabled: &[bool],
    choice: usize,
    on_broadcast: bool,
    latency_ns: i64,
    length_ns: i64,
) -> usize {
    if !timer_stop[choice] || on_broadcast {
        return choice;
    }
    let mut i = choice;
    #[cfg_attr(verus_keep_ghost, verus_spec(
        invariant
            i <= choice,
            choice < states.len(),
            states.len() == disabled.len(),
            states.len() == timer_stop.len(),
            !timer_stop@[0],
            timer_stop@[choice as int] && !on_broadcast,
        decreases i
    ))]
    while i > 0 {
        i -= 1;
        if !disabled[i] && !timer_stop[i] && fits(states[i], latency_ns, length_ns) {
            return i;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: [State; 3] = [
        State { target_residency_ns: 1_000, exit_latency_ns: 1_000 },
        State { target_residency_ns: 1_774_000, exit_latency_ns: 1_450_000 },
        State { target_residency_ns: 4_001_000, exit_latency_ns: 1_617_000 },
    ];
    const ON: [bool; 3] = [false; 3];
    const STOP: [bool; 3] = [false, true, true];

    #[test]
    fn admit_keeps_a_choice_that_fits() {
        assert_eq!(admit(&S, &ON, 2, i64::MAX, 5_000_000), 2);
        assert_eq!(admit(&S, &ON, 1, 1_450_000, 1_774_000), 1);
    }

    #[test]
    fn admit_goes_shallower_on_latency_or_timer() {
        assert_eq!(admit(&S, &ON, 2, 1_500_000, 5_000_000), 1);
        assert_eq!(admit(&S, &ON, 2, i64::MAX, 2_000_000), 1);
        assert_eq!(admit(&S, &ON, 2, i64::MAX, 1_000_000), 0);
        // teo's one-enabled-state case: WFI disabled, a timer in 1 ms; the deep state does not pay off.
        assert_eq!(admit(&S, &[true, true, false], 2, i64::MAX, 1_000_000), 0);
    }

    #[test]
    fn gate_needs_the_hand_over() {
        assert_eq!(gate(&S, &STOP, &ON, 2, true, i64::MAX, i64::MAX), 2);
        assert_eq!(gate(&S, &STOP, &ON, 2, false, i64::MAX, i64::MAX), 0);
        assert_eq!(gate(&S, &[false, false, true], &ON, 2, false, i64::MAX, i64::MAX), 1);
        assert_eq!(gate(&S, &[false, false, true], &[false, true, false], 2, false, i64::MAX, i64::MAX), 0);
        assert_eq!(gate(&S, &STOP, &ON, 0, false, i64::MAX, i64::MAX), 0);
    }
}
