//! A model of the WCN3990 firmware, seen from its side of the rings, for `tests/driver.rs`.
//!
//! It is not a modem. It was written from reading Linux's host driver, by the author of the driver it tests, so it can
//! only catch disagreements with that reading: it decodes the rings and messages with its own literal offsets (no code
//! shared with the encoders, on purpose) and answers as the C expects to be answered. The layout numbers it uses for
//! rx descriptors are compared with the C compiler's in `tests/driver.rs`.
#![allow(dead_code)]

use std::collections::{BTreeMap, VecDeque};

use lucina_ath10k::ce::{DescMem, Regs};
use lucina_ath10k::driver::{Dma, DmaBuf, Platform};

// ---- the machine ---------------------------------------------------------------------------------------------------

#[derive(Default)]
pub struct FakeRegs {
    pub map: BTreeMap<usize, u32>,
    pub writes: u64,
    pub reads: u64,
}

impl Regs for FakeRegs {
    fn read32(&mut self, offset: usize) -> u32 {
        self.reads += 1;
        *self.map.get(&offset).unwrap_or(&0)
    }
    fn write32(&mut self, offset: usize, value: u32) {
        self.writes += 1;
        self.map.insert(offset, value);
    }
}

pub struct FakeMem {
    pub ram: Vec<u8>,
    pub barriers: u64,
}

impl DescMem for FakeMem {
    fn read32(&mut self, offset: usize) -> u32 {
        let b = &self.ram[offset..offset + 4];
        u32::from_le_bytes([b[0], b[1], b[2], b[3]])
    }
    fn write32(&mut self, offset: usize, value: u32) {
        self.ram[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    fn barrier(&mut self, _kind: lucina_ath10k::ce::Barrier) {
        self.barriers += 1;
    }
}

#[derive(Default)]
pub struct FakeDma {
    pub bufs: BTreeMap<u64, (u64, Vec<u8>)>,
    pub coherent: std::collections::BTreeSet<u64>,
    next_id: u64,
    next_paddr: u64,
    pub limit: Option<usize>,
    pub live_bytes: usize,
    pub peak_bytes: usize,
}

impl FakeDma {
    fn new() -> FakeDma {
        FakeDma {
            next_paddr: 0x1_0000_0000,
            ..FakeDma::default()
        }
    }
    /// The device's view: a physical address anywhere inside a live buffer.
    fn find(&mut self, paddr: u64) -> Option<(&mut Vec<u8>, usize)> {
        for (base, b) in self.bufs.values_mut() {
            if paddr >= *base && paddr < *base + b.len() as u64 {
                return Some((b, (paddr - *base) as usize));
            }
        }
        None
    }
    pub fn dev_read(&mut self, paddr: u64, n: usize) -> Vec<u8> {
        let (b, off) = self
            .find(paddr)
            .unwrap_or_else(|| panic!("device read of {paddr:#x} hits no buffer"));
        assert!(
            off + n <= b.len(),
            "device read past a buffer at {paddr:#x}"
        );
        b[off..off + n].to_vec()
    }
    pub fn dev_write(&mut self, paddr: u64, data: &[u8]) {
        let (b, off) = self
            .find(paddr)
            .unwrap_or_else(|| panic!("device write to {paddr:#x} hits no buffer"));
        assert!(
            off + data.len() <= b.len(),
            "device write past a buffer at {paddr:#x}"
        );
        b[off..off + data.len()].copy_from_slice(data);
    }
}

impl Dma for FakeDma {
    fn alloc(&mut self, len: usize) -> Option<DmaBuf> {
        if self.limit.is_some_and(|l| self.live_bytes + len > l) {
            return None;
        }
        let id = self.next_id;
        self.next_id += 1;
        let paddr = self.next_paddr;
        self.next_paddr += ((len as u64) + 0x1000) & !0xfff;
        // junk: the driver must not count on zeroed memory
        self.bufs.insert(id, (paddr, vec![0xa5; len]));
        self.live_bytes += len;
        self.peak_bytes = self.peak_bytes.max(self.live_bytes);
        Some(DmaBuf { id, paddr, len })
    }
    fn alloc_coherent(&mut self, len: usize) -> Option<DmaBuf> {
        let buf = self.alloc(len)?;
        self.coherent.insert(buf.id);
        Some(buf)
    }
    fn free(&mut self, b: DmaBuf) {
        let (_, v) = self.bufs.remove(&b.id).expect("double free");
        self.live_bytes -= v.len();
        self.coherent.remove(&b.id);
    }
    fn write(&mut self, b: &DmaBuf, off: usize, data: &[u8]) {
        let v = &mut self.bufs.get_mut(&b.id).expect("write to a freed buffer").1;
        v[off..off + data.len()].copy_from_slice(data);
    }
    fn read(&mut self, b: &DmaBuf, off: usize, out: &mut [u8]) {
        let v = &self.bufs.get(&b.id).expect("read of a freed buffer").1;
        out.copy_from_slice(&v[off..off + out.len()]);
    }
}

pub const ARENA_BUS: u64 = 0x8000_0000;

pub struct RamPlatform {
    pub regs: FakeRegs,
    pub mem: FakeMem,
    pub dma: FakeDma,
}

impl RamPlatform {
    pub fn new(arena: usize) -> RamPlatform {
        RamPlatform {
            regs: FakeRegs::default(),
            mem: FakeMem {
                ram: vec![0xee; arena],
                barriers: 0,
            },
            dma: FakeDma::new(),
        }
    }
}

impl Platform for RamPlatform {
    type Regs = FakeRegs;
    type Mem = FakeMem;
    type Dma = FakeDma;
    fn parts(&mut self) -> (&mut FakeRegs, &mut FakeMem, &mut FakeDma) {
        (&mut self.regs, &mut self.mem, &mut self.dma)
    }
    fn arena_bus_addr(&self) -> u64 {
        ARENA_BUS
    }
    fn arena_len(&self) -> usize {
        self.mem.ram.len()
    }
}

// ---- the firmware's literal view -------------------------------------------------------------------------------------

const CE0: usize = 0x24_0000;
const STRIDE: usize = 0x1000;
const SR_LO: usize = 0x00;
const SR_HI: usize = 0x04;
const SR_SIZE: usize = 0x08;
const DR_LO: usize = 0x0c;
const DR_HI: usize = 0x10;
const DR_SIZE: usize = 0x14;
const DST_WR: usize = 0x40;
const CURRENT_SRRI: usize = 0x44;
const RRI_LO: usize = 0x24_c004;
const RRI_HI: usize = 0x24_c008;
const SHADOW0: usize = 0x3_2000;

/// Pipes as `target_service_to_ce_map_wlan` routes the services.
const PIPE_CTRL_DL: usize = 2;
const PIPE_WMI_DL: usize = 2;
const PIPE_HTT_DL: usize = 1;

/// One message the host sent, gather items joined.
#[derive(Clone, Debug)]
pub struct HostMsg {
    pub ce: usize,
    pub bytes: Vec<u8>,
    pub transfer_id: u16,
    /// The items of a gather, each `(addr, len)`.
    pub items: Vec<(u64, usize)>,
}

/// A WMI command, decoded as far as the model needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WmiCmd {
    pub id: u32,
    pub tlvs: Vec<u8>,
}

impl WmiCmd {
    /// `(tag, value)` of the top level.
    pub fn tlv(&self) -> Vec<(u16, Vec<u8>)> {
        walk(&self.tlvs)
    }
    /// A little-endian word of the first TLV's value.
    pub fn w(&self, i: usize) -> u32 {
        let t = self.tlv();
        word(&t[0].1, i)
    }
}

pub fn walk(mut b: &[u8]) -> Vec<(u16, Vec<u8>)> {
    let mut out = Vec::new();
    while b.len() >= 4 {
        let len = usize::from(u16::from_le_bytes([b[0], b[1]]));
        let tag = u16::from_le_bytes([b[2], b[3]]);
        assert!(4 + len <= b.len(), "command TLV runs past the message");
        out.push((tag, b[4..4 + len].to_vec()));
        b = &b[4 + len..];
    }
    assert!(b.is_empty(), "stray bytes after TLVs");
    out
}

pub fn word(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[4 * i], b[4 * i + 1], b[4 * i + 2], b[4 * i + 3]])
}

fn tlv(tag: u16, v: &[u8]) -> Vec<u8> {
    let mut o = Vec::with_capacity(4 + v.len());
    o.extend_from_slice(&(v.len() as u16).to_le_bytes());
    o.extend_from_slice(&tag.to_le_bytes());
    o.extend_from_slice(v);
    o
}

fn le32s(w: &[u32]) -> Vec<u8> {
    w.iter().flat_map(|x| x.to_le_bytes()).collect()
}

/// Tags and ids the model speaks, as literals from `wmi-tlv.h` (the test compares the crate with the fixture; a typo
/// here shows up as a parse failure in the driver).
pub mod id {
    pub const SERVICE_READY_EV: u32 = 1;
    pub const READY_EV: u32 = 2;
    pub const SERVICE_AVAILABLE_EV: u32 = 3;
    pub const SCAN_EV: u32 = 0x3001;
    pub const VDEV_START_RESP_EV: u32 = 0x5001;
    pub const VDEV_STOPPED_EV: u32 = 0x5002;
    pub const PEER_KICKOUT_EV: u32 = 0x6001;
    pub const PEER_DELETE_RESP_EV: u32 = 0x6007;
    pub const MGMT_RX_EV: u32 = 0x7001;
    pub const MGMT_TX_COMPL_EV: u32 = 0x7006;
    pub const MGMT_TX_BUNDLE_EV: u32 = 0x7007;
    pub const ROAM_EV: u32 = 0xb001;

    pub const INIT: u32 = 1;
    pub const START_SCAN: u32 = 0x3001;
    pub const STOP_SCAN: u32 = 0x3002;
    pub const SCAN_CHAN_LIST: u32 = 0x3003;
    pub const REGDOMAIN: u32 = 0x4001;
    pub const PDEV_SET_PARAM: u32 = 0x4003;
    pub const VDEV_CREATE: u32 = 0x5001;
    pub const VDEV_START: u32 = 0x5003;
    pub const VDEV_UP: u32 = 0x5005;
    pub const VDEV_STOP: u32 = 0x5006;
    pub const VDEV_DOWN: u32 = 0x5007;
    pub const VDEV_SET_PARAM: u32 = 0x5008;
    pub const INSTALL_KEY: u32 = 0x5009;
    pub const PEER_CREATE: u32 = 0x6001;
    pub const PEER_DELETE: u32 = 0x6002;
    pub const PEER_SET_PARAM: u32 = 0x6004;
    pub const PEER_ASSOC: u32 = 0x6005;
    pub const MGMT_TX_SEND: u32 = 0x7008;
    pub const STA_PS_MODE: u32 = 0x9001;
    pub const STA_PS_PARAM: u32 = 0x9002;
    pub const STA_KEEPALIVE: u32 = 0x1c005;
}

#[derive(Clone, Debug)]
pub struct ModelCfg {
    /// Main service numbers to advertise (below 128).
    pub services: Vec<u32>,
    /// Extended numbers (128 and up), in a service available event.
    pub ext_services: Vec<u32>,
    pub htt_major: u8,
    pub mac: [u8; 6],
    /// `(req_id, unit_size, num_unit_info, num_units)`.
    pub mem_reqs: Vec<(u32, u32, u32, u32)>,
    pub eeprom_rd: u32,
    pub bundle_mgmt: bool,
    pub num_rf_chains: u32,
    pub vht_cap: u32,
    /// Answer to a vdev start.
    pub vdev_start_status: u32,
    pub silent: Silent,
}

/// What the model refuses to answer, for the timeouts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Silent {
    pub no_ready: bool,
    pub no_service_ready: bool,
    pub no_credit: bool,
    pub no_sec_ind: bool,
    pub no_peer_map: bool,
    pub no_scan_events: bool,
}

impl Default for ModelCfg {
    fn default() -> Self {
        ModelCfg {
            services: vec![0, 29, 37, 76],
            ext_services: vec![155, 174, 179],
            htt_major: 3,
            mac: [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee],
            mem_reqs: vec![(0x1000, 256, 2, 0), (0x1001, 100, 0, 7)],
            eeprom_rd: 0x37, // ETSI1_WORLD
            bundle_mgmt: true,
            num_rf_chains: 2,
            vht_cap: (1 << 12) | (1 << 11),
            vdev_start_status: 0,
            silent: Silent::default(),
        }
    }
}

/// What an rx buffer should hold. Offsets are `struct htt_rx_desc_v2`'s; `tests/driver.rs` compares them with the fixture.
#[derive(Clone, Debug)]
pub struct RxSpec {
    pub payload: Vec<u8>,
    pub attention: u32,
    pub first_msdu: bool,
    pub last_msdu: bool,
    pub enctype: u32,
    pub decap: u32,
    pub tid: u32,
    pub hdr: Vec<u8>,
    pub rssi_comb: u8,
    pub ppdu_info1: u32,
    pub ppdu_info2: u32,
    pub ppdu_info3: u32,
    pub chained: Vec<Vec<u8>>,
    /// `msdu_len` override in the descriptor (for lies).
    pub msdu_len: Option<u32>,
}

pub const ATT_FIRST_MPDU: u32 = 1;
pub const ATT_LAST_MPDU: u32 = 2;
pub const ATT_MCAST: u32 = 4;
pub const ATT_PEER_INVALID: u32 = 8;
pub const ATT_MGMT: u32 = 1 << 8;
pub const ATT_FCS_ERR: u32 = 1 << 30;
pub const ATT_DECRYPT_ERR: u32 = 1 << 29;
pub const ATT_MIC_ERR: u32 = 1 << 28;
pub const ATT_DONE: u32 = 1 << 31;

impl Default for RxSpec {
    fn default() -> Self {
        RxSpec {
            payload: vec![],
            attention: ATT_DONE | ATT_FIRST_MPDU | ATT_LAST_MPDU,
            first_msdu: true,
            last_msdu: true,
            enctype: 7,
            decap: 1,
            tid: 0,
            hdr: vec![],
            rssi_comb: 40,
            ppdu_info1: 4 << 24,
            ppdu_info2: 0,
            ppdu_info3: 0,
            chained: vec![],
            msdu_len: None,
        }
    }
}

/// The firmware.
pub struct Model {
    pub cfg: ModelCfg,
    /// How far the device has read each source ring.
    src_read: [u32; 12],
    /// How far the device has filled each destination ring.
    dst_fill: [u32; 12],
    /// Messages to deliver, by pipe.
    pending: VecDeque<(usize, Vec<u8>)>,
    pub sent_ready: bool,
    next_eid: u8,
    pub wmi_eid: u8,
    pub htt_eid: u8,
    /// Everything received, in order.
    pub wmi_log: Vec<WmiCmd>,
    pub htt_log: Vec<Vec<u8>>,
    pub frames_tx: Vec<TxFrame>,
    pub mgmt_tx: Vec<(u32, Vec<u8>)>,
    /// HTT rx ring as the host set it up.
    pub rx_ring: Option<(u64, u32, u16)>,
    /// The firmware's index into it.
    rx_ring_fw: u32,
    pub frag_bank: Option<(u64, u16)>,
    pub setup_done: bool,
    pub connect_order: Vec<u16>,
    pub auto_tx_complete: bool,
    pub auto_mgmt_complete: bool,
    pub peer_id: u16,
    pub credits_given: u64,
}

/// A data frame the host queued, as the model fetched it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TxFrame {
    pub id: u16,
    pub flags0: u8,
    pub flags1: u16,
    pub frame: Vec<u8>,
    pub prefetch: Vec<u8>,
    pub eid: u8,
    pub htc_len: u16,
}

impl Model {
    pub fn new(cfg: ModelCfg) -> Model {
        Model {
            cfg,
            src_read: [0; 12],
            dst_fill: [0; 12],
            pending: VecDeque::new(),
            sent_ready: false,
            next_eid: 1,
            wmi_eid: 0,
            htt_eid: 0,
            wmi_log: Vec::new(),
            htt_log: Vec::new(),
            frames_tx: Vec::new(),
            mgmt_tx: Vec::new(),
            rx_ring: None,
            rx_ring_fw: 0,
            frag_bank: None,
            setup_done: false,
            connect_order: Vec::new(),
            auto_tx_complete: true,
            auto_mgmt_complete: true,
            peer_id: 0x35,
            credits_given: 0,
        }
    }

    /// The commands with id `id`.
    pub fn cmds(&self, id: u32) -> Vec<&WmiCmd> {
        self.wmi_log.iter().filter(|c| c.id == id).collect()
    }

    fn htc_frame(eid: u8, flags: u8, trailer_len: u8, payload: &[u8]) -> Vec<u8> {
        let mut o = vec![eid, flags];
        o.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        o.extend_from_slice(&[trailer_len, 0, 0, 0]);
        o.extend_from_slice(payload);
        o
    }

    pub fn queue_wmi_event(&mut self, id: u32, tlvs: &[u8]) {
        let mut p = id.to_le_bytes().to_vec();
        p.extend_from_slice(tlvs);
        self.pending
            .push_back((PIPE_WMI_DL, Self::htc_frame(self.wmi_eid, 0, 0, &p)));
    }

    pub fn queue_htt(&mut self, msg: &[u8]) {
        self.pending
            .push_back((PIPE_HTT_DL, Self::htc_frame(self.htt_eid, 0, 0, msg)));
    }

    /// Raw bytes on a pipe, for hostile tests.
    pub fn queue_raw(&mut self, pipe: usize, bytes: Vec<u8>) {
        self.pending.push_back((pipe, bytes));
    }

    /// CE5's form: an HTC header nobody parses.
    pub fn queue_htt_ce5(&mut self, msg: &[u8]) {
        self.pending
            .push_back((5, Self::htc_frame(0xff, 0xff, 0xff, msg)));
    }

    fn give_credit(&mut self) {
        if self.cfg.silent.no_credit {
            return;
        }
        // a trailer-only frame on the WMI endpoint: record CREDITS(1), length 4, eid, credits
        let rec = [1u8, 4, 0, 0, self.wmi_eid, 1, 0, 0];
        self.credits_given += 1;
        self.pending
            .push_back((PIPE_WMI_DL, Self::htc_frame(self.wmi_eid, 2, 8, &rec)));
    }

    /// One firmware pass: boot messages, everything the host sent, then deliveries.
    pub fn step(&mut self, p: &mut RamPlatform) {
        if !self.sent_ready && !self.cfg.silent.no_ready && self.dest_posted(p, PIPE_CTRL_DL) > 0 {
            // READY_EXT: id 1, credit count, credit size 2 KiB, max endpoints, version, bundle, alt size
            let mut r = 1u16.to_le_bytes().to_vec();
            r.extend_from_slice(&17u16.to_le_bytes());
            r.extend_from_slice(&1792u16.to_le_bytes());
            r.extend_from_slice(&[9, 0, 1, 0, 0, 0]);
            self.pending
                .push_back((PIPE_CTRL_DL, Self::htc_frame(0, 0, 0, &r)));
            self.sent_ready = true;
        }
        for ce in [0usize, 3, 4] {
            for m in self.take_source(p, ce) {
                self.on_host(p, m);
            }
        }
        self.deliver(p);
    }

    /// Entries the host has posted on a destination ring and the device has not used.
    fn dest_posted(&self, p: &mut RamPlatform, ce: usize) -> u32 {
        let ctrl = CE0 + ce * STRIDE;
        let size = p.regs.read32(ctrl + DR_SIZE);
        if size == 0 {
            return 0;
        }
        p.regs.read32(ctrl + DST_WR).wrapping_sub(self.dst_fill[ce]) & (size - 1)
    }

    fn arena_off(bus: u64) -> usize {
        (bus - ARENA_BUS) as usize
    }

    fn take_source(&mut self, p: &mut RamPlatform, ce: usize) -> Vec<HostMsg> {
        let ctrl = CE0 + ce * STRIDE;
        let size = p.regs.read32(ctrl + SR_SIZE);
        if size == 0 {
            return Vec::new();
        }
        let base =
            u64::from(p.regs.read32(ctrl + SR_LO)) | (u64::from(p.regs.read32(ctrl + SR_HI)) << 32);
        let shadow = match ce {
            0 | 3 | 4 | 5 | 7 => SHADOW0 + 4 * ce,
            _ => unreachable!(),
        };
        let wr = p.regs.read32(shadow);
        let mut out = Vec::new();
        let mut items = Vec::new();
        let mut bytes = Vec::new();
        while self.src_read[ce] != wr {
            let i = self.src_read[ce] & (size - 1);
            let off = Self::arena_off(base) + i as usize * 16;
            let ram = &p.mem.ram[off..off + 16];
            let lo = u32::from_le_bytes([ram[0], ram[1], ram[2], ram[3]]);
            let hi = u32::from_le_bytes([ram[4], ram[5], ram[6], ram[7]]);
            let n = u16::from_le_bytes([ram[8], ram[9]]);
            let fl = u16::from_le_bytes([ram[10], ram[11]]);
            let paddr = u64::from(lo) | (u64::from(hi & 0x1f) << 32);
            let gather = hi & (1 << 31) != 0;
            assert_eq!(gather, fl & 1 != 0, "the two gather bits disagree");
            let id = (fl & 0xfff0) >> 4;
            bytes.extend(p.dma.dev_read(paddr, usize::from(n)));
            items.push((paddr, usize::from(n)));
            self.src_read[ce] = (self.src_read[ce] + 1) & (size - 1);
            if !gather {
                out.push(HostMsg {
                    ce,
                    bytes: std::mem::take(&mut bytes),
                    transfer_id: id,
                    items: std::mem::take(&mut items),
                });
            }
        }
        // read indices: register for interrupt-driven engines, RRI memory for the polled one
        let rd = self.src_read[ce];
        p.regs.map.insert(ctrl + CURRENT_SRRI, rd);
        let rri = u64::from(p.regs.read32(RRI_LO)) | (u64::from(p.regs.read32(RRI_HI)) << 32);
        if rri != 0 {
            let off = Self::arena_off(rri) + 4 * ce;
            let old = u32::from_le_bytes(p.mem.ram[off..off + 4].try_into().unwrap());
            let v = (old & 0xffff_0000) | (rd & 0xffff);
            p.mem.ram[off..off + 4].copy_from_slice(&v.to_le_bytes());
        }
        out
    }

    /// Copy waiting messages into posted buffers.
    pub fn deliver(&mut self, p: &mut RamPlatform) {
        let mut later = VecDeque::new();
        while let Some((ce, m)) = self.pending.pop_front() {
            if !self.put_dest(p, ce, &m) {
                later.push_back((ce, m));
            }
        }
        self.pending = later;
    }

    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    fn put_dest(&mut self, p: &mut RamPlatform, ce: usize, m: &[u8]) -> bool {
        if self.dest_posted(p, ce) == 0 {
            return false;
        }
        let ctrl = CE0 + ce * STRIDE;
        let size = p.regs.read32(ctrl + DR_SIZE);
        let base =
            u64::from(p.regs.read32(ctrl + DR_LO)) | (u64::from(p.regs.read32(ctrl + DR_HI)) << 32);
        let i = self.dst_fill[ce] & (size - 1);
        let off = Self::arena_off(base) + i as usize * 16;
        let ram = &p.mem.ram[off..off + 16];
        let lo = u32::from_le_bytes([ram[0], ram[1], ram[2], ram[3]]);
        let hi = u32::from_le_bytes([ram[4], ram[5], ram[6], ram[7]]);
        let paddr = u64::from(lo) | (u64::from(hi & 0x1f) << 32);
        p.dma.dev_write(paddr, m);
        p.mem.ram[off + 8..off + 10].copy_from_slice(&(m.len() as u16).to_le_bytes());
        self.dst_fill[ce] = (self.dst_fill[ce] + 1) & (size - 1);
        true
    }

    fn on_host(&mut self, p: &mut RamPlatform, m: HostMsg) {
        let eid = m.bytes[0];
        let flags = m.bytes[1];
        let len = usize::from(u16::from_le_bytes([m.bytes[2], m.bytes[3]]));
        let body = &m.bytes[8..];
        if m.ce == 4 && !m.items.is_empty() && m.items.len() == 2 {
            self.on_tx_frm(p, &m);
            return;
        }
        assert_eq!(len, body.len(), "HTC length");
        assert_eq!(u16::from(eid), m.transfer_id, "transfer id is the endpoint");
        if eid == 0 {
            assert_eq!(m.ce, 0);
            self.on_control(body);
        } else if eid == self.wmi_eid {
            assert_eq!(m.ce, 3);
            assert_eq!(flags & 1, 1, "WMI asks for credit updates");
            self.on_wmi(p, body);
        } else if eid == self.htt_eid {
            assert_eq!(m.ce, 4);
            self.on_htt(p, body);
        } else {
            panic!("message for endpoint {eid}");
        }
    }

    fn on_control(&mut self, b: &[u8]) {
        let msg = u16::from_le_bytes([b[0], b[1]]);
        match msg {
            2 => {
                let svc = u16::from_le_bytes([b[2], b[3]]);
                let conn_flags = u16::from_le_bytes([b[4], b[5]]);
                self.connect_order.push(svc);
                let eid = self.next_eid;
                self.next_eid += 1;
                match svc {
                    0x0300 => {
                        assert_eq!(conn_flags & 8, 8, "HTT disables credit flow");
                        self.htt_eid = eid;
                    }
                    0x0100 => {
                        assert_eq!(conn_flags >> 8, 1, "one credit for WMI on WCN3990");
                        self.wmi_eid = eid;
                    }
                    other => panic!("connect to service {other:#x}"),
                }
                let mut r = 3u16.to_le_bytes().to_vec();
                r.extend_from_slice(&svc.to_le_bytes());
                r.extend_from_slice(&[0, eid]);
                r.extend_from_slice(&1792u16.to_le_bytes());
                r.extend_from_slice(&[0, 0]);
                self.pending
                    .push_back((PIPE_CTRL_DL, Self::htc_frame(0, 0, 0, &r)));
            }
            5 => {
                assert_eq!(b.len(), 12);
                self.setup_done = true;
                if !self.cfg.silent.no_service_ready {
                    self.send_service_ready();
                }
            }
            other => panic!("control message {other}"),
        }
    }

    fn send_service_ready(&mut self) {
        let mut ev = vec![0u32; 26];
        ev[0] = 0x1234; // fw_build_vers
        ev[1] = 0x0100_0000; // abi
        ev[2] = 53;
        ev[3] = 0x5f41_4351;
        ev[4] = 0x0000_4c4d;
        ev[7] = 3; // phy_capability
        ev[9] = self.cfg.num_rf_chains;
        ev[10] = 0x1ef; // ht_cap_info
        ev[11] = self.cfg.vht_cap;
        ev[12] = 0xfffa;
        ev[13] = 0;
        ev[14] = 30;
        ev[18] = self.cfg.mem_reqs.len() as u32;
        let reg = [self.cfg.eeprom_rd, 0, 0, 0, 0, 2312, 2732, 4915, 6100];
        // the service map: one word for every four services, as the target does it
        let mut map = vec![0u32; 32];
        for s in &self.cfg.services {
            map[(*s as usize) / 4] |= 1 << (s % 4);
        }
        let mut reqs = Vec::new();
        for r in &self.cfg.mem_reqs {
            reqs.extend(tlv(34, &le32s(&[r.0, r.1, r.2, r.3])));
        }
        let mut b = tlv(32, &le32s(&ev));
        b.extend(tlv(33, &le32s(&reg)));
        b.extend(tlv(16, &le32s(&map)));
        b.extend(tlv(18, &reqs));
        b.extend(tlv(16, &le32s(&[1, 2])));
        self.queue_wmi_event(id::SERVICE_READY_EV, &b);
        // the extended map
        let mut ext = vec![0u32; 5];
        ext[0] = 16;
        for s in &self.cfg.ext_services {
            let n = (*s - 128) as usize;
            ext[1 + n / 32] |= 1 << (n % 32);
        }
        self.queue_wmi_event(id::SERVICE_AVAILABLE_EV, &tlv(559, &le32s(&ext)));
    }

    fn on_wmi(&mut self, p: &mut RamPlatform, b: &[u8]) {
        let id = u32::from_le_bytes([b[0], b[1], b[2], b[3]]) & 0xff_ffff;
        assert_eq!(b[3], 0, "plt_priv");
        let c = WmiCmd {
            id,
            tlvs: b[4..].to_vec(),
        };
        self.wmi_log.push(c.clone());
        self.give_credit();
        match id {
            id::INIT => {
                let t = c.tlv();
                assert_eq!(t[0].0, 74);
                assert_eq!(word(&t[0].1, 0), 0x0100_0000);
                let mut ev = vec![0u8; 36];
                ev[0..4].copy_from_slice(&0x0100_0000u32.to_le_bytes());
                ev[4..8].copy_from_slice(&53u32.to_le_bytes());
                ev[24..30].copy_from_slice(&self.cfg.mac);
                self.queue_wmi_event(id::READY_EV, &tlv(35, &ev));
            }
            id::START_SCAN => {
                if !self.cfg.silent.no_scan_events {
                    self.scan_event(1, 0, 0);
                }
            }
            id::STOP_SCAN => self.scan_event(2, 1, 0),
            id::VDEV_START => {
                let vdev = c.w(0);
                let ev = le32s(&[vdev, 0, 0, self.cfg.vdev_start_status]);
                self.queue_wmi_event(id::VDEV_START_RESP_EV, &tlv(40, &ev));
            }
            id::PEER_CREATE => {
                if !self.cfg.silent.no_peer_map {
                    let t = c.tlv();
                    let addr: Vec<u8> = t[0].1[4..10].to_vec();
                    let mut m = vec![0x03, word(&t[0].1, 0) as u8];
                    m.extend_from_slice(&self.peer_id.to_le_bytes());
                    m.extend_from_slice(&addr);
                    m.extend_from_slice(&[0, 0]);
                    self.queue_htt(&m);
                }
            }
            id::PEER_DELETE => {
                let mut m = vec![0x04, 0];
                m.extend_from_slice(&self.peer_id.to_le_bytes());
                self.queue_htt(&m);
            }
            id::VDEV_STOP => {
                self.queue_wmi_event(id::VDEV_STOPPED_EV, &tlv(41, &le32s(&[c.w(0)])));
            }
            id::INSTALL_KEY => {
                if !self.cfg.silent.no_sec_ind {
                    let unicast = word(&c.tlv()[0].1, 4) == 0;
                    let mut m = vec![0x0b, 6 | if unicast { 0x80 } else { 0 }];
                    m.extend_from_slice(&self.peer_id.to_le_bytes());
                    m.resize(28, 0);
                    self.queue_htt(&m);
                }
            }
            id::MGMT_TX_SEND => self.on_mgmt_send(p, &c),
            _ => {}
        }
    }

    pub fn scan_event(&mut self, ty: u32, reason: u32, freq: u32) {
        let ev = le32s(&[ty, reason, freq, 0xa001, 0xa000, 0]);
        self.queue_wmi_event(id::SCAN_EV, &tlv(36, &ev));
    }

    fn on_mgmt_send(&mut self, p: &mut RamPlatform, c: &WmiCmd) {
        let t = c.tlv();
        let h = &t[0].1;
        assert_eq!(t[0].0, 422);
        let desc_id = word(h, 1);
        let paddr = u64::from(word(h, 3)) | (u64::from(word(h, 4)) << 32);
        let frame_len = word(h, 5) as usize;
        let buf_len = word(h, 6) as usize;
        assert_eq!(t[1].1.len(), buf_len);
        let frame = p.dma.dev_read(paddr, frame_len);
        let inline = frame_len.min(64);
        assert_eq!(&t[1].1[..inline], &frame[..inline], "inline copy");
        self.mgmt_tx.push((desc_id, frame));
        if self.auto_mgmt_complete {
            self.complete_mgmt(desc_id, 0);
        }
    }

    pub fn complete_mgmt(&mut self, desc_id: u32, status: u32) {
        if self.cfg.bundle_mgmt {
            let mut b = tlv(552, &le32s(&[1]));
            b.extend(tlv(16, &le32s(&[desc_id])));
            b.extend(tlv(16, &le32s(&[status])));
            b.extend(tlv(16, &le32s(&[9])));
            b.extend(tlv(16, &le32s(&[40])));
            self.queue_wmi_event(id::MGMT_TX_BUNDLE_EV, &b);
        } else {
            let ev = le32s(&[desc_id, status, 0, 3, 44]);
            self.queue_wmi_event(id::MGMT_TX_COMPL_EV, &tlv(423, &ev));
        }
    }

    fn on_htt(&mut self, p: &mut RamPlatform, b: &[u8]) {
        self.htt_log.push(b.to_vec());
        match b[0] {
            0 => {
                let major = self.cfg.htt_major;
                self.queue_htt(&[0, 4, major, 0]);
            }
            6 => {
                assert_eq!(b.len(), 64);
                assert_eq!(b[2], 1);
                assert_eq!(b[3], 72, "descriptor size");
                let base = u64::from_le_bytes(b[4..12].try_into().unwrap());
                let max = u16::from_le_bytes([b[38], b[39]]);
                self.frag_bank = Some((base, max));
            }
            2 => {
                assert_eq!(b.len(), 48);
                assert_eq!(b[1], 1);
                let shadow = u64::from_le_bytes(b[4..12].try_into().unwrap());
                let base = u64::from_le_bytes(b[12..20].try_into().unwrap());
                let len = u16::from_le_bytes([b[20], b[21]]);
                assert_eq!(u16::from_le_bytes([b[22], b[23]]), 2048, "buffer size");
                assert_eq!(u16::from_le_bytes([b[24], b[25]]), 0xffff, "flags");
                let init = u16::from_le_bytes([b[26], b[27]]);
                let idx = p.dma.dev_read(shadow, 4);
                assert_eq!(u32::from(init), word(&idx, 0), "the index at setup");
                self.rx_ring = Some((base, shadow as u32, len));
            }
            5 => {
                assert_eq!(&b[1..4], &[64, 3, 0]);
            }
            other => panic!("HTT message {other}"),
        }
    }

    fn on_tx_frm(&mut self, p: &mut RamPlatform, m: &HostMsg) {
        let hdr = &m.bytes[..28];
        let eid = hdr[0];
        assert_eq!(hdr[1], 0, "no HTC flags on the data path");
        assert_eq!(hdr[8], 1, "TX_FRM");
        assert_eq!(m.transfer_id, 0);
        let htc_len = u16::from_le_bytes([hdr[2], hdr[3]]);
        let flags0 = hdr[9];
        let flags1 = u16::from_le_bytes([hdr[10], hdr[11]]);
        let len = u16::from_le_bytes([hdr[12], hdr[13]]);
        let id = u16::from_le_bytes([hdr[14], hdr[15]]);
        let frags = u64::from_le_bytes(hdr[16..24].try_into().unwrap());
        assert_eq!(u16::from_le_bytes([hdr[24], hdr[25]]), 0xffff, "peerid");
        let prefetch = m.bytes[28..].to_vec();
        assert_eq!(usize::from(htc_len), 20 + prefetch.len(), "HTC length");
        assert_eq!(prefetch.len() % 4, 0);
        assert!(prefetch.len() <= 52);
        let (paddr, flen) = if flags0 >> 5 == 3 {
            // management: the descriptor points straight at the frame
            (frags, usize::from(len))
        } else {
            let (base, max) = self.frag_bank.expect("no frag bank");
            assert!(id <= max);
            assert_eq!(frags, base + 72 * u64::from(id));
            // the fragment list starts at +24: lo, hi16, len16
            let e = p.dma.dev_read(frags, 72);
            let lo = u32::from_le_bytes(e[24..28].try_into().unwrap());
            let hi = u16::from_le_bytes([e[28], e[29]]);
            let flen = u16::from_le_bytes([e[30], e[31]]);
            assert_eq!(flen, len);
            assert!(e[32..].iter().all(|x| *x == 0), "other fragments are clear");
            (u64::from(lo) | (u64::from(hi) << 32), usize::from(flen))
        };
        let frame = p.dma.dev_read(paddr, flen);
        assert_eq!(
            &prefetch[..flen.min(prefetch.len())],
            &frame[..flen.min(prefetch.len())],
            "prefetch"
        );
        assert_eq!(m.items[1].0, paddr, "the second gather item is the frame");
        self.frames_tx.push(TxFrame {
            id,
            flags0,
            flags1,
            frame,
            prefetch,
            eid,
            htc_len,
        });
        if self.auto_tx_complete {
            self.tx_complete(&[id], 0);
        }
    }

    /// `HTT_T2H_MSG_TYPE_TX_COMPL_IND` with `status` in the low bits.
    pub fn tx_complete(&mut self, ids: &[u16], status: u8) {
        let mut m = vec![0x07, status, ids.len() as u8, 0];
        for i in ids {
            m.extend_from_slice(&i.to_le_bytes());
        }
        if !ids.len().is_multiple_of(2) {
            m.extend_from_slice(&0xffffu16.to_le_bytes());
        }
        self.queue_htt(&m);
    }

    /// Write an rx descriptor and payload into the next ring buffers, as the MAC would, without telling the host yet.
    /// Returns the addresses used.
    pub fn fill_rx(&mut self, p: &mut RamPlatform, s: &RxSpec) -> Vec<u64> {
        let (paddr, chained) = self.next_buffers(p, 1 + s.chained.len());
        let mut d = vec![0u8; 360];
        d[4..8].copy_from_slice(&s.attention.to_le_bytes());
        d[10] = s.chained.len() as u8; // ring2_more_count
        d[16..20].copy_from_slice(&(s.enctype << 28).to_le_bytes());
        d[24..28].copy_from_slice(&(s.tid << 28).to_le_bytes());
        let total: usize = s.payload.len() + s.chained.iter().map(Vec::len).sum::<usize>();
        let mlen = s.msdu_len.unwrap_or(total as u32);
        d[28..32].copy_from_slice(&mlen.to_le_bytes());
        d[36..40].copy_from_slice(&(s.decap << 8).to_le_bytes());
        let mut end = 0u32;
        if s.first_msdu {
            end |= 1 << 14;
        }
        if s.last_msdu {
            end |= 1 << 15;
        }
        d[64..68].copy_from_slice(&end.to_le_bytes());
        d[124] = s.rssi_comb;
        d[128..132].copy_from_slice(&s.ppdu_info1.to_le_bytes());
        d[132..136].copy_from_slice(&s.ppdu_info2.to_le_bytes());
        d[136..140].copy_from_slice(&s.ppdu_info3.to_le_bytes());
        d[296..296 + s.hdr.len()].copy_from_slice(&s.hdr);
        p.dma.dev_write(paddr, &d);
        p.dma.dev_write(paddr + 360, &s.payload);
        for (a, c) in chained.iter().zip(&s.chained) {
            p.dma.dev_write(*a, c);
        }
        let mut all = vec![paddr];
        all.extend(chained);
        all
    }

    fn next_buffers(&mut self, p: &mut RamPlatform, n: usize) -> (u64, Vec<u64>) {
        let (base, _, len) = self.rx_ring.expect("no rx ring");
        let mut v = Vec::new();
        for _ in 0..n {
            let i = u64::from(self.rx_ring_fw & (u32::from(len) - 1));
            let e = p.dma.dev_read(base + 8 * i, 8);
            let a = u64::from_le_bytes(e[..8].try_into().unwrap());
            assert_ne!(a, 0, "ring slot {i} is empty");
            v.push(a);
            self.rx_ring_fw = (self.rx_ring_fw + 1) & (u32::from(len) - 1);
        }
        let head = v.remove(0);
        (head, v)
    }

    /// `HTT_T2H_MSG_TYPE_RX_IND` for `counts` MPDUs (one range each).
    pub fn rx_ind(&mut self, counts: &[u8], status: u8) {
        let mut m = vec![0u8; 48];
        m[0] = 0x01;
        m[2..4].copy_from_slice(&self.peer_id.to_le_bytes());
        m[7] = counts.len() as u8;
        for c in counts {
            m.extend_from_slice(&[*c, status, 0, 0]);
        }
        self.queue_htt(&m);
    }

    /// `HTT_T2H_MSG_TYPE_RX_IN_ORD_PADDR_IND` with the given `(paddr, msdu_len)`.
    pub fn rx_in_ord(&mut self, list: &[(u64, u16)], offload: bool) {
        let mut m = vec![0x12, 3 | if offload { 0x20 } else { 0 }];
        m.extend_from_slice(&self.peer_id.to_le_bytes());
        m.extend_from_slice(&[0, 0]);
        m.extend_from_slice(&(list.len() as u16).to_le_bytes());
        for (a, l) in list {
            m.extend_from_slice(&a.to_le_bytes());
            m.extend_from_slice(&l.to_le_bytes());
            m.extend_from_slice(&[0, 0]);
        }
        self.queue_htt(&m);
    }

    /// A management frame over WMI.
    pub fn mgmt_rx(&mut self, channel: u32, snr: u32, status: u32, frame: &[u8]) {
        let hdr = le32s(&[
            channel,
            snr,
            1000,
            1,
            frame.len() as u32,
            status,
            snr,
            128,
            0,
            0,
        ]);
        let mut b = tlv(44, &hdr);
        let mut f = frame.to_vec();
        while !f.len().is_multiple_of(4) {
            f.push(0);
        }
        b.extend(tlv(17, &f));
        self.queue_wmi_event(id::MGMT_RX_EV, &b);
    }

    pub fn roam(&mut self, reason: u32) {
        self.queue_wmi_event(id::ROAM_EV, &tlv(48, &le32s(&[0, reason, 30])));
    }

    pub fn kickout(&mut self, addr: &[u8; 6]) {
        let mut v = addr.to_vec();
        v.extend_from_slice(&[0, 0]);
        self.queue_wmi_event(id::PEER_KICKOUT_EV, &tlv(43, &v));
    }
}
