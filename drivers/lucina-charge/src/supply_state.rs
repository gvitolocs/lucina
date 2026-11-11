// SPDX-License-Identifier: MPL-2.0

//! Whether the adapter is in: one state, fed by the PD controllers and the board's AC pin (patch 1197). Verus proves this very
//! file (`verification/juno-charge/rules.rs`, D-011).
//!
//! The original is Samsung's `EmuEC.sys`. AC presence is a pin (`StatusAcPin`, FUN_140015f20: GPIO function 6, GpioIo slot 4 of
//! `\_SB.EMEC` = TLMM 11, high = AC in). A PD detach is function state 0, and with no attach before it "detach event is ignored"
//! (FUN at 13473 of the decompile). Every other state of the policy engine, as in a PD hard reset, says nothing about the cable.
//! Before this rule any state outside the sink range counted as unplugged, so a transient state at boot or a hard reset by the
//! adapter was published as "unplugged" with the adapter in.
//!
//! The state is unknown until something real was read. The first value is published as what is true at boot, not as a change.
#![allow(unexpected_cfgs)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// What the PD controllers say about a charger.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pd {
    /// A port is attached as sink (`State_PE_SNK_*`).
    Sink,
    /// Every port that can be read is detached or sourcing: no charger on any.
    NoSink,
    /// A port is between states, or could not be read: nothing can be said.
    Unsettled,
}

/// What one port says, from function state byte 0: 0 is Samsung's detached, 3, 6 and 7 attached as source, `is_sink` the sink
/// states. `up` says its bus exists at all; a port with no bus says nothing against the others.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == port_spec(up, state),
))]
// The range and the match stay spelled out: Verus checks this very code.
#[allow(clippy::manual_range_contains)]
fn port(up: bool, state: Option<u8>) -> u8 {
    if !up {
        return 0;
    }
    match state {
        Some(s) if (15 <= s && s <= 25) || s == 29 => 2,
        Some(0) | Some(3) | Some(6) | Some(7) => 0,
        _ => 1,
    }
}

/// The two ports together: a sink on either is a charger; otherwise a port between states leaves it open; with no bus at all
/// nothing is known.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == pd_spec(up, states),
))]
pub fn pd_of(up: [bool; 2], states: [Option<u8>; 2]) -> Pd {
    if !up[0] && !up[1] {
        return Pd::Unsettled;
    }
    let (a, b) = (port(up[0], states[0]), port(up[1], states[1]));
    if a == 2 || b == 2 {
        Pd::Sink
    } else if a == 1 || b == 1 {
        Pd::Unsettled
    } else {
        Pd::NoSink
    }
}

/// The next state. `prev`: the state so far, `None` unknown. `ac`: the AC pin, or the charger's AC_STAT without it.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == next_spec(prev, pd, ac),
))]
#[allow(clippy::manual_map)]
pub fn next_online(prev: Option<bool>, pd: Pd, ac: Option<bool>) -> Option<bool> {
    match pd {
        // At the PD controller's word: the owner's "set it when the laptop sees the cable", before the power is negotiated.
        Pd::Sink => Some(true),
        Pd::NoSink => match ac {
            // Power without a sink port is an adapter that does not speak PD. No power is the cable gone.
            Some(a) => Some(a),
            // Nothing against it, and something was attached: a detach. With nothing before: "detach event is ignored".
            None => match prev {
                Some(_) => Some(false),
                None => None,
            },
        },
        // Between states (a hard reset drops VBUS for about a second): the state stands. Unknown so far: the pin decides.
        Pd::Unsettled => match prev {
            Some(p) => Some(p),
            None => ac,
        },
    }
}

/// What the change from `prev` to `next` is worth telling.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Publish {
    Nothing,
    /// The first real reading: what is true at boot, not a change.
    First(bool),
    Changed(bool),
}

#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == publish_spec(prev, next),
))]
pub fn publish(prev: Option<bool>, next: Option<bool>) -> Publish {
    match (prev, next) {
        (None, Some(n)) => Publish::First(n),
        (Some(p), Some(n)) if p != n => Publish::Changed(n),
        _ => Publish::Nothing,
    }
}

#[cfg(verus_keep_ghost)]
verus! {

pub open spec fn port_spec(up: bool, state: Option<u8>) -> u8 {
    if !up {
        0
    } else {
        match state {
            Some(s) => if (15 <= s && s <= 25) || s == 29 { 2 } else if s == 0 || s == 3 || s == 6 || s == 7 { 0 } else { 1 },
            None => 1,
        }
    }
}

pub open spec fn pd_spec(up: [bool; 2], states: [Option<u8>; 2]) -> Pd {
    if !up[0] && !up[1] {
        Pd::Unsettled
    } else if port_spec(up[0], states[0]) == 2 || port_spec(up[1], states[1]) == 2 {
        Pd::Sink
    } else if port_spec(up[0], states[0]) == 1 || port_spec(up[1], states[1]) == 1 {
        Pd::Unsettled
    } else {
        Pd::NoSink
    }
}

pub open spec fn next_spec(prev: Option<bool>, pd: Pd, ac: Option<bool>) -> Option<bool> {
    match pd {
        Pd::Sink => Some(true),
        Pd::NoSink => match ac {
            Some(a) => Some(a),
            None => match prev { Some(_) => Some(false), None => None },
        },
        Pd::Unsettled => match prev { Some(p) => Some(p), None => ac },
    }
}

pub open spec fn publish_spec(prev: Option<bool>, next: Option<bool>) -> Publish {
    match (prev, next) {
        (None, Some(n)) => Publish::First(n),
        (Some(p), Some(n)) => if p != n { Publish::Changed(n) } else { Publish::Nothing },
        _ => Publish::Nothing,
    }
}

/// A false "unplugged" cannot come out of a state between PD states, nor from a detach that nothing attached before.
proof fn no_false_unplugged(prev: Option<bool>, ac: Option<bool>)
    ensures
        // A state between PD states never changes a known state: a hard reset is not an unplug.
        prev.is_some() ==> next_spec(prev, Pd::Unsettled, ac) == prev,
        // Power on the pin is never "unplugged" while the PD side says no sink.
        ac == Some(true) ==> next_spec(prev, Pd::NoSink, ac) == Some(true),
        // A detach with nothing known and no pin leaves it unknown.
        next_spec(None, Pd::NoSink, None).is_none(),
{
}

/// Unknown stays unknown until something real was read; a sink is online whatever the pin says.
proof fn unknown_until_read(ac: Option<bool>)
    ensures
        next_spec(None, Pd::Unsettled, None).is_none(),
        next_spec(None, Pd::Unsettled, ac) == ac,
        next_spec(None, Pd::Sink, ac) == Some(true),
{
}

/// A real unplug is reported: the PD side says no sink and the pin has no power.
proof fn a_real_unplug_is_seen(prev: Option<bool>)
    ensures
        next_spec(prev, Pd::NoSink, Some(false)) == Some(false),
        next_spec(Some(true), Pd::NoSink, None) == Some(false),
{
}

/// An event only on a real change; the first reading is never one.
proof fn events_only_on_a_change(p: bool, n: bool)
    ensures
        publish_spec(None, Some(n)) == Publish::First(n),
        publish_spec(None, None) == Publish::Nothing,
        publish_spec(Some(p), None) == Publish::Nothing,
        p != n ==> publish_spec(Some(p), Some(n)) == Publish::Changed(n),
        publish_spec(Some(n), Some(n)) == Publish::Nothing,
{
}

/// One port between states holds the answer open, unless the other is a sink; a port with no bus does not vote.
proof fn the_ports_together()
    ensures
        pd_spec([true, true], [Some(21u8), Some(100u8)]) == Pd::Sink,
        pd_spec([true, true], [Some(0u8), Some(100u8)]) == Pd::Unsettled,
        pd_spec([true, true], [Some(0u8), None]) == Pd::Unsettled,
        pd_spec([true, true], [Some(7u8), Some(0u8)]) == Pd::NoSink,
        pd_spec([false, true], [None, Some(0u8)]) == Pd::NoSink,
        pd_spec([false, false], [None, None]) == Pd::Unsettled,
{
    assert([true, true][0]); assert([true, true][1]); assert(![false, true][0]); assert([false, true][1]);
    assert(![false, false][0]); assert(![false, false][1]);
    assert([Some(21u8), Some(100u8)][0] == Some(21u8)); assert([Some(21u8), Some(100u8)][1] == Some(100u8));
    assert([Some(0u8), Some(100u8)][0] == Some(0u8)); assert([Some(0u8), Some(100u8)][1] == Some(100u8));
    assert([Some(0u8), None::<u8>][0] == Some(0u8)); assert([Some(0u8), None::<u8>][1] == None::<u8>);
    assert([Some(7u8), Some(0u8)][0] == Some(7u8)); assert([Some(7u8), Some(0u8)][1] == Some(0u8));
    assert([None::<u8>, Some(0u8)][1] == Some(0u8));
}

} // verus!

#[cfg(test)]
mod tests {
    use super::*;

    const UP: [bool; 2] = [true, true];

    #[test]
    fn the_laptops_own_port_states() {
        // Read on the laptop: port 0 `07` attached as source, port 1 `15` (21, PE_SNK_Ready) with the adapter.
        assert_eq!(pd_of(UP, [Some(0x07), Some(0x15)]), Pd::Sink);
        assert_eq!(pd_of(UP, [Some(0x07), Some(0x00)]), Pd::NoSink);
        // Between states on the adapter's port.
        assert_eq!(pd_of(UP, [Some(0x07), Some(0x05)]), Pd::Unsettled);
        assert_eq!(pd_of(UP, [Some(0x07), None]), Pd::Unsettled);
        assert_eq!(pd_of([false, false], [None, None]), Pd::Unsettled);
    }

    #[test]
    fn boot_with_the_adapter_in_is_not_unplugged() {
        // A state between states at 1.17 s, the pin high: online, published as the first reading.
        let n = next_online(None, Pd::Unsettled, Some(true));
        assert_eq!(n, Some(true));
        assert_eq!(publish(None, n), Publish::First(true));
        // No sink yet but power on the pin: still in.
        assert_eq!(next_online(None, Pd::NoSink, Some(true)), Some(true));
        // Nothing known at all: unknown, nothing published.
        assert_eq!(next_online(None, Pd::Unsettled, None), None);
        assert_eq!(publish(None, None), Publish::Nothing);
    }

    #[test]
    fn a_hard_reset_is_not_an_unplug() {
        assert_eq!(
            next_online(Some(true), Pd::Unsettled, Some(false)),
            Some(true)
        );
    }

    #[test]
    fn plug_and_unplug() {
        assert_eq!(next_online(Some(false), Pd::Sink, Some(false)), Some(true));
        assert_eq!(
            next_online(Some(true), Pd::NoSink, Some(false)),
            Some(false)
        );
        assert_eq!(publish(Some(true), Some(false)), Publish::Changed(false));
        assert_eq!(publish(Some(true), Some(true)), Publish::Nothing);
    }
}
