//! A host-side model of a UFSHCI controller with a UFS device behind it, for the tests.
//!
//! It implements what the driver uses, the way QEMU's `hw/ufs` and the specs describe it: HCE enable and reset,
//! DME_LINK_STARTUP / DME_GET / DME_HIBER_EXIT, the single-door-bell transfer request list, NOP OUT, query requests
//! (descriptors, fDeviceInit), and SCSI commands on logical units backed by byte vectors. Requests run synchronously
//! when the door bell is written. Knobs reproduce what the laptop's firmware may leave behind (an enabled controller
//! with a running link, a link in hibernate) and failures (a request that never completes, a clear that does not
//! work, UNIT ATTENTION after reset).

use alloc::{format, rc::Rc, string::String, vec, vec::Vec};
use core::{
    cell::{Cell, RefCell},
    fmt,
};

use crate::{
    host::{Clock, DmaArea, IrqWait, Regs},
    regs,
    upiu::{self, UPIU_HEADER_AND_TSF},
};

pub const MEM_BASE: u64 = 0x8000_0000;
pub const MEM_SIZE: usize = 0x20_0000;
/// Where the tests put the driver's area and their data buffers inside the model's memory.
pub const AREA_DADDR: u64 = MEM_BASE;
pub const DATA_DADDR: u64 = MEM_BASE + 0x10_0000;

#[derive(Clone, Debug)]
pub struct Lu {
    pub lun: u8,
    pub enabled: u8,
    pub boot_lun_id: u8,
    pub write_protect: u8,
    pub memory_type: u8,
    pub log2_bs: u8,
    pub data: Vec<u8>,
    /// Report UNIT ATTENTION to the next command, as after a reset.
    pub unit_attention: bool,
    /// The Caching mode page's WCE and RCD; `None`: MODE SENSE is an invalid opcode.
    pub caching: Option<(bool, bool)>,
    /// MODE SENSE answers carry an 8-byte block descriptor.
    pub mode_block_descriptor: bool,
}

impl Lu {
    pub fn new(lun: u8, blocks: usize, log2_bs: u8) -> Self {
        let mut data = vec![0u8; blocks << log2_bs];
        // Recognisable contents: each block starts with its LUN and block number.
        for (i, b) in data.chunks_mut(1 << log2_bs).enumerate() {
            b[0] = lun;
            b[1..9].copy_from_slice(&(i as u64).to_le_bytes());
        }
        Self {
            lun,
            enabled: 1,
            boot_lun_id: 0,
            write_protect: 0,
            memory_type: 0,
            log2_bs,
            data,
            unit_attention: false,
            caching: Some((true, false)),
            mode_block_descriptor: true,
        }
    }
    fn bs(&self) -> usize {
        1 << self.log2_bs
    }
    fn blocks(&self) -> u64 {
        (self.data.len() >> self.log2_bs) as u64
    }
}

#[derive(Default, Debug)]
pub struct Counters {
    pub reg_writes: u32,
    pub hce_writes: u32,
    /// Writes to UTRIACR, which the driver must never make.
    pub utrilacr_writes: u32,
    /// Every write to UTRLRSR and UTRLBA, in order: ("rsr", value) / ("ba", value).
    pub list_writes: Vec<(&'static str, u32)>,
    pub uic: Vec<u8>,
    /// Every SCSI command that reached the device: (LUN, opcode).
    pub scsi: Vec<(u8, u8)>,
    pub queries: Vec<(u8, u8, u8)>,
}

pub struct State {
    pub mem: Vec<u8>,
    pub cap: u32,
    pub ver: u32,
    pub hce: u32,
    pub hcs: u32,
    pub is: u32,
    pub ie: u32,
    pub utrlba: u32,
    pub utrlbau: u32,
    pub utmrlba: u32,
    pub utmrlbau: u32,
    pub utrldbr: u32,
    pub utrlrsr: u32,
    pub utmrlrsr: u32,
    pub arg: [u32; 3],
    pub lus: Vec<Lu>,
    pub device_init: bool,
    /// fDeviceInit reads that still return 1 after it was set.
    pub device_init_polls: u32,
    pub hibernated: bool,
    /// The controller takes the door bell this many UTRLDBR reads late; until then the bell reads 0. Seen on the laptop?
    pub bell_lag: u32,
    /// The kernel has the controller's interrupt: [`Clock::wait_interrupt`] sleeps (here: counts and lets time pass).
    pub irq: bool,
    pub irq_waits: u32,
    /// With [`Self::irq`]: the controller's interrupt never reaches the thread (a lost completion interrupt). Every
    /// [`Clock::wait_interrupt`] sleeps its whole timeout and ends with none; the controller still finishes requests.
    pub irq_lost: bool,
    /// The longest sleep the host asked [`Clock::wait_interrupt`] for.
    pub irq_wait_max_us: u64,
    /// [`Clock::missed_interrupt`] calls.
    pub missed: u32,
    pending_bell: Option<(u32, u32)>,
    /// The slot comes back at once and the descriptor is never touched: a list base not taken.
    pub base_ignored: bool,
    /// Door bells rung.
    pub bells: u32,
    /// Every NOP OUT fails, whatever the link's state.
    pub nop_fails: bool,
    /// DME_GET and DME_PEER_GET fail with this result (QEMU 10.2 does for the attributes it does not know).
    pub dme_get_result: Option<u32>,
    /// The next request never completes; `clear_works` decides whether UTRLCLR takes it back.
    pub hang_next: bool,
    pub clear_works: bool,
    pub max_lu_32: bool,
    /// UTRIACR as the firmware left it (read-only for the driver).
    pub utrilacr: u32,
    /// UIC error registers; they clear when read.
    pub uec: [u32; 5],
    /// The next request hangs and IS gets this value (a fatal interrupt bit).
    pub hang_and_fatal: Option<u32>,
    /// The next request hangs, IS.UE is set and the UEC registers read these values.
    pub hang_and_ue: Option<[u32; 5]>,
    /// The first N requests complete with this OCS instead of their own.
    pub ocs_fail: (u32, u8),
    /// The first N SCSI commands answer with status TASK ABORTED (0x40).
    pub task_aborted: u32,
    /// The next command's response carries this residual count and overflow flag.
    pub residual: Option<(u32, bool)>,
    /// The first N NOP OUTs never complete.
    pub hang_nop: u32,
    /// The first N device-descriptor reads fail with OCS_PEER_COMM_FAILURE.
    pub query_fail: u32,
    /// HCS.UPMCRS after DME_HIBER_EXIT.
    pub hiber_exit_upmcrs: u32,
    /// The driver gave its DMA area back.
    pub area_freed: bool,
    /// What the driver logged.
    pub notes: Vec<String>,
    pub n: Counters,
}

#[derive(Clone)]
pub struct Model(pub Rc<RefCell<State>>);

impl Model {
    /// A controller as at power-on: disabled, no link.
    pub fn cold(lus: Vec<Lu>) -> Self {
        Self(Rc::new(RefCell::new(State {
            mem: vec![0u8; MEM_SIZE],
            // 32 slots, 8 task slots, 64-bit addressing.
            cap: 0x1f | (7 << 16) | regs::CAP_64AS,
            ver: 0x0210,
            hce: 0,
            hcs: 0,
            is: 0,
            ie: 0,
            utrlba: 0,
            utrlbau: 0,
            utmrlba: 0,
            utmrlbau: 0,
            utrldbr: 0,
            utrlrsr: 0,
            utmrlrsr: 0,
            arg: [0; 3],
            lus,
            device_init: false,
            device_init_polls: 2,
            hibernated: false,
            nop_fails: false,
            bell_lag: 0,
            irq: false,
            irq_waits: 0,
            irq_lost: false,
            irq_wait_max_us: 0,
            missed: 0,
            pending_bell: None,
            base_ignored: false,
            bells: 0,
            dme_get_result: None,
            area_freed: false,
            notes: Vec::new(),
            hang_next: false,
            clear_works: true,
            max_lu_32: false,
            utrilacr: 0,
            uec: [0; 5],
            hang_and_fatal: None,
            hang_and_ue: None,
            ocs_fail: (0, 0),
            task_aborted: 0,
            residual: None,
            hang_nop: 0,
            query_fail: 0,
            hiber_exit_upmcrs: 1,
            n: Counters::default(),
        })))
    }

    /// A controller the firmware brought up and left running, its lists in the firmware's memory.
    pub fn firmware_left(lus: Vec<Lu>) -> Self {
        let m = Self::cold(lus);
        {
            let mut s = m.0.borrow_mut();
            s.hce = regs::HCE_ENABLE;
            s.hcs = regs::HCS_DP | regs::HCS_UTRLRDY | regs::HCS_UTMRLRDY | regs::HCS_UCRDY;
            s.utrlba = 0x9000_0000;
            s.utmrlba = 0x9000_1000;
            s.utrlrsr = 1;
            s.utmrlrsr = 1;
            s.ie = 0x30c5f;
        }
        m
    }

    pub fn regs(&self) -> ModelRegs {
        ModelRegs(self.clone())
    }
    pub fn area(&self) -> ModelArea {
        ModelArea(self.clone(), AREA_DADDR)
    }
    pub fn clock(&self) -> ModelClock {
        ModelClock(Cell::new(0), self.clone())
    }
    pub fn lu(&self, lun: u8) -> Lu {
        self.0
            .borrow()
            .lus
            .iter()
            .find(|l| l.lun == lun)
            .unwrap()
            .clone()
    }
    pub fn mem_write(&self, daddr: u64, d: &[u8]) {
        let o = (daddr - MEM_BASE) as usize;
        self.0.borrow_mut().mem[o..o + d.len()].copy_from_slice(d);
    }
    pub fn mem_read(&self, daddr: u64, len: usize) -> Vec<u8> {
        let o = (daddr - MEM_BASE) as usize;
        self.0.borrow().mem[o..o + len].to_vec()
    }
}

pub struct ModelRegs(Model);
pub struct ModelArea(Model, u64);

impl Drop for ModelArea {
    fn drop(&mut self) {
        self.0.0.borrow_mut().area_freed = true;
    }
}
pub struct ModelClock(Cell<u64>, Model);

impl Clock for ModelClock {
    fn now_us(&self) -> u64 {
        // Time moves on every look, so every wait ends.
        let t = self.0.get() + 1;
        self.0.set(t);
        t
    }
    fn delay_us(&self, us: u64) {
        self.0.set(self.0.get() + us);
    }
    fn wait_interrupt(&self, timeout_us: u64) -> IrqWait {
        let mut st = self.1.0.borrow_mut();
        if !st.irq {
            return IrqWait::Unavailable;
        }
        st.irq_waits += 1;
        assert!(timeout_us >= 1);
        st.irq_wait_max_us = st.irq_wait_max_us.max(timeout_us);
        if st.irq_lost {
            self.0.set(self.0.get() + timeout_us);
            return IrqWait::Expired;
        }
        self.0.set(self.0.get() + 1);
        IrqWait::Interrupted
    }
    fn missed_interrupt(&self, _late_us: u64) {
        self.1.0.borrow_mut().missed += 1;
    }
    fn note(&self, line: fmt::Arguments<'_>) {
        self.1.0.borrow_mut().notes.push(format!("{line}"));
    }
    fn platform_report(&self) {
        self.1.0.borrow_mut().notes.push("platform report".into());
    }
}

impl DmaArea for ModelArea {
    fn daddr(&self) -> u64 {
        self.1
    }
    fn read(&self, offset: usize, buf: &mut [u8]) {
        let o = (self.1 - MEM_BASE) as usize + offset;
        buf.copy_from_slice(&self.0.0.borrow().mem[o..o + buf.len()]);
    }
    fn write(&self, offset: usize, buf: &[u8]) {
        let o = (self.1 - MEM_BASE) as usize + offset;
        self.0.0.borrow_mut().mem[o..o + buf.len()].copy_from_slice(buf);
    }
}

impl Regs for ModelRegs {
    fn read32(&self, offset: usize) -> u32 {
        let mut s = self.0.0.borrow_mut();
        if offset == regs::UTRLDBR
            && let Some((slot, left)) = s.pending_bell
        {
            if left == 0 {
                s.pending_bell = None;
                run_slot(&mut s, slot);
            } else {
                s.pending_bell = Some((slot, left - 1));
            }
        }
        match offset {
            regs::CAP => s.cap,
            regs::VER => s.ver,
            regs::IS => s.is,
            regs::IE => s.ie,
            regs::HCS => s.hcs,
            regs::HCE => s.hce,
            regs::UTRIACR => s.utrilacr,
            regs::UECPA | regs::UECDL | regs::UECN | regs::UECT | regs::UECDME => {
                const UEC: [usize; 5] = [
                    regs::UECPA,
                    regs::UECDL,
                    regs::UECN,
                    regs::UECT,
                    regs::UECDME,
                ];
                // The UEC registers clear when read.
                core::mem::take(&mut s.uec[UEC.iter().position(|r| *r == offset).unwrap()])
            }
            regs::UTRLBA => s.utrlba,
            regs::UTRLBAU => s.utrlbau,
            regs::UTRLDBR => s.utrldbr,
            regs::UTRLRSR => s.utrlrsr,
            regs::UTMRLBA => s.utmrlba,
            regs::UTMRLBAU => s.utmrlbau,
            regs::UTMRLRSR => s.utmrlrsr,
            regs::UCMDARG1 => s.arg[0],
            regs::UCMDARG2 => s.arg[1],
            regs::UCMDARG3 => s.arg[2],
            _ => 0,
        }
    }

    fn write32(&self, offset: usize, v: u32) {
        let mut s = self.0.0.borrow_mut();
        s.n.reg_writes += 1;
        match offset {
            regs::IS => s.is &= !v,
            regs::IE => s.ie = v,
            regs::HCE => {
                s.n.hce_writes += 1;
                if v & 1 != 0 && s.hce == 0 {
                    s.hce = 1;
                    s.hcs = regs::HCS_UCRDY;
                } else if v & 1 == 0 && s.hce != 0 {
                    // Reset: lists stop, link down, requests dropped, units report UNIT ATTENTION.
                    s.hce = 0;
                    s.hcs = 0;
                    s.utrldbr = 0;
                    s.utrlrsr = 0;
                    s.utmrlrsr = 0;
                    s.device_init = false;
                    s.hibernated = false;
                    for l in s.lus.iter_mut() {
                        l.unit_attention = true;
                    }
                }
            }
            regs::UTRLBA => {
                s.n.list_writes.push(("ba", v));
                s.utrlba = v & !0x3ff
            }
            regs::UTRLBAU => s.utrlbau = v,
            regs::UTMRLBA => s.utmrlba = v & !0x3ff,
            regs::UTMRLBAU => s.utmrlbau = v,
            regs::UTRLRSR => {
                s.n.list_writes.push(("rsr", v));
                s.utrlrsr = v & 1
            }
            regs::UTMRLRSR => s.utmrlrsr = v & 1,
            regs::UTRLCLR => {
                if s.clear_works {
                    s.utrldbr &= v;
                }
            }
            regs::UCMDARG1 => s.arg[0] = v,
            regs::UCMDARG2 => s.arg[1] = v,
            regs::UCMDARG3 => s.arg[2] = v,
            regs::UICCMD => uic(&mut s, v as u8),
            regs::UTRLDBR => {
                if s.hce == 0 || s.utrlrsr == 0 {
                    return;
                }
                s.bells += 1;
                if s.base_ignored {
                    return;
                }
                if let Some(f) = s.hang_and_fatal.take() {
                    s.utrldbr |= v;
                    s.is |= f;
                    return;
                }
                if let Some(uec) = s.hang_and_ue.take() {
                    s.utrldbr |= v;
                    s.is |= regs::IS_UE;
                    s.uec = uec;
                    return;
                }
                if s.bell_lag > 0 {
                    s.pending_bell = Some((v.trailing_zeros(), s.bell_lag));
                    return;
                }
                s.utrldbr |= v;
                for slot in 0..32 {
                    if v & (1 << slot) != 0 {
                        if s.hang_next {
                            s.hang_next = false;
                            continue;
                        }
                        if s.hang_nop > 0 && is_nop(&s, slot) {
                            s.hang_nop -= 1;
                            continue;
                        }
                        run_slot(&mut s, slot);
                        s.utrldbr &= !(1 << slot);
                    }
                }
            }
            regs::UTRIACR => s.n.utrilacr_writes += 1,
            _ => {}
        }
    }
}

fn uic(s: &mut State, cmd: u8) {
    s.n.uic.push(cmd);
    let result = match cmd {
        regs::UIC_DME_LINK_STARTUP => {
            s.hcs |= regs::HCS_DP | regs::HCS_UTRLRDY | regs::HCS_UTMRLRDY;
            0
        }
        regs::UIC_DME_GET => match s.dme_get_result {
            Some(r) => r,
            None => {
                s.arg[2] = match (s.arg[0] >> 16) as u16 {
                    regs::PA_RXGEAR | regs::PA_TXGEAR => 3,
                    regs::PA_PWRMODE => 0x11,
                    regs::MPHY_TX_FSM_STATE if s.hibernated => regs::TX_FSM_HIBERN8,
                    regs::MPHY_TX_FSM_STATE => 3,
                    _ => 1,
                };
                0
            }
        },
        regs::UIC_DME_PEER_GET => {
            if s.hibernated {
                8 // PEER_COMM_FAILURE
            } else if let Some(r) = s.dme_get_result {
                r
            } else {
                s.arg[2] = 0x2;
                0
            }
        }
        regs::UIC_DME_HIBER_EXIT if s.hibernated => {
            s.hibernated = false;
            s.is |= regs::IS_UHXS;
            s.hcs = (s.hcs & !(regs::HCS_UPMCRS_MASK << regs::HCS_UPMCRS_SHIFT))
                | (s.hiber_exit_upmcrs << regs::HCS_UPMCRS_SHIFT);
            0
        }
        // Among these, as on the laptop: hibernate exit on a link that is not hibernating fails with FAILURE.
        _ => 1,
    };
    s.arg[1] = result;
    s.is |= regs::IS_UCCS;
}

fn rd(s: &State, daddr: u64, len: usize) -> Vec<u8> {
    let o = (daddr - MEM_BASE) as usize;
    s.mem[o..o + len].to_vec()
}

/// Whether the request in `slot` is a NOP OUT.
fn is_nop(s: &State, slot: u32) -> bool {
    let base = (s.utrlba as u64 | (s.utrlbau as u64) << 32) + slot as u64 * 32;
    let utrd = rd(s, base, 32);
    let ucd = u32::from_le_bytes(utrd[16..20].try_into().unwrap()) as u64
        | (u32::from_le_bytes(utrd[20..24].try_into().unwrap()) as u64) << 32;
    rd(s, ucd, 1)[0] == upiu::TT_NOP_OUT
}

fn wr(s: &mut State, daddr: u64, d: &[u8]) {
    let o = (daddr - MEM_BASE) as usize;
    s.mem[o..o + d.len()].copy_from_slice(d);
}

fn run_slot(s: &mut State, slot: u32) {
    let base = (s.utrlba as u64 | (s.utrlbau as u64) << 32) + slot as u64 * 32;
    let utrd = rd(s, base, 32);
    let dw = |i: usize| u32::from_le_bytes(utrd[i * 4..i * 4 + 4].try_into().unwrap());
    let ucd = dw(4) as u64 | (dw(5) as u64) << 32;
    let rsp = ucd + ((dw(6) >> 16) as u64) * 4;
    let prdt_n = (dw(7) & 0xffff) as usize;
    let prdt = ucd + ((dw(7) >> 16) as u64) * 4;
    let dd = (dw(0) >> 25) & 3;
    let req = rd(s, ucd, UPIU_HEADER_AND_TSF);
    let mut segs = Vec::new();
    for i in 0..prdt_n {
        let e = rd(s, prdt + i as u64 * 16, 16);
        let addr = u64::from_le_bytes(e[0..8].try_into().unwrap());
        let len = (u32::from_le_bytes(e[12..16].try_into().unwrap()) & 0x3ffff) as usize + 1;
        segs.push((addr, len));
    }
    let mut out = [0u8; 512];
    out[3] = req[3];
    let mut ocs = match req[0] {
        upiu::TT_NOP_OUT => {
            if s.hibernated || s.nop_fails {
                upiu::OCS_PEER_COMM_FAILURE
            } else {
                out[0] = upiu::TT_NOP_IN;
                0
            }
        }
        upiu::TT_QUERY_REQ => {
            out[0] = upiu::TT_QUERY_RSP;
            out[5] = req[5];
            out[12..16].copy_from_slice(&req[12..16]);
            s.n.queries.push((req[12], req[13], req[14]));
            query(s, &req, &mut out);
            0
        }
        upiu::TT_COMMAND => {
            out[0] = upiu::TT_RESPONSE;
            out[2] = req[2];
            scsi(s, &req, dd, &segs, &mut out)
        }
        _ => upiu::OCS_INVALID_CMD_TABLE_ATTR,
    };
    if s.ocs_fail.0 > 0 {
        s.ocs_fail.0 -= 1;
        ocs = s.ocs_fail.1;
    }
    if req[0] == upiu::TT_QUERY_REQ
        && req[12] == upiu::QUERY_OP_READ_DESC
        && req[13] == crate::desc::IDN_DEVICE
        && s.query_fail > 0
    {
        s.query_fail -= 1;
        ocs = upiu::OCS_PEER_COMM_FAILURE;
    }
    if req[0] == upiu::TT_COMMAND
        && let Some((residual, overflow)) = s.residual.take()
    {
        out[12..16].copy_from_slice(&residual.to_be_bytes());
        if overflow {
            out[1] |= 0x40;
        }
    }
    wr(s, rsp, &out);
    let mut done = utrd.clone();
    done[8] = ocs;
    wr(s, base, &done);
    if ocs != 0 || dw(0) & (1 << 24) != 0 {
        s.is |= regs::IS_UTRCS;
    }
}

fn desc_reply(out: &mut [u8; 512], d: &[u8]) {
    out[10..12].copy_from_slice(&(d.len() as u16).to_be_bytes());
    out[18..20].copy_from_slice(&(d.len() as u16).to_be_bytes());
    out[32..32 + d.len()].copy_from_slice(d);
}

fn query(s: &mut State, req: &[u8], out: &mut [u8; 512]) {
    let (op, idn, index) = (req[12], req[13], req[14]);
    match (op, idn) {
        (upiu::QUERY_OP_READ_DESC, crate::desc::IDN_DEVICE) => {
            let mut d = [0u8; 0x59];
            d[0] = 0x59;
            d[1] = 0;
            d[6] = s.lus.iter().filter(|l| l.enabled != 0).count() as u8;
            d[7] = 4;
            d[0x10..0x12].copy_from_slice(&0x0310u16.to_be_bytes());
            d[0x18..0x1a].copy_from_slice(&0x01ceu16.to_be_bytes());
            d[0x1a] = 0x10;
            desc_reply(out, &d);
        }
        (upiu::QUERY_OP_READ_DESC, crate::desc::IDN_GEOMETRY) => {
            let mut d = [0u8; 0x57];
            d[0] = 0x57;
            d[1] = crate::desc::IDN_GEOMETRY;
            d[0x0c] = s.max_lu_32 as u8;
            desc_reply(out, &d);
        }
        (upiu::QUERY_OP_READ_DESC, crate::desc::IDN_UNIT) => {
            match s.lus.iter().find(|l| l.lun == index) {
                Some(l) => {
                    let mut d = [0u8; 0x2d];
                    d[0] = 0x2d;
                    d[1] = crate::desc::IDN_UNIT;
                    d[2] = l.lun;
                    d[3] = l.enabled;
                    d[4] = l.boot_lun_id;
                    d[5] = l.write_protect;
                    d[8] = l.memory_type;
                    d[0x0a] = l.log2_bs;
                    d[0x0b..0x13].copy_from_slice(&l.blocks().to_be_bytes());
                    desc_reply(out, &d);
                }
                None => out[6] = 0xf8, // QUERY_RESULT_INVALID_INDEX
            }
        }
        (upiu::QUERY_OP_READ_FLAG, crate::desc::FLAG_DEVICE_INIT) => {
            let v = if s.device_init && s.device_init_polls > 0 {
                s.device_init_polls -= 1;
                1
            } else {
                s.device_init = false;
                0
            };
            out[20..24].copy_from_slice(&(v as u32).to_be_bytes());
        }
        (upiu::QUERY_OP_SET_FLAG, crate::desc::FLAG_DEVICE_INIT) => {
            s.device_init = true;
            out[20..24].copy_from_slice(&1u32.to_be_bytes());
        }
        _ => out[6] = 0xfe, // QUERY_RESULT_INVALID_OPCODE
    }
}

fn check_condition(out: &mut [u8; 512], key: u8, asc: u8, ascq: u8) -> u8 {
    out[7] = upiu::SCSI_CHECK_CONDITION;
    let mut sense = [0u8; 18];
    sense[0] = 0x70;
    sense[2] = key;
    sense[7] = 10;
    sense[12] = asc;
    sense[13] = ascq;
    out[10..12].copy_from_slice(&20u16.to_be_bytes());
    out[32..34].copy_from_slice(&18u16.to_be_bytes());
    out[34..52].copy_from_slice(&sense);
    0
}

fn scsi(s: &mut State, req: &[u8], dd: u32, segs: &[(u64, usize)], out: &mut [u8; 512]) -> u8 {
    let lun = req[2];
    let cdb: Vec<u8> = req[16..32].to_vec();
    s.n.scsi.push((lun, cdb[0]));
    if s.task_aborted > 0 {
        s.task_aborted -= 1;
        out[7] = upiu::SCSI_TASK_ABORTED;
        return 0;
    }
    let expected = u32::from_be_bytes(req[12..16].try_into().unwrap()) as usize;
    let have: usize = segs.iter().map(|x| x.1).sum();
    if have < expected {
        return upiu::OCS_MISMATCH_DATA_BUF_SIZE;
    }
    let Some(li) = s.lus.iter().position(|l| l.lun == lun && l.enabled != 0) else {
        return check_condition(out, 0x5, 0x25, 0); // LOGICAL UNIT NOT SUPPORTED
    };
    if s.lus[li].unit_attention
        && cdb[0] != crate::scsi::INQUIRY
        && cdb[0] != crate::scsi::REQUEST_SENSE
    {
        s.lus[li].unit_attention = false;
        return check_condition(out, 0x6, 0x29, 0); // POWER ON, RESET
    }
    let reply = |s: &mut State, d: &[u8]| {
        let mut left = d;
        for &(a, l) in segs {
            let n = l.min(left.len());
            wr(s, a, &left[..n]);
            left = &left[n..];
        }
    };
    let lu = s.lus[li].clone();
    let bs = lu.bs();
    match cdb[0] {
        crate::scsi::TEST_UNIT_READY | crate::scsi::SYNCHRONIZE_CACHE_10 => 0,
        crate::scsi::INQUIRY => {
            let mut d = [b' '; 36];
            d[0] = 0;
            d[1..8].fill(0);
            d[8..16].copy_from_slice(b"MODEL   ");
            d[16..32].copy_from_slice(b"UFS LU          ");
            d[32..36].copy_from_slice(b"0001");
            reply(s, &d[..expected.min(36)]);
            0
        }
        crate::scsi::READ_CAPACITY_10 => {
            let mut d = [0u8; 8];
            d[0..4].copy_from_slice(&((lu.blocks() - 1) as u32).to_be_bytes());
            d[4..8].copy_from_slice(&(bs as u32).to_be_bytes());
            reply(s, &d);
            0
        }
        crate::scsi::SERVICE_ACTION_IN_16 => {
            let mut d = [0u8; 32];
            d[0..8].copy_from_slice(&(lu.blocks() - 1).to_be_bytes());
            d[8..12].copy_from_slice(&(bs as u32).to_be_bytes());
            reply(s, &d[..expected.min(32)]);
            0
        }
        op @ (crate::scsi::READ_10
        | crate::scsi::WRITE_10
        | crate::scsi::READ_16
        | crate::scsi::WRITE_16) => {
            let (lba, n) = if op == crate::scsi::READ_10 || op == crate::scsi::WRITE_10 {
                (
                    u32::from_be_bytes(cdb[2..6].try_into().unwrap()) as u64,
                    u16::from_be_bytes([cdb[7], cdb[8]]) as u64,
                )
            } else {
                (
                    u64::from_be_bytes(cdb[2..10].try_into().unwrap()),
                    u32::from_be_bytes(cdb[10..14].try_into().unwrap()) as u64,
                )
            };
            if lba + n > lu.blocks() {
                return check_condition(out, 0x5, 0x21, 0); // LBA OUT OF RANGE
            }
            let range = lba as usize * bs..(lba + n) as usize * bs;
            let write = op == crate::scsi::WRITE_10 || op == crate::scsi::WRITE_16;
            if write {
                if dd != 1 {
                    return upiu::OCS_INVALID_CMD_TABLE_ATTR;
                }
                if lu.write_protect != 0 {
                    return check_condition(out, 0x7, 0x27, 0); // DATA PROTECT, WRITE PROTECTED
                }
                let mut data = Vec::new();
                for &(a, l) in segs {
                    data.extend_from_slice(&rd(s, a, l));
                }
                s.lus[li].data[range.clone()].copy_from_slice(&data[..range.len()]);
            } else {
                if dd != 2 {
                    return upiu::OCS_INVALID_CMD_TABLE_ATTR;
                }
                let d = lu.data[range].to_vec();
                reply(s, &d);
            }
            0
        }
        crate::scsi::MODE_SENSE_10 => {
            let Some((wce, rcd)) = lu.caching else {
                return check_condition(out, 0x5, 0x20, 0);
            };
            if dd != 2 {
                return upiu::OCS_INVALID_CMD_TABLE_ATTR;
            }
            let (pc, page) = (cdb[2] >> 6, cdb[2] & 0x3f);
            if page != crate::scsi::MODE_PAGE_CACHING && page != 0x3f {
                return check_condition(out, 0x5, 0x24, 0); // INVALID FIELD IN CDB
            }
            let mut d = vec![0u8; 8];
            d[3] = 0x10; // DPOFUA
            if lu.mode_block_descriptor {
                d[7] = 8;
                let mut bd = [0u8; 8];
                bd[0..4].copy_from_slice(&(lu.blocks().min(u32::MAX as u64) as u32).to_be_bytes());
                bd[5..8].copy_from_slice(&(bs as u32).to_be_bytes()[1..4]);
                d.extend_from_slice(&bd);
            }
            let mut page08 = [0u8; 20];
            page08[0] = crate::scsi::MODE_PAGE_CACHING;
            page08[1] = 18;
            page08[2] = match pc {
                1 => 0x04,
                _ => (wce as u8) << 2 | rcd as u8,
            };
            d.extend_from_slice(&page08);
            let mdl = (d.len() - 2) as u16;
            d[0..2].copy_from_slice(&mdl.to_be_bytes());
            let alloc = u16::from_be_bytes([cdb[7], cdb[8]]) as usize;
            let n = d.len().min(alloc).min(expected);
            reply(s, &d[..n]);
            // A device moves no more than it has: the rest is an underflow residual.
            if n < expected {
                s.residual = Some(((expected - n) as u32, false));
            }
            0
        }
        _ => check_condition(out, 0x5, 0x20, 0), // INVALID COMMAND OPERATION CODE
    }
}
