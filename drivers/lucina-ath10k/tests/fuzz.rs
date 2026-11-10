//! Deterministic fuzzing of everything that parses bytes the firmware (or the copy engine) wrote: random buffers,
//! and every fixture message truncated at every length and mutated. Nothing may panic; where the C trusted the
//! length it was given, the answer is an error.
//! The expected values derive from Linux ath10k (SPDX ISC), see oracle/README.md.

mod common;

use common::{Lfsr, hex};
use lucina_ath10k::ce::{self, CeError, CopyEngine, DescMem, Regs, RingPlacement};
use lucina_ath10k::htc::{self, Htc};
use lucina_ath10k::htt;
use lucina_ath10k::snoc_config::{self, CE_COUNT, HOST_CE_CONFIG};
use lucina_ath10k::wmi_tlv as wmi;

/// Every message in the fixtures: `bytes.NAME HEX`, `NAME LEN HEX` and the hex of the `trailer` vectors.
fn corpus() -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for text in [
        include_str!("fixtures/wmi_oracle.txt"),
        include_str!("fixtures/htt_oracle.txt"),
        include_str!("fixtures/htc_oracle.txt"),
    ] {
        for l in text.lines() {
            let f: Vec<&str> = l.split(' ').collect();
            let h = if l.starts_with("bytes.") && f.len() == 2 {
                f[1]
            } else if f.len() == 3
                && f[1].chars().all(|c| c.is_ascii_digit())
                && f[2].len().is_multiple_of(2)
                && !f[2].is_empty()
                && f[2].chars().all(|c| c.is_ascii_hexdigit())
            {
                f[2]
            } else {
                continue;
            };
            out.push(hex(h));
        }
    }
    assert!(out.len() > 40, "corpus too small: {}", out.len());
    out
}

fn run_all_decoders(b: &[u8]) {
    // HTC
    let _ = htc::Hdr::parse(b);
    let mut h = Htc::new().unwrap();
    let _ = h.process_trailer(b);
    let _ = h.rx_frame(b);
    let _ = h.on_control_message(b);
    // READY first, then connect messages in every state
    let mut ready = vec![1, 0, 1, 0, 0, 8, 8, 0, 1, 16, 0, 0];
    ready[2..4].copy_from_slice(&0x20u16.to_le_bytes());
    ready[4..6].copy_from_slice(&0x800u16.to_le_bytes());
    if h.on_control_message(&ready).is_ok() {
        let _ = h.connect_service(htc::svc::WMI_CONTROL);
        let _ = h.on_control_message(b);
        let _ = h.rx_frame(b);
        let _ = h.process_trailer(b);
        let _ = h.start();
        let _ = h.on_control_message(b);
    }
    // WMI-TLV
    for t in wmi::iter(b) {
        let _ = t;
    }
    let _ = wmi::rx(b);
    let _ = wmi::parse_service_ready(b);
    let _ = wmi::parse_ready(b);
    let _ = wmi::parse_service_available(b);
    let _ = wmi::parse_scan_event(b);
    let _ = wmi::parse_vdev_start_response(b);
    // HTT
    let _ = htt::parse_resp(b);
    let _ = htt::parse_version_conf(b);
    let _ = htt::parse_tx_completion(b);
    let _ = htt::parse_tx_credit_update(b);
    let _ = htt::parse_rx_in_ord_ind(b);
    let _ = htt::parse_rx_offload_msdu(b);
    let _ = htt::parse_rx_offload_ind(b);
}

#[test]
fn random_buffers_never_panic() {
    let mut rng = Lfsr(0x1234_5678);
    for _ in 0..20_000 {
        let n = (rng.next() % 300) as usize;
        let mut b = vec![0u8; n];
        rng.fill(&mut b);
        // bias the first bytes towards valid types so the parsers get past their first check
        if n > 0 && rng.next().is_multiple_of(2) {
            b[0] = [0x07, 0x12, 0x00, 0x11, 0x0f][(rng.next() % 5) as usize];
        }
        run_all_decoders(&b);
    }
}

#[test]
fn every_fixture_message_truncated_and_mutated_never_panics() {
    let mut rng = Lfsr(0xdead_beef);
    let corpus = corpus();
    let mut runs = 0usize;
    for m in &corpus {
        for cut in 0..=m.len() {
            run_all_decoders(&m[..cut]);
            runs += 1;
        }
        for _ in 0..150 {
            let mut x = m.clone();
            for _ in 0..(1 + rng.next() % 4) {
                if x.is_empty() {
                    break;
                }
                let i = (rng.next() as usize) % x.len();
                x[i] = match rng.next() % 3 {
                    0 => 0xff,
                    1 => x[i] ^ (1 << (rng.next() % 8)),
                    _ => rng.byte(),
                };
            }
            if rng.next().is_multiple_of(4) {
                let keep = (rng.next() as usize) % (x.len() + 1);
                x.truncate(keep);
            }
            run_all_decoders(&x);
            runs += 1;
        }
    }
    assert!(runs > 10_000, "{runs}");
}

#[test]
fn tlv_iteration_terminates_and_stays_in_bounds() {
    let mut rng = Lfsr(77);
    for _ in 0..5_000 {
        let n = (rng.next() % 200) as usize;
        let mut b = vec![0u8; n];
        rng.fill(&mut b);
        let mut covered = 0usize;
        for t in wmi::iter(&b) {
            match t {
                Ok(t) => {
                    assert!(t.offset >= 4 && t.offset + t.value.len() <= b.len());
                    assert!(t.offset >= covered + 4);
                    covered = t.offset + t.value.len();
                }
                Err(e) => assert_eq!(e, wmi::WmiError::Invalid),
            }
        }
    }
}

#[test]
fn service_lookup_never_panics_for_any_id() {
    for id in 0..=u16::MAX {
        let _ = snoc_config::map_service_to_pipe(id);
    }
}

// ---- copy engine against a hostile target -----------------------------------------------------------------------

struct Dev {
    rng: Lfsr,
    /// Whether register reads return random values or the written ones.
    chaos: bool,
    regs: Vec<u32>,
}

impl Regs for Dev {
    fn read32(&mut self, off: usize) -> u32 {
        if self.chaos {
            match self.rng.next() % 4 {
                0 => 0xffff_ffff,
                1 => self.rng.next(),
                _ => self.regs[off / 4],
            }
        } else {
            self.regs[off / 4]
        }
    }
    fn write32(&mut self, off: usize, v: u32) {
        self.regs[off / 4] = v;
    }
}

struct Mem {
    rng: Lfsr,
    hostile: bool,
    bytes: Vec<u8>,
}

impl DescMem for Mem {
    fn read32(&mut self, off: usize) -> u32 {
        if self.hostile && self.rng.next().is_multiple_of(3) {
            return self.rng.next();
        }
        u32::from_le_bytes(self.bytes[off..off + 4].try_into().unwrap())
    }
    fn write32(&mut self, off: usize, v: u32) {
        self.bytes[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }
}

fn setup(seed: u32, chaos: bool) -> (CopyEngine, Dev, Mem) {
    let mut ce = CopyEngine::new();
    let mut dev = Dev {
        rng: Lfsr(seed),
        chaos: false,
        regs: vec![0; 0x80_0000 / 4],
    };
    let mut mem = Mem {
        rng: Lfsr(seed ^ 0x55aa),
        hostile: false,
        bytes: vec![0; 0x20_0000],
    };
    let mut host = 0usize;
    let mut bus = 0x1_0000_0000u64;
    let mut place = |size: usize| {
        let p = RingPlacement {
            host_offset: host,
            bus_addr: bus,
        };
        host += (size + 15) & !15;
        bus += 0x10000;
        p
    };
    let rri = place(CE_COUNT * 4);
    ce.alloc_rri(&mut dev, rri.host_offset, rri.bus_addr);
    for (id, &a) in HOST_CE_CONFIG.iter().enumerate() {
        let s = (a.src_nentries != 0).then(|| place(ce::ring_mem_size(a.src_nentries as usize)));
        let d = (a.dest_nentries != 0).then(|| place(ce::ring_mem_size(a.dest_nentries as usize)));
        ce.alloc_pipe(id, &a, s, d).unwrap();
        ce.init_pipe(&mut dev, &mut mem, id, &a).unwrap();
    }
    dev.chaos = chaos;
    mem.hostile = chaos;
    (ce, dev, mem)
}

fn check_invariants(ce: &CopyEngine) {
    for id in 0..CE_COUNT {
        let p = ce.pipe(id).unwrap();
        for r in [p.src_ring.as_ref(), p.dest_ring.as_ref()]
            .into_iter()
            .flatten()
        {
            assert!(r.sw_index < r.nentries(), "ce {id} sw_index {}", r.sw_index);
            assert!(
                r.write_index < r.nentries(),
                "ce {id} write_index {}",
                r.write_index
            );
            assert!(r.hw_index < r.nentries(), "ce {id} hw_index {}", r.hw_index);
        }
    }
}

#[test]
fn copy_engine_survives_a_hostile_target() {
    for seed in 1..=40u32 {
        let (mut ce, mut dev, mut mem) = setup(seed, true);
        let mut rng = Lfsr(seed.wrapping_mul(2654435761) | 1);
        for _ in 0..3_000 {
            let id = (rng.next() % 12) as usize;
            match rng.next() % 9 {
                0 => {
                    let _ = ce.send(
                        &mut dev,
                        &mut mem,
                        id,
                        rng.next().into(),
                        rng.next() as u64 * rng.next() as u64,
                        rng.next() % 70_000,
                        rng.next(),
                        rng.next() & 0x1_0001,
                    );
                }
                1 => {
                    let _ = ce.rx_post_buf(&mut dev, &mut mem, id, 7, (rng.next() as u64) << 7);
                }
                2 => {
                    // drain: must stop within one lap
                    let n = ce
                        .pipe(id)
                        .unwrap()
                        .src_ring
                        .as_ref()
                        .map_or(0, |r| r.nentries());
                    // a target that changes its mind on every read can keep completing; the caller bounds the loop
                    let mut k = 0;
                    while ce.completed_send_next(&mut dev, &mut mem, id).is_ok() && k <= 4 * n {
                        k += 1;
                    }
                }
                3 => {
                    let n = ce
                        .pipe(id)
                        .unwrap()
                        .dest_ring
                        .as_ref()
                        .map_or(0, |r| r.nentries());
                    let mut k = 0;
                    while ce.completed_recv_next(&mut mem, id).is_ok() && k <= 4 * n {
                        k += 1;
                    }
                }
                4 => {
                    let _ = ce.rx_update_write_idx(&mut dev, id, rng.next());
                }
                5 => {
                    ce.service_any(&mut dev, |_, _| {});
                }
                6 => {
                    let _ = ce.per_engine_service(&mut dev, id);
                }
                7 => {
                    let _ = ce.enable_interrupt(&mut dev, id);
                    let _ = ce.disable_interrupt(&mut dev, id);
                }
                _ => {
                    let _ = ce.num_free_src_entries(id);
                    let _ = ce.rx_num_free_bufs(id);
                }
            }
            check_invariants(&ce);
        }
    }
}

#[test]
fn rri_words_of_every_shape_are_harmless() {
    let (mut ce, mut dev, mut mem) = setup(5, false);
    let rri_off = 0usize; // alloc_rri was given offset 0 by setup()
    for word in [
        0u32,
        1,
        0xffff,
        0x1_0000,
        0xffff_ffff,
        0xffff_0000,
        0x8000_0001,
        0x0003_ffff,
    ] {
        for id in 0..CE_COUNT {
            mem.write32(rri_off + 4 * id, word);
            dev.regs[(ce::hw::ce_base(id) + ce::hw::CURRENT_SRRI) / 4] = word;
        }
        for id in [0usize, 3, 4, 7] {
            for _ in 0..40 {
                let _ = ce.send(&mut dev, &mut mem, id, 1, 0x1000, 8, 0, 0);
            }
            let n = ce.pipe(id).unwrap().src_ring.as_ref().unwrap().nentries();
            {
                // the register path (engines that take interrupts) reports the cached index masked, too
                let r = ce.src_ring_mut(id).unwrap();
                r.hw_index = r.sw_index;
            }
            let mut k = 0;
            while ce.completed_send_next(&mut dev, &mut mem, id).is_ok() {
                k += 1;
                assert!(
                    k <= n,
                    "index {word:#x}: completion loop ran away on ce {id}"
                );
            }
            check_invariants(&ce);
        }
    }
}

#[test]
fn register_read_index_of_all_ones_is_no_device() {
    let (mut ce, mut dev, mut mem) = setup(9, false);
    // CE3 reads its read index from the register (it is not DIS_INTR)
    dev.regs[(ce::hw::ce_base(3) + ce::hw::CURRENT_SRRI) / 4] = 0xffff_ffff;
    {
        let r = ce.src_ring_mut(3).unwrap();
        r.hw_index = r.sw_index;
    }
    assert_eq!(
        ce.completed_send_next(&mut dev, &mut mem, 3),
        Err(CeError::NoDevice)
    );
    // and a shadow-less engine cannot ring a doorbell
    assert_eq!(
        ce.send(&mut dev, &mut mem, 1, 0, 0, 0, 0, 0),
        Err(CeError::BadPipe)
    );
    assert_eq!(
        ce.send(&mut dev, &mut mem, 12, 0, 0, 0, 0, 0),
        Err(CeError::BadPipe)
    );
    assert_eq!(
        ce.send(&mut dev, &mut mem, 3, 0, 0, 0x1_0000, 0, 0),
        Err(CeError::TooLong)
    );
    ce.crash_flush = true;
    assert_eq!(
        ce.send(&mut dev, &mut mem, 3, 0, 0, 1, 0, 0),
        Err(CeError::Shutdown)
    );
}

#[test]
fn ring_math_matches_the_macros() {
    // CE_RING_DELTA / power of two rounding
    assert_eq!(ce::ring_delta(15, 3, 3), 0);
    assert_eq!(ce::ring_delta(15, 14, 2), 4);
    assert_eq!(ce::ring_delta(15, 2, 14u32.wrapping_sub(0)), 12);
    assert_eq!(ce::ring_delta(15, 0, 0u32.wrapping_sub(1)), 15);
    assert_eq!(ce::ring_entries(0), None);
    assert_eq!(ce::ring_entries(1), Some(1));
    assert_eq!(ce::ring_entries(17), Some(32));
    assert_eq!(ce::ring_entries(512), Some(512));
    assert_eq!(ce::ring_entries(0x1_0000), Some(0x1_0000));
    assert_eq!(ce::ring_entries(0x1_0001), None);
}

#[test]
fn descriptor_roundtrip_and_fields() {
    let d = ce::Desc64 {
        addr_lo: 1,
        addr_hi: 0x8001_0007,
        nbytes: 0x1234,
        flags: 0x0ab3,
        toeplitz_hash_result: 9,
    };
    assert_eq!(ce::Desc64::from_le_bytes(&d.to_le_bytes()), d);
    assert_eq!(d.buffer_addr(), (7u64 << 32) | 1);
    assert_eq!(d.transfer_id(), 0xab);
}
