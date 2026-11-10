//! When the USB-C power path may be switched to "consumer" (sink): the decision of Samsung's `EmuEC.sys` after a plug, as
//! plain logic. Verus proves this very file (`verification/juno-charge/rules.rs`); what each function promises is in its
//! `ensures`.
//!
//! The original (decompiled, `docs/research/juno-charge.md`): on an attach as sink EmuEC calls `set_CP_Switch(port,
//! consumer = 1)`, unless the port reports a CC short, an SBU short or water; with adapters on both ports it keeps the
//! stronger one as consumer and switches the other off; `Policy_Check_CP_Conflict` reads both ports' switch status first
//! and does nothing when the switch is already as wanted.
//!
//! Differences, on the safe side: with both ports attached as sink nothing is switched (EmuEC compares the adapters'
//! power, which this does not read yet); a port whose switch status could not be read is never switched; one attempt per
//! attach.
#![allow(unexpected_cfgs)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// What one port says, from its PD controller's function state and its switch status.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PortView {
    /// The policy engine is in one of Samsung's sink states.
    pub sink: bool,
    /// CC short, SBU short or water, as EmuEC reads them.
    pub fault: bool,
    /// The switch status (`03 92`): consumer on, if it could be read.
    pub consumer: Option<bool>,
    /// A consumer-on was already sent during this attach.
    pub tried: bool,
}

/// What to do.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Switch {
    Nothing,
    /// Send EmuEC's consumer-on to this port (0 or 1).
    ConsumerOn(u8),
}

#[cfg(verus_keep_ghost)]
verus! {

/// A port that may be made consumer: attached as sink, no fault, its switch read as off, not tried yet this attach.
pub open spec fn may_switch(p: PortView) -> bool {
    p.sink && !p.fault && p.consumer == Some(false) && !p.tried
}

/// A port that is, or may be, a consumer already, or would compete for it.
pub open spec fn busy(p: PortView) -> bool {
    p.sink || p.consumer != Some(false)
}

}

/// The decision for the two ports.
#[cfg_attr(verus_keep_ghost, verus_spec(s =>
    ensures
        s matches Switch::ConsumerOn(n) ==> (n == 0 || n == 1),
        s == Switch::ConsumerOn(0) ==> may_switch(*p0) && !busy(*p1),
        s == Switch::ConsumerOn(1) ==> may_switch(*p1) && !busy(*p0),
        (p0.sink && p1.sink) ==> s == Switch::Nothing,
))]
pub fn decide(p0: &PortView, p1: &PortView) -> Switch {
    let off0 = matches!(p0.consumer, Some(false));
    let off1 = matches!(p1.consumer, Some(false));
    if p0.sink && !p0.fault && off0 && !p0.tried && !p1.sink && off1 {
        Switch::ConsumerOn(0)
    } else if p1.sink && !p1.fault && off1 && !p1.tried && !p0.sink && off0 {
        Switch::ConsumerOn(1)
    } else {
        Switch::Nothing
    }
}

/// Samsung's sink states of the policy engine (`State_PE_SNK_*`, 15 to 25, and 29), by function state byte 0.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == ((15 <= state && state <= 25) || state == 29),
))]
pub fn is_sink_state(state: u8) -> bool {
    (15 <= state && state <= 25) || state == 29
}

/// The faults EmuEC reads from function state byte 3: bit 5 CC short, bit 6 SBU short; water from register `0x0060`
/// bit 3.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == (byte3 & 0x60 != 0 || water & 0x08 != 0),
))]
pub fn has_fault(byte3: u8, water: u8) -> bool {
    byte3 & 0x60 != 0 || water & 0x08 != 0
}


// ---- The PDO request after a Source_Capabilities message: EmuEC's `process_pd` ((address not published)), its PDO parse
// ((address not published)), its current limit ((address not published), read from the disassembly) and `s2mm005xxx_select_pdo`
// ((address not published)). Without it a hot-plugged adapter stays on the default 5 V PDO.

/// EmuEC keeps a PDO as the choice only below 15001 mV (`< 0x3a99`).
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const PDO_KEEP_BELOW_MV: u32 = 15001;
/// The policy-engine states EmuEC writes after the PDO number (`03 02 <state>`).
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const PE_SELECT_V0: u8 = 0x11;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const PE_SELECT: u8 = 0x13;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const PE_SELECT_LOW: u8 = 0x19;
/// Message flag: a Source_Capabilities message came (`process_pd`, bit 17 of the word at `0x0040`).
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const SRC_CAP_FLAG: u32 = 0x0002_0000;

#[cfg(verus_keep_ghost)]
verus! {

/// The number of data objects in a Source_Capabilities header (bits 14:12 of the low half-word), as EmuEC reads it:
/// none when the field is 7.
pub open spec fn objects(header: u32) -> int {
    let h = header as int % 65536;
    if h % 32768 < 0x7000 { (h / 4096) % 8 } else { 0 }
}

/// A fixed-supply PDO (bits 31:30 zero).
pub open spec fn is_fixed(raw: u32) -> bool {
    raw < 0x4000_0000
}

/// A fixed PDO's voltage in mV.
pub open spec fn pdo_mv(raw: u32) -> int {
    if is_fixed(raw) { ((raw as int / 1024) % 1024) * 50 } else { 0 }
}

/// PDO `i` was advertised by the adapter in this message: a data object of the header, fixed supply.
pub open spec fn advertised(caps: [u32; 8], i: int) -> bool {
    1 <= i <= objects(caps[0]) && is_fixed(caps[i])
}

}

/// A fixed-supply PDO's voltage (mV, bits 19:10 × 50) and current (mA, bits 9:0 × 10); `(0, 0)` for any other type,
/// which EmuEC's table does not take.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r.0 == pdo_mv(raw),
        r.0 <= 51150,
        r.1 <= 10230,
))]
pub fn fixed_pdo(raw: u32) -> (u32, u32) {
    if raw < 0x4000_0000 {
        (((raw / 1024) % 1024) * 50, (raw % 1024) * 10)
    } else {
        (0, 0)
    }
}

/// Whether EmuEC's parse counts a PDO toward the last usable one: fixed supply at or below 20 V (`units < 0x191`).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r ==> is_fixed(raw),
))]
pub fn usable_pdo(raw: u32) -> bool {
    raw < 0x4000_0000 && (raw / 1024) % 1024 < 0x191
}

/// EmuEC's current limit for a PDO ((address not published)): the port's power budget for the voltage band divided by the
/// voltage, capped per band, then never above the PDO's own current. Budgets from `0x14001ead8`: 15 W to 5 V, 25 W to
/// 9 V, 36 W to 12 V, 35 W to 15 V, 30 W to 20 V; above 20 V 452 mA; with no voltage, the PDO's current.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r <= ma,
        mv != 0 ==> r <= 3000,
        (5000 < mv && mv <= 9000) ==> r <= 2770,
        (12000 < mv && mv <= 15000) ==> r <= 2300,
        (15000 < mv && mv <= 20000) ==> r <= 1500,
        20000 < mv ==> r <= 452,
))]
pub fn limit_ma(mv: u32, ma: u32) -> u32 {
    let band = if mv == 0 {
        ma
    } else if mv <= 5000 {
        let b = 15_000_000 / mv;
        if b > 3000 { 3000 } else { b }
    } else if mv <= 9000 {
        let b = 25_000_000 / mv;
        if b > 2770 { 2770 } else { b }
    } else if mv <= 12000 {
        let b = 36_000_000 / mv;
        if b > 3000 { 3000 } else { b }
    } else if mv <= 15000 {
        let b = 35_000_000 / mv;
        if b > 2300 { 2300 } else { b }
    } else if mv <= 20000 {
        let b = 30_000_000 / mv;
        if b > 1500 { 1500 } else { b }
    } else {
        452
    };
    if band < ma { band } else { ma }
}

/// The PDO EmuEC picks from a Source_Capabilities message (32 bytes at `0x0260` as eight little-endian words: the
/// header, then up to seven PDOs): among PDOs 1 to the last usable one, the one with the most power (voltage ×
/// [`limit_ma`]; a later one wins a tie), kept only below 15001 mV. `fault` (CC short) picks PDO 1, the 5 V one every
/// source lists first. Returns the PDO number (0: none), its voltage and its limited current.
///
/// One tightening over EmuEC: PDO 1 on a fault only when the message has a data object (a Source_Capabilities always
/// has; EmuEC would name PDO 1 regardless).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r.0 <= 7,
        r.0 != 0 ==> advertised(*caps, r.0 as int),
        r.0 != 0 ==> r.1 as int == pdo_mv(caps[r.0 as int]),
        r.0 == 0 ==> r.1 == 0 && r.2 == 0,
        !fault && r.0 != 0 ==> r.1 < PDO_KEEP_BELOW_MV,
        r.0 != 0 ==> r.1 < PDO_KEEP_BELOW_MV || r.0 == 1,
        fault && advertised(*caps, 1) ==> r.0 == 1,
        r.2 <= 10230,
))]
pub fn choose_pdo(caps: &[u32; 8], fault: bool) -> (u8, u32, u32) {
    let h = caps[0] % 65536;
    let num: usize = if h % 32768 < 0x7000 { ((h / 4096) % 8) as usize } else { 0 };
    let mut last: usize = 0;
    let mut i: usize = 1;
    #[cfg_attr(verus_keep_ghost, verus_spec(
        invariant
            num as int == objects(caps[0]),
            num <= 6,
            1 <= i <= num + 1,
            last < i,
            last == 0 || advertised(*caps, last as int),
        decreases num + 1 - i
    ))]
    while i <= num {
        if usable_pdo(caps[i]) {
            last = i;
        }
        i = i + 1;
    }
    let mut sel: usize = 0;
    let mut sel_mv: u32 = 0;
    let mut sel_ma: u32 = 0;
    let mut best: u64 = 0;
    let mut i: usize = 1;
    // Entry 0 of EmuEC's table is never filled: zero power, the choice only when nothing else has power.
    #[cfg_attr(verus_keep_ghost, verus_spec(
        invariant
            num as int == objects(caps[0]),
            last <= num,
            1 <= i <= last + 1,
            sel <= last,
            sel == 0 ==> sel_mv == 0 && sel_ma == 0,
            sel != 0 ==> 1 <= sel && sel <= num && is_fixed(caps[sel as int]),
            sel != 0 ==> sel_mv as int == pdo_mv(caps[sel as int]) && sel_mv < PDO_KEEP_BELOW_MV,
            sel_ma <= 10230,
        decreases last + 1 - i
    ))]
    while i <= last {
        let (mv, ma) = fixed_pdo(caps[i]);
        let lim = limit_ma(mv, ma);
        #[cfg(verus_keep_ghost)]
        proof! {
            assert((mv as u64) * (lim as u64) <= 51150u64 * 10230u64) by (nonlinear_arith)
                requires mv <= 51150, lim <= 10230;
        }
        let watt = (mv as u64) * (lim as u64);
        if best <= watt {
            if mv < PDO_KEEP_BELOW_MV && caps[i] < 0x4000_0000 {
                sel = i;
                sel_mv = mv;
                sel_ma = lim;
            }
            best = watt;
        }
        i = i + 1;
    }
    if fault && num >= 1 && caps[1] < 0x4000_0000 {
        sel = 1;
        let (mv, ma) = fixed_pdo(caps[1]);
        sel_mv = mv;
        sel_ma = limit_ma(mv, ma);
    }
    (sel as u8, sel_mv, sel_ma)
}

/// Which policy-engine state EmuEC sends with the chosen PDO, by the PD firmware's main version (register `0x09`,
/// first byte): before version 5 always `0x11`; from 5 on `0x13` for any PDO but the first, and for PDO 1 `0x19` once
/// per attach when its current is 501 to 2999 mA, else nothing. An unread version asks for nothing.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        sel == 0 ==> r.is_none(),
        version.is_none() ==> r.is_none(),
        version.is_some() && version.unwrap() < 5 && sel != 0 ==> r == Some(PE_SELECT_V0),
        version.is_some() && version.unwrap() >= 5 && sel > 1 ==> r == Some(PE_SELECT),
        version.is_some() && version.unwrap() >= 5 && sel == 1 && low_done ==> r.is_none(),
))]
pub fn pe_state(version: Option<u8>, sel: u8, sel_ma: u32, low_done: bool) -> Option<u8> {
    match version {
        None => None,
        Some(v) => {
            if sel == 0 {
                None
            } else if v < 5 {
                Some(PE_SELECT_V0)
            } else if sel != 1 {
                Some(PE_SELECT)
            } else if 501 <= sel_ma && sel_ma <= 2999 && !low_done {
                Some(PE_SELECT_LOW)
            } else {
                None
            }
        }
    }
}

/// What one PDO round asks for: `Some((pdo, state))` to send `03 03 <pdo>` and `03 02 <state>`, or nothing.
///
/// Nothing unless the port is attached as sink, its message flags were read with a Source_Capabilities flagged, the
/// capabilities were read, the PD firmware version is known, and the chosen PDO is not the one already asked for. So a
/// detach, an unattached port or any failed read leaves the PD controller on its own default contract, the 5 V PDO
/// (USB PD: a sink that asks nothing gets vSafe5V), and a request only ever names a fixed PDO the adapter advertised in
/// that message, below 15001 mV unless it is PDO 1 on a CC fault.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        !sink ==> r.is_none(),
        flags.is_none() ==> r.is_none(),
        flags.is_some() && flags.unwrap() & SRC_CAP_FLAG == 0 ==> r.is_none(),
        caps.is_none() ==> r.is_none(),
        version.is_none() ==> r.is_none(),
        r.is_some() ==> caps.is_some() && advertised(caps.unwrap(), r.unwrap().0 as int) && r.unwrap().0 != selected,
        r.is_some() && !fault ==> pdo_mv(caps.unwrap()[r.unwrap().0 as int]) < PDO_KEEP_BELOW_MV,
        r.is_some() && fault && advertised(caps.unwrap(), 1) ==> r.unwrap().0 == 1,
        r.is_some() ==> pdo_mv(caps.unwrap()[r.unwrap().0 as int]) < PDO_KEEP_BELOW_MV || r.unwrap().0 == 1,
))]
pub fn pdo_plan(
    sink: bool,
    flags: Option<u32>,
    caps: Option<[u32; 8]>,
    version: Option<u8>,
    fault: bool,
    selected: u8,
    low_done: bool,
) -> Option<(u8, u8)> {
    if !sink {
        return None;
    }
    let f = match flags {
        Some(f) => f,
        None => return None,
    };
    if f & SRC_CAP_FLAG == 0 {
        return None;
    }
    let c = match caps {
        Some(c) => c,
        None => return None,
    };
    let (sel, _mv, ma) = choose_pdo(&c, fault);
    if sel == selected {
        return None;
    }
    match pe_state(version, sel, ma, low_done) {
        Some(pe) => Some((sel, pe)),
        None => None,
    }
}

/// The charger's input-current limit EmuEC sets after a PD contract (`Calculate_Limit_Watt` (address not published) →
/// (address not published) → `ChangeInputCurrent` (address not published)): the chosen PDO's current through [`limit_ma`], written to the
/// BQ25710's IIN_HOST (`0x3F`) as `mA / 50` in bits 14:8. A 12 V 3 A adapter gives `0x3c00`, the value the firmware
/// leaves after a cold boot. `None` for a PDO without voltage (no real contract; EmuEC would write the raw current).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        mv == 0 ==> r.is_none(),
        r.is_some() && 12000 < mv && mv <= 15000 ==> r.unwrap() <= 0x2e00,
        r.is_some() && 15000 < mv && mv <= 20000 ==> r.unwrap() <= 0x1e00,
        r.is_some() ==> r.unwrap() <= 0x3c00,
        r.is_some() ==> (r.unwrap() as int / 256) * 50 <= ma,
        r.is_some() ==> r.unwrap() % 256 == 0,
))]
pub fn iin_host(mv: u32, ma: u32) -> Option<u16> {
    if mv == 0 {
        return None;
    }
    let lim = limit_ma(mv, ma);
    Some(((lim / 50) * 256) as u16)
}

/// Whether the charge controller writes the PD contract's input limit now: only with the adapter in, a limit the PD
/// thread confirmed, and a value [`iin_host`] can give (a multiple of 50 mA, not 0, at most `0x3c00` = 3000 mA), when the
/// chip holds something else. EmuEC writes it on every contract; once it holds, nothing more is written.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        !online ==> r.is_none(),
        wanted.is_none() ==> r.is_none(),
        r.is_some() ==> online && r == wanted && r.unwrap() != now,
        r.is_some() ==> r.unwrap() != 0 && r.unwrap() <= 0x3c00 && r.unwrap() % 256 == 0,
))]
pub fn iin_write(wanted: Option<u16>, now: u16, online: bool) -> Option<u16> {
    if !online {
        return None;
    }
    match wanted {
        None => None,
        Some(w) => {
            if w != now && w != 0 && w <= 0x3c00 && w % 256 == 0 {
                Some(w)
            } else {
                None
            }
        }
    }
}

/// A new Source_Capabilities message (bit 17 of the message flags) means a new attach or a new contract: EmuEC asks the
/// consumer switch again on every attach, so the once-per-attach mark is reset.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == (flags & SRC_CAP_FLAG != 0),
))]
pub fn rearm(flags: u32) -> bool {
    flags & SRC_CAP_FLAG != 0
}
