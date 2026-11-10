// SPDX-License-Identifier: GPL-2.0-only
//! When the GPU is powered: Linux's runtime PM for the msm GPU, as rules the kernel applies under the one lock that also
//! guards every submit. `msm_gpu_submit` takes `pm_runtime_get_sync` (resume if suspended) before it writes the ring,
//! retiring drops it with `pm_runtime_put_autosuspend`, and the device suspends once `inactive_period` (66 ms,
//! `DRM_MSM_INACTIVE_PERIOD`) has passed since it was last busy (`adreno_gpu.c`: `pm_runtime_set_autosuspend_delay`).
//!
//! Here a submit waits for its fence before it lets go of the lock, so "busy" ends when the fence arrives. The idle timer
//! and the submit path both decide with [`decide`] and record the outcome with [`apply`], under that lock: there is no
//! moment in which the timer can see an idle GPU that a submit is about to use.
//!
//! Verus checks this very file (`verification/juno-gpu-power/rules.rs`); what is proved is in the `ensures` clauses.
//! What is not covered: that the kernel holds the lock around each pair, and that the hardware steps do what Linux's do.
#![allow(unexpected_cfgs)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// Linux's `DRM_MSM_INACTIVE_PERIOD` for the A618 (`a6xx_catalog.c`: `.inactive_period`), in ms.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const INACTIVE_PERIOD_MS: u64 = 66;

/// What the GPU is, as far as power goes. A device that never came up has no `Power` at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub enum State {
    /// GX and CX on, the command processor running: a batch may be written to the ring.
    Active,
    /// The GMU slumbered, GX off, the GMU's clocks and the CX vote dropped. A batch needs a resume first.
    Suspended,
    /// A step failed: no more batches, and the memory the GPU may still touch is kept for good.
    Failed,
}

/// The power state and when the GPU was last busy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub struct Power {
    pub state: State,
    /// Milliseconds on the monotonic clock when the last batch's fence arrived, or the GPU came up.
    pub last_busy_ms: u64,
    /// How long it stays idle before it suspends; 0 never suspends (`lucina.gpu.idle=off`).
    pub idle_ms: u64,
    /// A batch is on the ring and its fence has not arrived.
    pub busy: bool,
    pub suspends: u64,
    pub resumes: u64,
}

/// What is about to happen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub enum Event {
    /// A program hands the GPU a batch.
    Submit,
    /// The idle timer looks.
    Timer,
}

/// What the kernel must do for an [`Event`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub enum Action {
    /// Write the batch to the ring.
    Run,
    /// Power the GPU up (`a6xx_gmu_pm_resume`, then `hw_init`), and only then write the batch.
    ResumeThenRun,
    /// Refuse the batch: the GPU failed.
    Refuse,
    /// Power the GPU down (`a6xx_gmu_pm_suspend`).
    Suspend,
    Nothing,
}

/// How a hardware step went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub enum Outcome {
    Ok,
    Failed,
}

/// A GPU that just came up, at `now_ms`.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r.state == State::Active,
        r.last_busy_ms == now_ms,
        r.idle_ms == idle_ms,
        !r.busy,
        r.suspends == 0 && r.resumes == 0,
))]
pub fn new(idle_ms: u64, now_ms: u64) -> Power {
    Power { state: State::Active, last_busy_ms: now_ms, idle_ms, busy: false, suspends: 0, resumes: 0 }
}

/// Whether the idle period is over: active, nothing on the ring, suspending allowed, and `idle_ms` passed since it was last
/// busy. A clock that reads earlier than `last_busy_ms` counts as not over.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures r == (p.state == State::Active && !p.busy && p.idle_ms > 0
        && now_ms >= p.last_busy_ms && now_ms - p.last_busy_ms >= p.idle_ms)
))]
pub fn idle_over(p: &Power, now_ms: u64) -> bool {
    let active = match p.state {
        State::Active => true,
        _ => false,
    };
    active && !p.busy && p.idle_ms > 0 && now_ms >= p.last_busy_ms && now_ms - p.last_busy_ms >= p.idle_ms
}

/// When the idle timer should look next, or `None` while there is nothing to wait for (suspended, failed, busy, or never).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r.is_some() == (p.state == State::Active && !p.busy && p.idle_ms > 0),
        r.is_some() ==> r.unwrap() >= p.last_busy_ms,
        r.is_some() && p.last_busy_ms + p.idle_ms <= u64::MAX ==> r.unwrap() == p.last_busy_ms + p.idle_ms,
))]
pub fn due_ms(p: &Power) -> Option<u64> {
    let active = match p.state {
        State::Active => true,
        _ => false,
    };
    if active && !p.busy && p.idle_ms > 0 {
        Some(p.last_busy_ms.saturating_add(p.idle_ms))
    } else {
        None
    }
}

/// The rule. A batch never meets a powered-down GPU: it resumes it first. The timer only ever suspends, and only an idle,
/// active GPU with nothing on the ring.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        event == Event::Submit && p.state == State::Active ==> r == Action::Run,
        event == Event::Submit && p.state == State::Suspended ==> r == Action::ResumeThenRun,
        event == Event::Submit && p.state == State::Failed ==> r == Action::Refuse,
        event == Event::Timer ==> (r == Action::Suspend || r == Action::Nothing),
        r == Action::Suspend <==> (event == Event::Timer && p.state == State::Active && !p.busy && p.idle_ms > 0
            && now_ms >= p.last_busy_ms && now_ms - p.last_busy_ms >= p.idle_ms),
        r == Action::Run ==> p.state == State::Active,
))]
pub fn decide(p: &Power, event: Event, now_ms: u64) -> Action {
    match event {
        Event::Submit => match p.state {
            State::Active => Action::Run,
            State::Suspended => Action::ResumeThenRun,
            State::Failed => Action::Refuse,
        },
        Event::Timer => {
            if idle_over(p, now_ms) {
                Action::Suspend
            } else {
                Action::Nothing
            }
        }
    }
}

/// The state after a resume: active and just busy if it worked, failed if not.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires p.state == State::Suspended,
    ensures
        outcome == Outcome::Ok ==> r.state == State::Active && r.last_busy_ms == now_ms,
        outcome == Outcome::Failed ==> r.state == State::Failed,
        r.resumes == if p.resumes < u64::MAX { p.resumes + 1 } else { p.resumes as int },
        r.suspends == p.suspends && r.idle_ms == p.idle_ms && r.busy == p.busy,
))]
pub fn resumed(p: &Power, outcome: Outcome, now_ms: u64) -> Power {
    let resumes = p.resumes.saturating_add(1);
    match outcome {
        Outcome::Ok => Power { state: State::Active, last_busy_ms: now_ms, idle_ms: p.idle_ms, busy: p.busy, suspends: p.suspends, resumes },
        Outcome::Failed => Power { state: State::Failed, last_busy_ms: p.last_busy_ms, idle_ms: p.idle_ms, busy: p.busy, suspends: p.suspends, resumes },
    }
}

/// The state after a suspend. A failed suspend leaves the GPU in an unknown power state: failed.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires p.state == State::Active && !p.busy,
    ensures
        outcome == Outcome::Ok ==> r.state == State::Suspended,
        outcome == Outcome::Failed ==> r.state == State::Failed,
        r.suspends == if p.suspends < u64::MAX { p.suspends + 1 } else { p.suspends as int },
        r.resumes == p.resumes && r.idle_ms == p.idle_ms && !r.busy && r.last_busy_ms == p.last_busy_ms,
))]
pub fn suspended(p: &Power, outcome: Outcome) -> Power {
    let suspends = p.suspends.saturating_add(1);
    match outcome {
        Outcome::Ok => Power { state: State::Suspended, last_busy_ms: p.last_busy_ms, idle_ms: p.idle_ms, busy: false, suspends, resumes: p.resumes },
        Outcome::Failed => Power { state: State::Failed, last_busy_ms: p.last_busy_ms, idle_ms: p.idle_ms, busy: false, suspends, resumes: p.resumes },
    }
}

/// A batch went onto the ring.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires p.state == State::Active,
    ensures r.state == State::Active && r.busy && r.last_busy_ms == p.last_busy_ms
        && r.suspends == p.suspends && r.resumes == p.resumes && r.idle_ms == p.idle_ms,
))]
pub fn started(p: &Power) -> Power {
    Power { state: p.state, last_busy_ms: p.last_busy_ms, idle_ms: p.idle_ms, busy: true, suspends: p.suspends, resumes: p.resumes }
}

/// Its fence arrived (`Ok`) at `now_ms`, or did not (`Failed`: the GPU is failed). A clock that went back keeps the later time.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires p.state == State::Active && p.busy,
    ensures
        !r.busy,
        outcome == Outcome::Ok ==> r.state == State::Active
            && r.last_busy_ms == if now_ms > p.last_busy_ms { now_ms } else { p.last_busy_ms },
        outcome == Outcome::Failed ==> r.state == State::Failed,
        r.suspends == p.suspends && r.resumes == p.resumes && r.idle_ms == p.idle_ms,
))]
pub fn finished(p: &Power, outcome: Outcome, now_ms: u64) -> Power {
    match outcome {
        Outcome::Ok => {
            let last_busy_ms = if now_ms > p.last_busy_ms { now_ms } else { p.last_busy_ms };
            Power { state: p.state, last_busy_ms, idle_ms: p.idle_ms, busy: false, suspends: p.suspends, resumes: p.resumes }
        }
        Outcome::Failed => Power { state: State::Failed, last_busy_ms: p.last_busy_ms, idle_ms: p.idle_ms, busy: false, suspends: p.suspends, resumes: p.resumes },
    }
}

/// `lucina.gpu.idle=`: `off` (the default until the laptop has proved it), `on` for Linux's 66 ms, or a time in ms
/// from 1 to 60000. Anything else is `None`, and the caller keeps its default.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures r.is_some() ==> r.unwrap() <= 60000,
))]
pub fn parse_idle(value: &[u8]) -> Option<u64> {
    if value.len() == 3 && value[0] == b'o' && value[1] == b'f' && value[2] == b'f' {
        return Some(0);
    }
    if value.len() == 2 && value[0] == b'o' && value[1] == b'n' {
        return Some(INACTIVE_PERIOD_MS);
    }
    if value.len() == 0 || value.len() > 5 {
        return None;
    }
    let mut ms: u64 = 0;
    let mut i = 0;
    #[cfg_attr(verus_keep_ghost, verus_spec(
        invariant 0 <= i <= value.len(), value.len() <= 5, ms < 100000,
        decreases value.len() - i
    ))]
    while i < value.len() {
        let c = value[i];
        if c < b'0' || c > b'9' {
            return None;
        }
        if ms >= 10000 {
            return None;
        }
        ms = ms * 10 + (c - b'0') as u64;
        i += 1;
    }
    if ms == 0 || ms > 60000 { None } else { Some(ms) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_submit_resumes_a_suspended_gpu_and_the_timer_waits_for_the_period() {
        let p = new(INACTIVE_PERIOD_MS, 1000);
        assert_eq!(decide(&p, Event::Timer, 1065), Action::Nothing);
        assert_eq!(decide(&p, Event::Timer, 1066), Action::Suspend);
        let p = suspended(&p, Outcome::Ok);
        assert_eq!(decide(&p, Event::Timer, 5000), Action::Nothing);
        assert_eq!(decide(&p, Event::Submit, 5000), Action::ResumeThenRun);
        let p = resumed(&p, Outcome::Ok, 5000);
        assert_eq!((p.state, p.resumes, p.suspends), (State::Active, 1, 1));
        let p = started(&p);
        // Busy: never suspended, however long the fence takes.
        assert_eq!(decide(&p, Event::Timer, 9000), Action::Nothing);
        assert_eq!(due_ms(&p), None);
        let p = finished(&p, Outcome::Ok, 5010);
        assert_eq!(due_ms(&p), Some(5076));
    }

    #[test]
    fn off_never_suspends_and_a_failure_refuses() {
        let p = new(0, 0);
        assert_eq!(decide(&p, Event::Timer, u64::MAX), Action::Nothing);
        let p = finished(&started(&p), Outcome::Failed, 3);
        assert_eq!(decide(&p, Event::Submit, 4), Action::Refuse);
        let p = suspended(&new(66, 0), Outcome::Failed);
        assert_eq!(p.state, State::Failed);
    }

    #[test]
    fn the_command_line_value() {
        assert_eq!(parse_idle(b"off"), Some(0));
        assert_eq!(parse_idle(b"on"), Some(66));
        assert_eq!(parse_idle(b"250"), Some(250));
        assert_eq!(parse_idle(b"60000"), Some(60000));
        assert_eq!(parse_idle(b"60001"), None);
        assert_eq!(parse_idle(b"0"), None);
        assert_eq!(parse_idle(b""), None);
        assert_eq!(parse_idle(b"1x"), None);
        assert_eq!(parse_idle(b"999999"), None);
    }
}
