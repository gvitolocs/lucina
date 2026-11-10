//! A read-only look at the Galaxy Book Go's USB-C power path: the Samsung PD controller (s2mm005, `0x33`) and the cable
//! detector (MUIC, `0x14`) on each port's I2C bus (`i2c@888000`, `i2c@a84000`: EmuEC's I2C3 and I2C8 in the DSDT).
//!
//! Why: the adapter gives power only after a cold power-on, when the firmware sets the port up. Samsung's Windows driver
//! `EmuEC.sys` drives these chips at every plug event; Juno does not yet. This reads what they hold, so that a plug event
//! can be seen in the log before anything is written. The register map is Samsung's GPL s2mm005 driver
//! (`drivers/ccic/s2mm005*.c`, Galaxy S7 tree) and Linux's `extcon-sm5502.c` / Samsung's `sm5508-muic.c`.
//!
//! Only reads. Not read on purpose: the MUIC's interrupt registers (`0x03`-`0x05`), which clear when read, and the PD
//! controller's interrupt block (`0x30`), whose bits the driver clears by command.

/// The PD controller: 16-bit register addresses, big endian.
pub const PDIC: u8 = 0x33;
/// The cable detector: 8-bit register addresses.
pub const MUIC: u8 = 0x14;
/// Samsung's wake-up: five reads of this register before talking to the chip.
pub const PDIC_WAKE: u16 = 0x0008;
pub const PDIC_WAKE_READS: usize = 5;
/// The function state: byte 0 is the policy engine's state, byte 2 the next one; all zero when detached.
pub const PDIC_FUNC_STATE: u16 = 0x0020;
/// The RID byte.
pub const PDIC_RID: u16 = 0x0050;
/// MUIC registers read (SM5504/SM5508 maps): never the interrupt registers.
pub const MUIC_READS: [(u8, &str); 8] = [
    (0x01, "DeviceID"),
    (0x02, "Control"),
    (0x09, "ADC"),
    (0x0a, "DeviceType1"),
    (0x0b, "DeviceType2"),
    (0x0c, "DeviceType3"),
    (0x15, "VbusValid"),
    (0x17, "ChgType"),
];

/// A byte-addressed register read: write the address bytes, then read `buf` with a repeated start.
pub trait RegRead {
    type Error;
    fn read_reg(&mut self, addr: u8, reg: &[u8], buf: &mut [u8]) -> Result<(), Self::Error>;
}

/// What one port's chips hold. `None` where a read failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Port {
    pub func_state: Option<[u8; 4]>,
    pub rid: Option<u8>,
    pub muic: [Option<u8>; MUIC_READS.len()],
}

impl Port {
    pub fn read<B: RegRead>(bus: &mut B) -> Port {
        let mut one = [0u8; 1];
        for _ in 0..PDIC_WAKE_READS {
            let _ = bus.read_reg(PDIC, &PDIC_WAKE.to_be_bytes(), &mut one);
        }
        let mut state = [0u8; 4];
        let func_state = bus
            .read_reg(PDIC, &PDIC_FUNC_STATE.to_be_bytes(), &mut state)
            .ok()
            .map(|()| state);
        let rid = bus
            .read_reg(PDIC, &PDIC_RID.to_be_bytes(), &mut one)
            .ok()
            .map(|()| one[0]);
        let mut muic = [None; MUIC_READS.len()];
        for (value, (reg, _)) in muic.iter_mut().zip(MUIC_READS) {
            *value = bus.read_reg(MUIC, &[reg], &mut one).ok().map(|()| one[0]);
        }
        Port {
            func_state,
            rid,
            muic,
        }
    }

    /// The policy engine's state, by Samsung's numbers.
    pub fn state_name(&self) -> &'static str {
        match self.func_state.map(|s| s[0]) {
            None => "unread",
            Some(0) => "detached",
            Some(3 | 6 | 7) => "attached as source",
            Some(15..=25 | 29) => "attached as sink",
            Some(_) => "other",
        }
    }
}

impl core::fmt::Display for Port {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.func_state {
            Some(s) => write!(f, "PD state {:02x} {:02x} {:02x} {:02x} ({})", s[0], s[1], s[2], s[3], self.state_name())?,
            None => f.write_str("PD state unread")?,
        }
        match self.rid {
            Some(r) => write!(f, ", RID 0x{r:02x}")?,
            None => f.write_str(", RID unread")?,
        }
        f.write_str("; MUIC")?;
        for (value, (_, name)) in self.muic.iter().zip(MUIC_READS) {
            match value {
                Some(v) => write!(f, " {name} 0x{v:02x}")?,
                None => write!(f, " {name} ?")?,
            }
        }
        Ok(())
    }
}

/// The PD controller's command window: `03 92` asks for the switch status, read back as 4 bytes at `0x0014`.
pub const PDIC_CMD: u16 = 0x0010;
pub const PDIC_CMD_RESULT: u16 = 0x0014;
/// Register with the water-detect bit (bit 3), read only.
pub const PDIC_WATER: u16 = 0x0060;
pub const GET_CP_SWITCH: [u8; 2] = [0x03, 0x92];
/// EmuEC's `set_CP_Switch(port, consumer = 1, 0)`: `03 91 X Y 00 X|Y<<1` with X = 0, Y = 1.
pub const CONSUMER_ON: [u8; 6] = [0x03, 0x91, 0x00, 0x01, 0x00, 0x02];

/// A byte-addressed register write: the address bytes, then the data, in one message.
pub trait RegWrite: RegRead {
    fn write_reg(&mut self, addr: u8, reg: &[u8], data: &[u8]) -> Result<(), Self::Error>;
}

/// The switch status: Provider, Consumer, Discharging, Collision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpStatus(pub [u8; 4]);

impl CpStatus {
    pub fn consumer(&self) -> bool {
        self.0[1] == 1
    }
}

fn wake<B: RegRead>(bus: &mut B) {
    let mut one = [0u8; 1];
    for _ in 0..PDIC_WAKE_READS {
        let _ = bus.read_reg(PDIC, &PDIC_WAKE.to_be_bytes(), &mut one);
    }
}

/// EmuEC's `get_CP_Switch_Status`: wake-up reads, `03 92` to the command window, 4 bytes from `0x0014`; once more on an
/// error.
pub fn get_cp_switch<B: RegWrite>(bus: &mut B) -> Option<CpStatus> {
    for _ in 0..2 {
        wake(bus);
        let mut status = [0u8; 4];
        let asked = bus.write_reg(PDIC, &PDIC_CMD.to_be_bytes(), &GET_CP_SWITCH).is_ok();
        if asked && bus.read_reg(PDIC, &PDIC_CMD_RESULT.to_be_bytes(), &mut status).is_ok() {
            return Some(CpStatus(status));
        }
    }
    None
}

/// The water byte, read only.
pub fn water<B: RegRead>(bus: &mut B) -> Option<u8> {
    let mut one = [0u8; 1];
    bus.read_reg(PDIC, &PDIC_WATER.to_be_bytes(), &mut one).ok().map(|()| one[0])
}

/// The PD controller's interrupt block, 48 bytes from `0x0030`; EmuEC's message flags are the word at `0x0040`.
pub const PDIC_IRQ: u16 = 0x0030;
pub const PDIC_IRQ_LEN: usize = 48;
/// The last Source_Capabilities message: header and up to seven PDOs, eight little-endian words.
pub const PDIC_SRC_CAP: u16 = 0x0260;
/// The request sent: the object position is bits 30:28 of the second word.
pub const PDIC_RDO: u16 = 0x0240;
/// EmuEC's interrupt clear: `01` to the command window.
pub const IRQ_CLEAR: [u8; 1] = [0x01];
/// EmuEC's version read: `AA` to the command window, then the main version at `0x0009` (3 bytes).
pub const VERSION_CMD: [u8; 1] = [0xaa];
pub const PDIC_MAIN_VERSION: u16 = 0x0009;

/// The PD contract's input limit for the charge controller: [`crate::pdic_rules::iin_host`] of the PDO the request
/// named, with bit 16 set; 0 for none (no confirmed contract yet, or a detach). The PD thread sets it, the charge
/// thread reads it.
static PD_IIN_HOST: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// The input limit of the confirmed PD contract, if there is one.
pub fn pd_iin_host() -> Option<u16> {
    let v = PD_IIN_HOST.load(core::sync::atomic::Ordering::Relaxed);
    (v & 0x1_0000 != 0).then_some(v as u16)
}

fn set_pd_iin_host(v: Option<u16>) {
    PD_IIN_HOST.store(v.map_or(0, |v| 0x1_0000 | u32::from(v)), core::sync::atomic::Ordering::Relaxed);
}

/// The message-flag word, read only.
pub fn msg_flags<B: RegRead>(bus: &mut B) -> Option<u32> {
    let mut block = [0u8; PDIC_IRQ_LEN];
    bus.read_reg(PDIC, &PDIC_IRQ.to_be_bytes(), &mut block).ok()?;
    Some(u32::from_le_bytes([block[16], block[17], block[18], block[19]]))
}

/// Eight little-endian words from `reg`.
pub fn read_words<B: RegRead>(bus: &mut B, reg: u16, out: &mut [u32; 8]) -> bool {
    let mut raw = [0u8; 32];
    if bus.read_reg(PDIC, &reg.to_be_bytes(), &mut raw).is_err() {
        return false;
    }
    for (i, w) in out.iter_mut().enumerate() {
        *w = u32::from_le_bytes([raw[4 * i], raw[4 * i + 1], raw[4 * i + 2], raw[4 * i + 3]]);
    }
    true
}

/// EmuEC's interrupt clear ((address not published)): wake-up, then `01` to the command window.
pub fn clear_irq<B: RegWrite>(bus: &mut B) -> bool {
    wake(bus);
    bus.write_reg(PDIC, &PDIC_CMD.to_be_bytes(), &IRQ_CLEAR).is_ok()
}

/// EmuEC's version read ((address not published)): 20 ms, `AA` to the command window, 20 ms, the main version's first byte.
pub fn main_version<B: RegWrite>(bus: &mut B, wait_ms: &mut dyn FnMut(u64)) -> Option<u8> {
    wait_ms(20);
    bus.write_reg(PDIC, &PDIC_CMD.to_be_bytes(), &VERSION_CMD).ok()?;
    wait_ms(20);
    let mut v = [0u8; 3];
    bus.read_reg(PDIC, &PDIC_MAIN_VERSION.to_be_bytes(), &mut v).ok()?;
    Some(v[0])
}

/// EmuEC's `s2mm005xxx_select_pdo` ((address not published)): wake-up, `03 03 <pdo>`, then `03 02 <policy-engine state>`.
pub fn select_pdo<B: RegWrite>(bus: &mut B, pdo: u8, state: u8) -> bool {
    wake(bus);
    let a = bus.write_reg(PDIC, &PDIC_CMD.to_be_bytes(), &[0x03, 0x03, pdo]).is_ok();
    let b = bus.write_reg(PDIC, &PDIC_CMD.to_be_bytes(), &[0x03, 0x02, state]).is_ok();
    a && b
}

/// Plug handling for both ports, after each [`Port::read`]: in `on` it turns the sink port's consumer switch on as EmuEC
/// does (by [`crate::pdic_rules::decide`]) and asks for the PDO EmuEC would (by [`crate::pdic_rules::choose_pdo`]); in
/// report it only says what it would do.
pub struct Consumer {
    pub on: bool,
    tried: [bool; 2],
    sink: [bool; 2],
    said: [bool; 2],
    pd: [PdoState; 2],
    /// This round's message flags per port, read once and the interrupt cleared after (EmuEC's order).
    flags: [Option<u32>; 2],
    /// The cable as last reported: a port attached as sink.
    cable: Option<bool>,
}

/// EmuEC's PDO state for one port: the PD firmware's main version, the PDO last asked for (`DAT_140044828`), the last
/// Source_Capabilities words (to tell a changed message), the once-per-attach `0x19` flag, and the attach seen.
#[derive(Clone, Copy, Debug, Default)]
struct PdoState {
    version: Option<u8>,
    selected: u8,
    caps: [u32; 8],
    low_done: bool,
    sink: bool,
}

impl Consumer {
    pub const fn new(on: bool) -> Consumer {
        Consumer {
            on,
            tried: [false; 2],
            sink: [false; 2],
            said: [false; 2],
            pd: [PdoState { version: None, selected: 0, caps: [0; 8], low_done: false, sink: false }; 2],
            flags: [None; 2],
            cable: None,
        }
    }

    /// One round. `buses[n]` is port n's bus if it is up; `ports` what [`Port::read`] just read; `wait_ms` sleeps.
    /// The cable, from the ports just read: `Some(in)` when it changed (a port attached as sink, or none), for the very first
    /// action after the interrupt, before any register is written. `up` says which ports' buses are up; a port whose state could
    /// not be read leaves the cable as it was.
    pub fn cable_change(&mut self, ports: &[Port; 2], up: [bool; 2]) -> Option<bool> {
        use crate::pdic_rules::is_sink_state;
        if (0..2).any(|n| up[n] && ports[n].func_state.is_none()) {
            return None;
        }
        let attached = (0..2).any(|n| up[n] && ports[n].func_state.is_some_and(|s| is_sink_state(s[0])));
        if self.cable == Some(attached) {
            return None;
        }
        self.cable = Some(attached);
        Some(attached)
    }

    /// First EmuEC's interrupt read ((address not published): the message flags, then the interrupt cleared, on every interrupt), then the
    /// consumer switch, then the PDO request. A new Source_Capabilities is a new attach: the consumer switch is asked again
    /// (a detach between two looks is not missed). Returns whether a port had anything flagged, for the charge thread.
    pub fn round<B: RegWrite>(
        &mut self,
        buses: &mut [Option<&mut B>; 2],
        ports: &[Port; 2],
        wait_ms: &mut dyn FnMut(u64),
        log: &mut dyn FnMut(core::fmt::Arguments<'_>),
    ) -> bool {
        let mut active = false;
        for n in 0..2 {
            self.flags[n] = None;
            if let Some(bus) = buses[n].as_deref_mut() {
                let f = msg_flags(bus);
                if self.on {
                    clear_irq(bus);
                }
                if f.is_some_and(|f| crate::pdic_rules::rearm(f)) {
                    self.tried[n] = false;
                    self.said[n] = false;
                }
                active |= f.is_some_and(|f| f != 0);
                self.flags[n] = f;
            }
        }
        self.consumer_round(buses, ports, wait_ms, log);
        for n in 0..2 {
            if let Some(bus) = buses[n].as_deref_mut() {
                self.pdo_round(n, bus, &ports[n], wait_ms, log);
            }
        }
        active
    }

    /// EmuEC's `process_pd` for one port, polled: when the message flags say a Source_Capabilities came (bit 17), clear
    /// the interrupt, read the capabilities (`0x0260`), pick a PDO by [`crate::pdic_rules::choose_pdo`] and send it with
    /// the policy-engine state of [`crate::pdic_rules::pe_state`]; 20 ms later read the request (`0x0240`) and act on its
    /// object position as EmuEC does. Report mode only reads and says what it would ask for.
    fn pdo_round<B: RegWrite>(
        &mut self,
        n: usize,
        bus: &mut B,
        port: &Port,
        wait_ms: &mut dyn FnMut(u64),
        log: &mut dyn FnMut(core::fmt::Arguments<'_>),
    ) {
        use crate::pdic_rules::{
            PE_SELECT, PE_SELECT_LOW, SRC_CAP_FLAG, choose_pdo, fixed_pdo, is_sink_state, pdo_plan,
        };
        let sink = port.func_state.is_some_and(|s| is_sink_state(s[0]));
        let on = self.on;
        let read_flags = self.flags[n];
        let st = &mut self.pd[n];
        if sink != st.sink {
            // EmuEC forgets the asked-for PDO on a detach; the 0x19 flag is kept per attach here.
            st.sink = sink;
            st.selected = 0;
            st.low_done = false;
            // A detach ends the contract: the chip goes back to its own input limit.
            set_pd_iin_host(None);
        }
        if !sink {
            return;
        }
        let Some(flags) = read_flags else { return };
        if flags & SRC_CAP_FLAG == 0 {
            return;
        }
        let mut caps = [0u32; 8];
        if !read_words(bus, PDIC_SRC_CAP, &mut caps) {
            log(format_args!("port {n}: Source_Capabilities flagged, read failed"));
            return;
        }
        let byte3 = port.func_state.map_or(0, |s| s[3]);
        let cc_short = byte3 & 0x20 != 0;
        let (sel, mv, ma) = choose_pdo(&caps, cc_short);
        if !on {
            if caps != st.caps {
                st.caps = caps;
                log(format_args!(
                    "port {n}: Source_Capabilities {caps:08x?}; report: would ask for PDO {sel} ({mv} mV, {ma} mA), IIN_HOST {:?}", crate::pdic_rules::iin_host(mv, ma)
                ));
            }
            return;
        }
        // The contract the controller holds now (0x0240): after a detach missed between two looks, what was asked for before
        // is no guide (it would keep a fresh 5 V contract).
        let mut held = [0u32; 8];
        if read_words(bus, PDIC_RDO, &mut held) {
            st.selected = ((held[1] >> 28) & 7) as u8;
        }
        let changed = (1..8).any(|i| fixed_pdo(caps[i]) != fixed_pdo(st.caps[i]));
        st.caps = caps;
        if st.version.is_none() {
            st.version = main_version(bus, wait_ms);
            log(format_args!("port {n}: PD firmware main version {:?}", st.version));
        }
        log(format_args!(
            "port {n}: Source_Capabilities {caps:08x?}: PDO {sel} ({mv} mV, {ma} mA); EmuEC input limit IIN_HOST {:?} (not written yet)",
            crate::pdic_rules::iin_host(mv, ma)
        ));
        // The proved decision: only a PDO the adapter advertised, nothing after a failed read or with no version.
        match pdo_plan(true, Some(flags), Some(caps), st.version, cc_short, st.selected, st.low_done) {
            Some((pdo, pe)) => {
                let sent = select_pdo(bus, pdo, pe);
                st.selected = pdo;
                if pe == PE_SELECT_LOW {
                    st.low_done = true;
                }
                log(format_args!(
                    "port {n}: asked for PDO {pdo} (03 03 {pdo:02x}, 03 02 {pe:02x}): {}",
                    if sent { "sent" } else { "NOT sent: bus error" }
                ));
            }
            None => log(format_args!(
                "port {n}: no PDO request (version {:?}, already asked for {})",
                st.version, st.selected
            )),
        }
        if st.version.is_none() {
            return;
        }
        wait_ms(20);
        let mut rdo = [0u32; 8];
        if sel == 0 || !read_words(bus, PDIC_RDO, &mut rdo) {
            return;
        }
        let pos = ((rdo[1] >> 28) & 7) as u8;
        if pos == sel {
            // The request names the PDO EmuEC chose: its input limit (Calculate_Limit_Watt → ChangeInputCurrent).
            set_pd_iin_host(crate::pdic_rules::iin_host(mv, ma));
        }
        if pos == st.selected {
            if changed {
                st.selected = 0;
                log(format_args!("port {n}: request names PDO {pos}: power negotiation is requested"));
            }
        } else if st.selected == 0 {
            st.selected = pos;
            log(format_args!("port {n}: request names PDO {pos}, taken as the selection"));
        } else {
            let sent = select_pdo(bus, sel, PE_SELECT);
            st.selected = sel;
            log(format_args!(
                "port {n}: request names PDO {pos}, not {sel}: set again (03 02 13): {}",
                if sent { "sent" } else { "NOT sent: bus error" }
            ));
        }
    }

    /// EmuEC's consumer switch, once per attach.
    fn consumer_round<B: RegWrite>(
        &mut self,
        buses: &mut [Option<&mut B>; 2],
        ports: &[Port; 2],
        wait_ms: &mut dyn FnMut(u64),
        log: &mut dyn FnMut(core::fmt::Arguments<'_>),
    ) {
        use crate::pdic_rules::{PortView, Switch, decide, has_fault, is_sink_state};
        let mut views = [PortView { sink: false, fault: false, consumer: None, tried: false }; 2];
        for n in 0..2 {
            let state = ports[n].func_state;
            let sink = buses[n].is_some() && state.is_some_and(|s| is_sink_state(s[0]));
            if sink != self.sink[n] {
                // A new attach, or a detach: one attempt per attach.
                self.tried[n] = false;
                self.said[n] = false;
                self.sink[n] = sink;
            }
            views[n].sink = sink;
            views[n].tried = self.tried[n];
        }
        if !views.iter().any(|v| v.sink && !v.tried) {
            return;
        }
        if !self.on {
            for n in 0..2 {
                if views[n].sink && !self.said[n] {
                    self.said[n] = true;
                    log(format_args!(
                        "port {n}: attached as sink; report: would ask its switch status and turn the consumer switch on if off (lucina.pdic=on)"
                    ));
                }
            }
            return;
        }
        for n in 0..2 {
            let Some(bus) = buses[n].as_deref_mut() else { continue };
            let byte3 = ports[n].func_state.map_or(0, |s| s[3]);
            views[n].fault = match water(bus) {
                Some(w) => has_fault(byte3, w),
                None => true,
            };
            let status = get_cp_switch(bus);
            views[n].consumer = status.map(|s| s.consumer());
            log(format_args!("port {n}: switch status {:?}", status.map(|s| s.0)));
        }
        let Switch::ConsumerOn(n) = decide(&views[0], &views[1]) else {
            for n in 0..2 {
                // Asked once this attach: not again until the next one (EmuEC asks on plug events, not on a timer).
                if views[n].sink {
                    self.tried[n] = true;
                }
                if views[n].sink && !self.said[n] {
                    self.said[n] = true;
                    log(format_args!("port {n}: attached as sink, nothing to switch ({:?})", views[n]));
                }
            }
            return;
        };
        let n = usize::from(n);
        self.tried[n] = true;
        let Some(bus) = buses[n].as_deref_mut() else { return };
        wake(bus);
        let sent = bus.write_reg(PDIC, &PDIC_CMD.to_be_bytes(), &CONSUMER_ON).is_ok();
        wait_ms(2);
        let after = get_cp_switch(bus);
        log(format_args!(
            "port {n}: consumer switch on {} (03 91 00 01 00 02), status now {:?}",
            if sent { "sent" } else { "NOT sent: bus error" },
            after.map(|s| s.0)
        ));
    }
}
