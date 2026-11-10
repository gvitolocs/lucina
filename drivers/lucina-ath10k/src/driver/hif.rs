//! The SNOC HIF of `snoc.c` over [`CopyEngine`]: pipes in one arena, rx buffer posting, single sends and the polled
//! completion loops.
//!
//! Derived from Linux `drivers/net/wireless/ath/ath10k/snoc.c` (SPDX ISC; Copyright (c) 2018 The Linux Foundation,
//! Qualcomm Technologies, Inc. and/or its subsidiaries).
//!
//! `ath10k_snoc_hif_power_up` is [`Hif::power_up`], minus the rails and `wlan_enable`, which the kernel has done over
//! QMI before. `ath10k_snoc_hif_start` is [`Hif::post_all`]. `ath10k_snoc_process_rx_cb`, the tx callbacks and
//! `ath10k_snoc_napi_poll` are [`Hif::service`].
//!
//! [`Hif::service`] runs every engine with a callback on every call, not only those `service_any` flags. That summary
//! register belongs to the PCI path (`ath10k_ce_per_engine_service_any`); SNOC goes by per-engine interrupts, and nobody
//! here has seen the summary work on this chip, while a completion loop needs no interrupt to find finished
//! descriptors.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::ce::{self, CeError, CopyEngine, RingPlacement};
use crate::snoc_config::{CE_COUNT, CE_POLL_PIPE, CeAttr, HOST_CE_CONFIG};

use super::{Config, Dma, DmaBuf, Platform};

/// The context of a send with nothing to free (an HTT frame: its buffers stay with the MSDU id, as in the C, where
/// the transfer context is NULL).
pub(crate) const CTX_NONE: u64 = u64::MAX;

struct RxPipe {
    buf_sz: usize,
    /// The buffer posted under each context.
    posted: Vec<Option<DmaBuf>>,
    free_ctx: Vec<u32>,
    spare: Vec<DmaBuf>,
    /// `post_pipe` could not fill the ring: try again without waiting for traffic. The refill writes a register.
    owed: bool,
}

/// One received buffer: the engine and its bytes.
pub(crate) struct RxMsg {
    pub ce: u8,
    pub bytes: Vec<u8>,
}

pub(crate) struct Hif {
    pub ce: CopyEngine,
    attrs: [CeAttr; CE_COUNT],
    rx: Vec<RxPipe>,
    tx: BTreeMap<u64, DmaBuf>,
    next_ctx: u64,
}

/// The host engine table with the receive rings cut to [`Config`]: `HOST_CE_CONFIG` sizes them for a much faster
/// machine, and the host alone decides how many buffers it offers.
pub fn host_attrs(cfg: &Config) -> [CeAttr; CE_COUNT] {
    let mut a = HOST_CE_CONFIG;
    for (i, e) in a.iter_mut().enumerate() {
        let cap = match i {
            1 => cfg.ce_rx_htt,
            2 => cfg.ce_rx_wmi,
            5 => cfg.ce_rx_htt_small,
            8 | 11 => cfg.ce_rx_unused,
            9 | 10 => cfg.ce_rx_htt_extra,
            _ => e.dest_nentries,
        };
        e.dest_nentries = e.dest_nentries.min(cap);
    }
    a[CE_POLL_PIPE].src_nentries = a[CE_POLL_PIPE].src_nentries.min(cfg.ce4_entries);
    a
}

/// Bytes of the ring arena [`Hif::power_up`] uses: each ring with its alignment slack, then the RRI array.
pub fn arena_bytes(attrs: &[CeAttr; CE_COUNT]) -> usize {
    let mut n = 0;
    for a in attrs {
        for e in [a.src_nentries, a.dest_nentries] {
            if let Some(r) = ce::ring_entries(e) {
                n += ce::ring_mem_size(r as usize);
            }
        }
    }
    n + CE_COUNT * 4 + ce::DESC_RING_ALIGN
}

impl Hif {
    pub fn new(cfg: &Config) -> Hif {
        let mut ce = CopyEngine::new();
        ce.poll_from_memory();
        let attrs = host_attrs(cfg);
        let mut rx = Vec::with_capacity(CE_COUNT);
        for a in &attrs {
            let n = ce::ring_entries(a.dest_nentries).unwrap_or(0) as usize;
            let mut posted = Vec::with_capacity(n);
            posted.resize_with(n, || None);
            rx.push(RxPipe {
                buf_sz: a.src_sz_max as usize,
                posted,
                free_ctx: (0..n as u32).rev().collect(),
                spare: Vec::new(),
                owed: false,
            });
        }
        Hif {
            ce,
            attrs,
            rx,
            tx: BTreeMap::new(),
            next_ctx: 0,
        }
    }

    /// `ath10k_ce_alloc_pipe` for all engines, `ath10k_ce_alloc_rri`, `ath10k_snoc_init_pipes`,
    /// `ath10k_ce_enable_interrupts`. Returns false if the arena is too small or not 8-byte aligned.
    pub fn power_up<P: Platform>(&mut self, plat: &mut P) -> Result<(), CeError> {
        let bus = plat.arena_bus_addr();
        let need = arena_bytes(&self.attrs);
        if plat.arena_len() < need || bus & (ce::DESC_RING_ALIGN as u64 - 1) != 0 {
            return Err(CeError::BadPipe);
        }
        let (regs, mem, _) = plat.parts();
        let mut off = 0usize;
        let mut place = |entries: u32| -> Option<RingPlacement> {
            let r = ce::ring_entries(entries)? as usize;
            let at = RingPlacement {
                host_offset: off,
                bus_addr: bus + off as u64,
            };
            off += ce::ring_mem_size(r);
            Some(at)
        };
        for (i, a) in self.attrs.iter().enumerate() {
            let src = place(a.src_nentries);
            let dest = place(a.dest_nentries);
            self.ce.alloc_pipe(i, a, src, dest)?;
        }
        // the RRI array, zeroed: the device reads and writes it
        let rri = (off + ce::DESC_RING_ALIGN - 1) & !(ce::DESC_RING_ALIGN - 1);
        for i in 0..CE_COUNT {
            ce::DescMem::write32(mem, rri + 4 * i, 0);
        }
        self.ce.alloc_rri(regs, rri, bus + rri as u64);
        for (i, a) in self.attrs.iter().enumerate() {
            self.ce.init_pipe(regs, mem, i, a)?;
        }
        self.ce.enable_interrupts(regs);
        Ok(())
    }

    /// `ath10k_snoc_rx_post_pipe`. A buffer that cannot be allocated is tried again on the next poll (the C uses a
    /// 50 ms timer for the same thing).
    fn post_pipe<P: Platform>(&mut self, plat: &mut P, id: usize) {
        let Some(pipe) = self.rx.get_mut(id) else {
            return;
        };
        if pipe.buf_sz == 0 || self.ce.pipe(id).is_none_or(|p| p.dest_ring.is_none()) {
            return;
        }
        let num = self.ce.rx_num_free_bufs(id).unwrap_or(0);
        let (regs, mem, dma) = plat.parts();
        for _ in 0..num {
            let Some(ctx) = pipe.free_ctx.pop() else {
                return;
            };
            let Some(buf) = pipe.spare.pop().or_else(|| dma.alloc(pipe.buf_sz)) else {
                pipe.free_ctx.push(ctx);
                pipe.owed = true;
                return;
            };
            match self
                .ce
                .rx_post_buf(regs, mem, id, u64::from(ctx), buf.paddr)
            {
                Ok(()) => pipe.posted[ctx as usize] = Some(buf),
                Err(_) => {
                    pipe.spare.push(buf);
                    pipe.free_ctx.push(ctx);
                    pipe.owed = true;
                    return;
                }
            }
        }
    }

    /// `ath10k_snoc_rx_post`.
    pub fn post_all<P: Platform>(&mut self, plat: &mut P) {
        for id in 0..CE_COUNT {
            self.post_pipe(plat, id);
        }
    }

    /// `ath10k_ce_send` for one buffer: copy `bytes` into a DMA buffer freed at completion. The transfer id is the
    /// endpoint, as `ath10k_htc_send` sets it.
    pub fn send<P: Platform>(
        &mut self,
        plat: &mut P,
        pipe: u8,
        transfer_id: u8,
        bytes: &[u8],
    ) -> Result<(), CeError> {
        let (regs, mem, dma) = plat.parts();
        let Some(buf) = dma.alloc(bytes.len()) else {
            return Err(CeError::NoBuffers);
        };
        dma.write(&buf, 0, bytes);
        let ctx = self.next_ctx;
        self.next_ctx = (ctx + 1) % CTX_NONE;
        match self.ce.send(
            regs,
            mem,
            usize::from(pipe),
            ctx,
            buf.paddr,
            bytes.len() as u32,
            u32::from(transfer_id),
            0,
        ) {
            Ok(()) => {
                self.tx.insert(ctx, buf);
                Ok(())
            }
            Err(e) => {
                dma.free(buf);
                Err(e)
            }
        }
    }

    /// One pass over the engines: `ath10k_ce_per_engine_service`'s acknowledge, then the callbacks, receive before send
    /// on each engine, with CE4's completions first (`ath10k_snoc_htt_rx_cb` polls CE4 whenever HTT arrives). Each engine's
    /// loop runs at most a ring's worth of times, whatever the descriptors say.
    ///
    /// An idle pass touches no register, on purpose. Linux reaches the block's registers only from an interrupt, when the
    /// device has just done something. Polling them every millisecond on the laptop killed the machine about 15 s after the
    /// firmware came up, while the same firmware unpolled stayed up, which fits the block powering down when idle, though
    /// nobody has seen it do that. So completions are found in shared memory: descriptors for receive, the read indexes the
    /// device mirrors for send. Doorbells go to shadow registers, as in Linux. The HOST_IS clear comes before the drain,
    /// as `ath10k_ce_per_engine_service` does it in the C, but only for an engine that has work: its block is awake
    /// because the device just used it.
    /// `ath10k_snoc_napi_poll`'s first half: `ath10k_ce_per_engine_service` for each engine whose interrupt came (bit `id` of
    /// `mask`), which clears its `HOST_IS` so that its level line drops before the caller unmasks it. [`Hif::service`] then
    /// drains every engine as before.
    pub fn ack<P: Platform>(&mut self, plat: &mut P, mask: u32) {
        let (regs, _, _) = plat.parts();
        for id in 0..CE_COUNT {
            if mask & (1 << id) != 0 {
                let _ = self.ce.per_engine_service(regs, id);
            }
        }
    }

    /// Whether a receive pipe is still owed buffers (a post failed), which `ath10k_snoc_rx_post_pipe` retries on its
    /// `rx_post_retry` timer.
    pub fn rx_owed(&self) -> bool {
        self.rx.iter().any(|p| p.owed)
    }

    pub fn service<P: Platform>(&mut self, plat: &mut P) -> Vec<RxMsg> {
        let mut out = Vec::new();
        self.drain_send(plat, CE_POLL_PIPE);
        for id in 0..CE_COUNT {
            let a = self.attrs[id];
            let has_rx = a.recv_cb.is_some() && a.dest_nentries != 0;
            let has_tx = a.send_cb.is_some() && a.src_nentries != 0;
            if !has_rx && !has_tx {
                continue;
            }
            // Linux clears HOST_IS before running the callbacks; the peek is memory-only, so an idle engine still
            // gets no register write.
            let pending = {
                let (_, mem, _) = plat.parts();
                (has_rx && self.ce.recv_pending(mem, id))
                    || (has_tx && self.ce.send_pending(mem, id))
            };
            if pending {
                let (regs, _, _) = plat.parts();
                let _ = self.ce.per_engine_service(regs, id);
            }
            let received = out.len();
            if has_rx {
                self.drain_recv(plat, id, &mut out);
            }
            if has_tx {
                self.drain_send(plat, id);
            }
            if has_rx && (out.len() != received || self.rx[id].owed) {
                self.rx[id].owed = false;
                self.post_pipe(plat, id);
            }
        }
        out
    }

    /// One line per engine and one for the RRI registers. Register values, the driver's own indexes, the read indexes the
    /// device mirrors into memory and the first destination descriptor.
    pub fn report<P: Platform>(&mut self, plat: &mut P) -> Vec<alloc::string::String> {
        use ce::hw;

        let (regs, mem, _) = plat.parts();
        let mut lines = Vec::new();
        lines.push(alloc::format!(
            "rri registers {:#x}:{:#x}",
            ce::Regs::read32(regs, hw::CE_RRI_HIGH),
            ce::Regs::read32(regs, hw::CE_RRI_LOW)
        ));
        for id in 0..CE_COUNT {
            let b = hw::ce_base(id);
            let mut r = |off: usize| ce::Regs::read32(regs, b + off);
            let mut line = alloc::format!(
                "ce{}: sr {:#x}:{:#x}+{} dr {:#x}:{:#x}+{} ctrl1 {:#x} ie {:#x} is {:#x} misc {:#x}/{:#x} wr {}/{} rr {}/{} wm {:#x}/{:#x}",
                id,
                r(hw::SR_BASE_ADDR_HI),
                r(hw::SR_BASE_ADDR_LO),
                r(hw::SR_SIZE),
                r(hw::DR_BASE_ADDR_HI),
                r(hw::DR_BASE_ADDR_LO),
                r(hw::DR_SIZE),
                r(hw::CTRL1),
                r(hw::HOST_IE),
                r(hw::HOST_IS),
                r(hw::MISC_IE),
                r(hw::MISC_IE + 4),
                r(hw::SR_WR_INDEX),
                r(hw::DST_WR_INDEX),
                r(hw::CURRENT_SRRI),
                r(hw::CURRENT_DRRI),
                r(hw::SRC_WM),
                r(hw::DST_WM),
            );
            if let Some(word) = self.ce.rri_raw(mem, id) {
                line.push_str(&alloc::format!(" ddr {word:#x}"));
            }
            if let Some(p) = self.ce.pipe(id)
                && let Some(ring) = p.dest_ring.as_ref()
            {
                let off = ring.desc_offset(0);
                let (a, b) = (ce::DescMem::read32(mem, off), ce::DescMem::read32(mem, off + 4));
                line.push_str(&alloc::format!(
                    " host {}/{} desc0 {:#x} {:#x} posted {}",
                    ring.sw_index,
                    ring.write_index,
                    a,
                    b,
                    self.rx[id].posted.iter().filter(|p| p.is_some()).count()
                ));
            }
            lines.push(line);
        }
        lines
    }

    /// `ath10k_snoc_process_rx_cb`: a buffer whose length exceeds what was posted is dropped, as in the C.
    fn drain_recv<P: Platform>(&mut self, plat: &mut P, id: usize, out: &mut Vec<RxMsg>) {
        let limit = self.rx[id].posted.len();
        let (_, mem, dma) = plat.parts();
        for _ in 0..limit {
            let Ok((ctx, nbytes)) = self.ce.completed_recv_next(mem, id) else {
                break;
            };
            let pipe = &mut self.rx[id];
            let Some(ctx) = ctx else {
                continue;
            };
            let Some(buf) = pipe.posted.get_mut(ctx as usize).and_then(Option::take) else {
                continue;
            };
            pipe.free_ctx.push(ctx as u32);
            let n = nbytes as usize;
            if n <= buf.len {
                let mut bytes = alloc::vec![0u8; n];
                dma.read(&buf, 0, &mut bytes);
                out.push(RxMsg {
                    ce: id as u8,
                    bytes,
                });
            }
            pipe.spare.push(buf);
        }
    }

    /// `ath10k_snoc_htc_tx_cb` and `ath10k_snoc_htt_tx_cb`: free what the engine has finished.
    /// Whether any send finished.
    pub fn drain_send<P: Platform>(&mut self, plat: &mut P, id: usize) -> bool {
        let limit = self
            .ce
            .pipe(id)
            .and_then(|p| p.src_ring.as_ref())
            .map_or(0, |r| r.nentries());
        let (regs, mem, dma) = plat.parts();
        let mut any = false;
        for _ in 0..limit {
            match self.ce.completed_send_next(regs, mem, id) {
                Ok(Some(ctx)) if ctx != CTX_NONE => {
                    any = true;
                    if let Some(b) = self.tx.remove(&ctx) {
                        dma.free(b);
                    }
                }
                Ok(_) => any = true,
                Err(_) => break,
            }
        }
        any
    }

    /// `ath10k_snoc_hif_stop` and `ath10k_snoc_hif_power_down`: free the posted and spare receive buffers and the
    /// sends in flight. Only after the target is stopped (QMI `wlan_disable`): memory the firmware may still write
    /// is never given back while it runs, which is why the kernel forgets the driver instead of stopping it. The rings
    /// and the RRI array stay in the platform's arena and are left alone.
    ///
    /// No copy-engine register is written here, as in the C: `ath10k_snoc_hif_stop` only disables the GIC lines and
    /// cleans buffers, and `ath10k_ce_deinit_pipe` is never called on this bus. Once the last CE interrupt is cleared
    /// the block may sleep and any register access can fault (`ce.c`'s bring-up note), so the stop path touches
    /// memory only.
    pub fn power_down<P: Platform>(&mut self, plat: &mut P) {
        let (_, _, dma) = plat.parts();
        for pipe in &mut self.rx {
            for b in pipe.posted.iter_mut().filter_map(Option::take) {
                dma.free(b);
            }
            for b in pipe.spare.drain(..) {
                dma.free(b);
            }
        }
        while let Some((_, b)) = self.tx.pop_first() {
            dma.free(b);
        }
    }
}
