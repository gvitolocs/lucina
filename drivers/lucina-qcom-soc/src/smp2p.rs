//! Shared Memory Point-to-Point, Linux 7.1 `drivers/soc/qcom/smp2p.c` (GPL-2.0-only). Adapted from narf
//! `drivers/soc/qcom/src/smp2p.rs` (GPL-2.0-or-later, not run on silicon).
//!
//! Corrections from the recheck against Linux: the outbound version is 2, or the remote's if it published one of at
//! most 2, not a fixed 1; a remote with a newer version is refused; a lower version from the remote makes us step down
//! and kick; `qcom_smp2p_check_ssr` clears every entry's `last_value`, which narf missed, so that bits are seen again
//! after a remote restart.
//!
//! Each direction is one SMEM item written by one side:
//!
//! ```text
//! 0   u32 magic "$SMP"      12  u16 total_entries, u16 valid_entries
//! 4   u8 version, u24 features 16 u32 flags
//! 8   u16 local_pid, u16 remote_pid    20 entries[16] { u8 name[16]; u32 value; }
//! ```
//!
//! For the modem (`smp2p-mpss`): outbound `master-kernel` bit 0 is `stop`; inbound `slave-kernel` bits 0, 1, 2, 3, 7
//! are fatal, ready, handover, stop-ack, shutdown-ack. The modem's interrupt is SPI 451, our doorbell APCS bit 14.

use alloc::string::String;
use alloc::vec::Vec;

use crate::hwlock::HwSpinlock;
use crate::io::RegIo;
use crate::smem::{Smem, SmemError, SmemItem};

pub const SMP2P_MAGIC: u32 = 0x504d_5324;
pub const SMP2P_MAX_ENTRY: u32 = 16;
pub const SMP2P_MAX_ENTRY_NAME: usize = 16;
pub const SMP2P_FEATURE_SSR_ACK: u32 = 0x1;
pub const SMP2P_ALL_FEATURES: u32 = SMP2P_FEATURE_SSR_ACK;
pub const MAX_VERSION: u32 = 2;
const FLAGS_RESTART_DONE_BIT: u32 = 0;
const FLAGS_RESTART_ACK_BIT: u32 = 1;
/// `sizeof(struct smp2p_smem_item)`.
pub const ITEM_SIZE: u32 = 20 + SMP2P_MAX_ENTRY * 20;

/// Inbound `slave-kernel` bits of the modem (the device tree's `interrupt-names` order).
pub mod modem_bit {
    pub const FATAL: u32 = 0;
    pub const READY: u32 = 1;
    pub const HANDOVER: u32 = 2;
    pub const STOP_ACK: u32 = 3;
    pub const SHUTDOWN_ACK: u32 = 7;
}

const OFF_VERSION: u32 = 4;
const OFF_PIDS: u32 = 8;
const OFF_ENTRIES_N: u32 = 12;
const OFF_FLAGS: u32 = 16;
const OFF_ENTRY: u32 = 20;

/// Rings the remote's doorbell (`qcom_smp2p_kick`).
pub trait Kick {
    fn kick(&self);
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Smp2pError {
    Smem(SmemError),
    Full,
    BadName,
    BadHandle,
    ItemTooSmall,
    /// The remote published a version above [`MAX_VERSION`].
    UnsupportedVersion(u32),
}

impl From<SmemError> for Smp2pError {
    fn from(e: SmemError) -> Self {
        Smp2pError::Smem(e)
    }
}

/// `qcom,smem = <inbound outbound>`, `qcom,local-pid`, `qcom,remote-pid`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub smem_in: u32,
    pub smem_out: u32,
    pub local_pid: u16,
    pub remote_pid: u16,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct OutHandle(usize);
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct InHandle(usize);

struct InEntry {
    name: String,
    value: Option<u32>,
    last_value: u32,
    enabled: u32,
    rising: u32,
    falling: u32,
}

/// An enabled inbound bit that changed in its trigger's direction.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Irq {
    pub entry: InHandle,
    pub bit: u32,
    pub high: bool,
}

/// One edge (`struct qcom_smp2p`).
pub struct Smp2p<K> {
    cfg: Config,
    kick: K,
    out: SmemItem,
    inb: Option<SmemItem>,
    valid_entries: u32,
    negotiation_done: bool,
    ssr_ack_enabled: bool,
    ssr_ack: bool,
    inbound: Vec<InEntry>,
    outbound: Vec<u32>,
}

fn name_words(name: &str) -> Result<[u32; 4], Smp2pError> {
    let b = name.as_bytes();
    if b.len() >= SMP2P_MAX_ENTRY_NAME {
        return Err(Smp2pError::BadName);
    }
    let mut raw = [0u8; 16];
    raw[..b.len()].copy_from_slice(b);
    let mut w = [0u32; 4];
    for (i, c) in raw.as_chunks::<4>().0.iter().enumerate() {
        w[i] = u32::from_le_bytes([c[0], c[1], c[2], c[3]]);
    }
    Ok(w)
}

impl<K: Kick> Smp2p<K> {
    /// `qcom_smp2p_alloc_outbound_item`. The version goes last: it validates the item for the remote. Linux kicks here
    /// and again after adding the entries ([`Smp2p::kick`]).
    pub fn new<R: RegIo, L: HwSpinlock>(
        smem: &Smem<R>,
        lock: &L,
        cfg: Config,
        kick: K,
    ) -> Result<Self, Smp2pError> {
        match smem.alloc(lock, cfg.remote_pid, cfg.smem_out, ITEM_SIZE) {
            Ok(()) | Err(SmemError::Exists) => {}
            Err(e) => return Err(e.into()),
        }
        let out = smem.get(cfg.remote_pid, cfg.smem_out)?;
        if out.len < ITEM_SIZE {
            return Err(Smp2pError::ItemTooSmall);
        }
        // qcom_smp2p_in_version: the remote's version byte if its item already exists.
        let in_version = match smem.get(cfg.remote_pid, cfg.smem_in) {
            Ok(item) if item.len >= ITEM_SIZE => {
                smem.mem().read32(item.offset + OFF_VERSION) & 0xff
            }
            _ => 0,
        };
        if in_version > MAX_VERSION {
            return Err(Smp2pError::UnsupportedVersion(in_version));
        }
        let m = smem.mem();
        for w in 0..ITEM_SIZE / 4 {
            m.write32(out.offset + w * 4, 0);
        }
        m.write32(out.offset, SMP2P_MAGIC);
        m.write32(
            out.offset + OFF_PIDS,
            cfg.local_pid as u32 | ((cfg.remote_pid as u32) << 16),
        );
        m.write32(out.offset + OFF_ENTRIES_N, SMP2P_MAX_ENTRY);
        m.write32(out.offset + OFF_VERSION, SMP2P_ALL_FEATURES << 8);
        let version = if in_version != 0 {
            in_version
        } else {
            MAX_VERSION
        };
        m.write32(
            out.offset + OFF_VERSION,
            (SMP2P_ALL_FEATURES << 8) | version,
        );
        kick.kick();
        Ok(Self {
            cfg,
            kick,
            out,
            inb: None,
            valid_entries: 0,
            negotiation_done: false,
            ssr_ack_enabled: false,
            ssr_ack: false,
            inbound: Vec::new(),
            outbound: Vec::new(),
        })
    }

    pub fn negotiated(&self) -> bool {
        self.negotiation_done
    }
    pub fn ssr_ack_enabled(&self) -> bool {
        self.ssr_ack_enabled
    }
    pub fn outbound_item(&self) -> SmemItem {
        self.out
    }
    pub fn kick(&self) {
        self.kick.kick();
    }

    /// `qcom_smp2p_outbound_entry`.
    pub fn add_outbound<R: RegIo>(
        &mut self,
        smem: &Smem<R>,
        name: &str,
    ) -> Result<OutHandle, Smp2pError> {
        let words = name_words(name)?;
        let m = smem.mem();
        let n = m.read32(self.out.offset + OFF_ENTRIES_N) >> 16;
        if n >= SMP2P_MAX_ENTRY {
            return Err(Smp2pError::Full);
        }
        let e = self.out.offset + OFF_ENTRY + n * 20;
        for (i, w) in words.iter().enumerate() {
            m.write32(e + i as u32 * 4, *w);
        }
        m.write32(e + 16, 0);
        m.write32(
            self.out.offset + OFF_ENTRIES_N,
            SMP2P_MAX_ENTRY | ((n + 1) << 16),
        );
        self.outbound.push(e + 16);
        Ok(OutHandle(self.outbound.len() - 1))
    }

    /// `smp2p_update_bits`: kicks only if the value changed.
    pub fn update_bits<R: RegIo>(
        &self,
        smem: &Smem<R>,
        h: OutHandle,
        mask: u32,
        value: u32,
    ) -> Result<(), Smp2pError> {
        let e = *self.outbound.get(h.0).ok_or(Smp2pError::BadHandle)?;
        let m = smem.mem();
        let orig = m.read32(e);
        let v = (orig & !mask) | value;
        m.write32(e, v);
        if v != orig {
            self.kick.kick();
        }
        Ok(())
    }

    /// Registers an inbound entry by name (`slave-kernel`).
    pub fn add_inbound(&mut self, name: &str) -> Result<InHandle, Smp2pError> {
        name_words(name)?;
        self.inbound.push(InEntry {
            name: String::from(name),
            value: None,
            last_value: 0,
            enabled: 0,
            rising: 0,
            falling: 0,
        });
        Ok(InHandle(self.inbound.len() - 1))
    }

    /// `smp2p_set_irq_type` and `smp2p_unmask_irq` for one bit.
    pub fn enable_irq(
        &mut self,
        h: InHandle,
        bit: u32,
        rising: bool,
        falling: bool,
    ) -> Result<(), Smp2pError> {
        let e = self.inbound.get_mut(h.0).ok_or(Smp2pError::BadHandle)?;
        let b = 1u32 << (bit & 31);
        e.enabled |= b;
        e.rising = if rising { e.rising | b } else { e.rising & !b };
        e.falling = if falling {
            e.falling | b
        } else {
            e.falling & !b
        };
        Ok(())
    }

    /// Where an outbound entry's value is in SMEM: for a stop that cannot go through `self`, such as a quiesce hook before a jump.
    pub fn outbound_value_offset(&self, h: OutHandle) -> Option<u32> {
        self.outbound.get(h.0).copied()
    }

    /// Where an inbound entry's value is in SMEM, once the remote has made it.
    pub fn inbound_value_offset(&self, h: InHandle) -> Option<u32> {
        self.inbound.get(h.0)?.value
    }

    /// The last value read of an inbound entry; `None` before the remote created it.
    pub fn inbound_value(&self, h: InHandle) -> Option<u32> {
        let e = self.inbound.get(h.0)?;
        e.value.map(|_| e.last_value)
    }

    /// `qcom_smp2p_negotiate`.
    fn negotiate<R: RegIo>(&mut self, smem: &Smem<R>, inb: SmemItem) {
        let m = smem.mem();
        let in_word = m.read32(inb.offset + OFF_VERSION);
        let out_word = m.read32(self.out.offset + OFF_VERSION);
        let (in_ver, out_ver) = (in_word & 0xff, out_word & 0xff);
        if in_ver == out_ver {
            let features = (out_word >> 8) & (in_word >> 8);
            m.write32(self.out.offset + OFF_VERSION, (features << 8) | out_ver);
            self.ssr_ack_enabled = features & SMP2P_FEATURE_SSR_ACK != 0;
            self.negotiation_done = true;
        } else if in_ver != 0 && in_ver < out_ver {
            m.write32(self.out.offset + OFF_VERSION, (out_word & !0xff) | in_ver);
            self.kick.kick();
        }
    }

    /// `qcom_smp2p_intr`, the edge's interrupt. Returns each enabled bit whose edge matches.
    pub fn handle_interrupt<R: RegIo>(&mut self, smem: &Smem<R>) -> Vec<Irq> {
        let mut fired = Vec::new();
        if self.inb.is_none() {
            match smem.get(self.cfg.remote_pid, self.cfg.smem_in) {
                Ok(i) if i.len >= ITEM_SIZE => self.inb = Some(i),
                _ => return fired,
            }
        }
        let Some(inb) = self.inb else {
            return fired;
        };
        if !self.negotiation_done {
            self.negotiate(smem, inb);
        }
        if !self.negotiation_done {
            return fired;
        }
        let m = smem.mem();
        // qcom_smp2p_check_ssr
        let mut ack_restart = false;
        if self.ssr_ack_enabled {
            let done = m.read32(inb.offset + OFF_FLAGS) & (1 << FLAGS_RESTART_DONE_BIT) != 0;
            ack_restart = done != self.ssr_ack;
            for e in self.inbound.iter_mut().filter(|e| e.value.is_some()) {
                e.last_value = 0;
            }
        }
        // qcom_smp2p_notify_in: newly created entries. The count comes from the remote, so it is capped.
        let in_valid = (m.read32(inb.offset + OFF_ENTRIES_N) >> 16).min(SMP2P_MAX_ENTRY);
        for i in self.valid_entries..in_valid {
            let e = inb.offset + OFF_ENTRY + i * 20;
            let mut raw = [0u8; 16];
            crate::io::read_bytes(m, e, &mut raw);
            let len = raw.iter().position(|&b| b == 0).unwrap_or(16);
            if let Some(ent) = self
                .inbound
                .iter_mut()
                .find(|n| n.name.as_bytes() == &raw[..len])
            {
                ent.value = Some(e + 16);
            }
        }
        self.valid_entries = self.valid_entries.max(in_valid);
        for (idx, ent) in self.inbound.iter_mut().enumerate() {
            let Some(at) = ent.value else { continue };
            let val = m.read32(at);
            let status = val ^ ent.last_value;
            ent.last_value = val;
            for bit in 0..32 {
                let b = 1u32 << bit;
                if ent.enabled & b == 0 || status & b == 0 {
                    continue;
                }
                let high = val & b != 0;
                if (high && ent.rising & b != 0) || (!high && ent.falling & b != 0) {
                    fired.push(Irq {
                        entry: InHandle(idx),
                        bit,
                        high,
                    });
                }
            }
        }
        if ack_restart {
            // qcom_smp2p_do_ssr_ack
            self.ssr_ack = !self.ssr_ack;
            let f = m.read32(self.out.offset + OFF_FLAGS) & !(1 << FLAGS_RESTART_ACK_BIT);
            m.write32(
                self.out.offset + OFF_FLAGS,
                f | ((self.ssr_ack as u32) << FLAGS_RESTART_ACK_BIT),
            );
            self.kick.kick();
        }
        fired
    }
}
