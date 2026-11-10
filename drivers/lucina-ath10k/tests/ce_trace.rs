//! Replays oracle/ce_oracle.c scenario by scenario. The C side runs Linux's unchanged ce.c against a fake register
//! block; both sides print every register access and descriptor in the same format, and the texts must be equal.
//! The expected values derive from Linux ath10k (SPDX ISC), see oracle/README.md.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::rc::Rc;

use lucina_ath10k::ce::{self, CeError, CopyEngine, DescMem, Regs, RingPlacement, hw};
use lucina_ath10k::snoc_config::{CE_COUNT, HOST_CE_CONFIG};

type Out = Rc<RefCell<String>>;

struct Dev {
    regs: Vec<u32>,
    out: Out,
}

impl Regs for Dev {
    fn read32(&mut self, off: usize) -> u32 {
        let v = self.regs[off / 4];
        writeln!(self.out.borrow_mut(), "R {off:06x} -> {v:08x}").unwrap();
        v
    }
    fn write32(&mut self, off: usize, v: u32) {
        writeln!(self.out.borrow_mut(), "W {off:06x} = {v:08x}").unwrap();
        self.regs[off / 4] = v;
    }
}

struct Arena(Vec<u8>);

impl DescMem for Arena {
    fn read32(&mut self, off: usize) -> u32 {
        u32::from_le_bytes(self.0[off..off + 4].try_into().unwrap())
    }
    fn write32(&mut self, off: usize, v: u32) {
        self.0[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }
}

fn errno(r: Result<(), CeError>) -> i32 {
    match r {
        Ok(()) => 0,
        Err(e) => err_no(e),
    }
}

fn err_no(e: CeError) -> i32 {
    match e {
        CeError::NoSpace => -63,
        CeError::NoBuffers => -28,
        CeError::NotReady => -5,
        CeError::NoDevice => -19,
        other => panic!("unexpected {other:?}"),
    }
}

struct Sim {
    ce: CopyEngine,
    dev: Dev,
    mem: Arena,
    out: Out,
    rri_off: usize,
    rri_paddr: u64,
}

macro_rules! p {
    ($s:expr, $($a:tt)*) => { writeln!($s.out.borrow_mut(), $($a)*).unwrap() };
}

const RING_ARENA: usize = 0x10_0000;

impl Sim {
    fn new() -> Sim {
        let out: Out = Rc::new(RefCell::new(String::new()));
        Sim {
            ce: CopyEngine::new(),
            dev: Dev {
                regs: vec![0; 0x80_0000 / 4],
                out: out.clone(),
            },
            mem: Arena(vec![0; RING_ARENA]),
            out,
            rri_off: 0,
            rri_paddr: 0,
        }
    }

    fn rri_set(&mut self, id: usize, v: u32) {
        let off = self.rri_off + 4 * id;
        self.mem.write32(off, v);
    }

    fn dump_pipe(&self, id: usize) {
        let p = self.ce.pipe(id).unwrap();
        let mut s = format!(
            "STATE ce={} ctrl={:06x} attr={} src_sz_max={}",
            id, p.ctrl_addr, p.attr_flags, p.src_sz_max
        );
        if let Some(r) = &p.src_ring {
            write!(
                s,
                " src[n={} sw={} wr={} hw={} base={:x}]",
                r.nentries(),
                r.sw_index,
                r.write_index,
                r.hw_index,
                r.bus_base()
            )
            .unwrap();
        }
        if let Some(r) = &p.dest_ring {
            write!(
                s,
                " dst[n={} sw={} wr={} base={:x}]",
                r.nentries(),
                r.sw_index,
                r.write_index,
                r.bus_base()
            )
            .unwrap();
        }
        self.out.borrow_mut().push_str(&s);
        self.out.borrow_mut().push('\n');
    }

    fn dump_desc(&mut self, tag: &str, id: usize, ring_is_src: bool, idx: u32) {
        let off = {
            let p = self.ce.pipe(id).unwrap();
            let r = if ring_is_src {
                p.src_ring.as_ref().unwrap()
            } else {
                p.dest_ring.as_ref().unwrap()
            };
            r.desc_offset(idx)
        };
        let mut s = String::new();
        for b in &self.mem.0[off..off + 16] {
            write!(s, "{b:02x}").unwrap();
        }
        p!(self, "DESC {tag} ce={id} idx={idx} {s}");
    }

    fn send(&mut self, id: usize, ctx: u64, addr: u64, n: u32, tid: u32, flags: u32) -> i32 {
        errno(
            self.ce
                .send(&mut self.dev, &mut self.mem, id, ctx, addr, n, tid, flags),
        )
    }

    fn post(&mut self, id: usize, ctx: u64, paddr: u64) -> i32 {
        errno(
            self.ce
                .rx_post_buf(&mut self.dev, &mut self.mem, id, ctx, paddr),
        )
    }

    fn completed_send(&mut self, id: usize) -> (i32, u64) {
        match self
            .ce
            .completed_send_next(&mut self.dev, &mut self.mem, id)
        {
            Ok(c) => (0, c.unwrap_or(0)),
            Err(e) => (err_no(e), 99),
        }
    }

    fn cb_lines(&self, id: usize, s: ce::Service) {
        let a = &HOST_CE_CONFIG[id];
        if s.run_recv_cb {
            p!(self, "CB {} ce={}", a.recv_cb.unwrap().name(), id);
        }
        if s.run_send_cb {
            p!(self, "CB {} ce={}", a.send_cb.unwrap().name(), id);
        }
    }

    fn preset_regs(&mut self) {
        for id in 0..12u32 {
            let b = (0x24_0000 + 0x1000 * id) as usize;
            let r = &mut self.dev.regs;
            r[(b + 0x18) / 4] = 0x0001_2345 + id;
            r[(b + 0x2c) / 4] = 0x2;
            r[(b + 0x34) / 4] = 0x3ff;
            r[(b + 0x10) / 4] = 0x1f;
            r[(b + 0x4c) / 4] = 0xa5a5_5a5a;
            r[(b + 0x50) / 4] = 0x5a5a_a5a5;
            r[(b + 0x3c) / 4] = 2 + id;
            r[(b + 0x40) / 4] = 7;
            r[(b + 0x44) / 4] = 2 + id;
            r[(b + 0x48) / 4] = 7;
        }
    }

    fn init_all(&mut self) {
        let mut bus_next: u64 = 0x2_0000_0000;
        let mut host_next: usize = 0;
        let mut alloc = |size: usize| {
            let bus = bus_next;
            bus_next += 0x10000;
            let host = host_next;
            host_next += (size + 15) & !15;
            RingPlacement {
                host_offset: host,
                bus_addr: bus,
            }
        };
        self.preset_regs();
        p!(self, "== alloc_rri");
        let rri = alloc(CE_COUNT * 4);
        self.rri_off = rri.host_offset;
        self.rri_paddr = rri.bus_addr;
        self.ce
            .alloc_rri(&mut self.dev, rri.host_offset, rri.bus_addr);
        for id in 0..12usize {
            self.rri_set(id, (7 << 16) | (2 + id as u32));
        }
        p!(self, "== alloc_pipe");
        for (id, &a) in HOST_CE_CONFIG.iter().enumerate() {
            let src = (a.src_nentries != 0).then(|| {
                alloc(ce::ring_mem_size(
                    ce::ring_entries(a.src_nentries).unwrap() as usize
                ))
            });
            let dst = (a.dest_nentries != 0).then(|| {
                alloc(ce::ring_mem_size(
                    ce::ring_entries(a.dest_nentries).unwrap() as usize,
                ))
            });
            let r = errno(self.ce.alloc_pipe(id, &a, src, dst));
            p!(self, "alloc ce={id} ret={r}");
        }
        p!(self, "== init_pipe");
        for (id, &a) in HOST_CE_CONFIG.iter().enumerate() {
            let r = errno(self.ce.init_pipe(&mut self.dev, &mut self.mem, id, &a));
            p!(self, "init ce={id} ret={r}");
            self.dump_pipe(id);
        }
    }
}

#[test]
fn replay_matches_linux_ce_c() {
    let mut s = Sim::new();
    p!(s, "rri_paddr {:x}", 0u64);
    s.init_all();
    p!(s, "rri.paddr {:x}", s.rri_paddr);
    p!(s, "== enable_interrupts");
    s.ce.enable_interrupts(&mut s.dev);

    // ---- send on CE3
    p!(s, "== send_ce3");
    {
        let t: [(u64, u32, u32, u32); 5] = [
            (0x1_2345_6780, 100, 0x001, 0),
            (0x7_ffff_f000, 2048, 0xfff, ce::SEND_FLAG_BYTE_SWAP),
            (0x2_0000_0040, 64, 0x12, ce::SEND_FLAG_GATHER),
            (
                0x0_0000_1000,
                1,
                0,
                ce::SEND_FLAG_GATHER | ce::SEND_FLAG_BYTE_SWAP,
            ),
            (0x3_0000_0008, 7, 5, 0),
        ];
        for (i, (addr, n, id, flags)) in t.iter().enumerate() {
            let w = s.ce.pipe(3).unwrap().src_ring.as_ref().unwrap().write_index;
            let r = s.send(3, 100 + i as u64, *addr, *n, *id, *flags);
            p!(s, "send ret={r}");
            s.dump_desc("src", 3, true, w);
        }
        s.dump_pipe(3);
        s.dev.regs[(0x24_0000 + 0x1000 * 3 + 0x44) / 4] = 8;
        p!(s, "-- completed_send (srri=8)");
        loop {
            let (r, ctx) = s.completed_send(3);
            p!(s, "completed_send ret={r} ctx={ctx}");
            if r != 0 {
                break;
            }
        }
        s.dev.regs[(0x24_0000 + 0x1000 * 3 + 0x44) / 4] = 0xffff_ffff;
        {
            let r = s.ce.src_ring_mut(3).unwrap();
            r.hw_index = r.sw_index;
        }
        let (r, _) = s.completed_send(3);
        p!(s, "completed_send srri=ffffffff ret={r}");
        s.dump_pipe(3);
    }

    // ---- send on CE4
    p!(s, "== send_ce4");
    {
        for i in 0..3u32 {
            let w = s.ce.pipe(4).unwrap().src_ring.as_ref().unwrap().write_index;
            let r = s.send(
                4,
                200 + u64::from(i),
                0x1_0000_0000 + 0x100 * u64::from(i),
                50 + i,
                i,
                0,
            );
            p!(s, "send ret={r}");
            s.dump_desc("src", 4, true, w);
        }
        s.dump_pipe(4);
        s.rri_set(4, 0x0007_0000 | 9);
        loop {
            let (r, ctx) = s.completed_send(4);
            p!(s, "completed_send ret={r} ctx={ctx}");
            if r != 0 {
                break;
            }
        }
        s.dump_pipe(4);
    }

    // ---- full source ring
    p!(s, "== send_full_ce0");
    {
        let mut ok = 0;
        for i in 0..20u32 {
            let r = s.send(0, u64::from(i), 0x1000 + u64::from(i), 4, i, 0);
            if r == 0 {
                ok += 1;
            } else {
                p!(s, "send {i} ret={r}");
            }
        }
        p!(s, "sent_ok={ok}");
        s.dump_pipe(0);
        s.rri_set(0, 8);
        for i in 0..12u32 {
            let r = s.send(0, 30 + u64::from(i), 0x2000 + u64::from(i), 4, i, 0);
            if r != 0 {
                p!(s, "send {i} ret={r}");
            }
        }
        s.dump_pipe(0);
        s.dump_desc("src", 0, true, 15);
        s.dump_desc("src", 0, true, 0);
    }

    // ---- receive on CE2
    p!(s, "== recv_ce2");
    {
        let first =
            s.ce.pipe(2)
                .unwrap()
                .dest_ring
                .as_ref()
                .unwrap()
                .write_index;
        for i in 0..5u64 {
            let r = s.post(2, 300 + i, 0x1_8000_0000 + 0x800 * i);
            p!(s, "post ret={r}");
            let w =
                s.ce.pipe(2)
                    .unwrap()
                    .dest_ring
                    .as_ref()
                    .unwrap()
                    .write_index;
            s.dump_desc("dst", 2, false, (w + 63) & 63);
        }
        let off = |s: &Sim, idx: u32| {
            s.ce.pipe(2)
                .unwrap()
                .dest_ring
                .as_ref()
                .unwrap()
                .desc_offset(idx)
        };
        let (o0, o1, o2) = (
            off(&s, first),
            off(&s, (first + 1) & 63),
            off(&s, (first + 2) & 63),
        );
        s.mem.write16(o0 + 8, 60);
        s.mem.write16(o1 + 8, 1500);
        s.mem.write16(o2 + 8, 0xffff);
        loop {
            match s.ce.completed_recv_next(&mut s.mem, 2) {
                Ok((c, n)) => p!(s, "completed_recv ret=0 ctx={} nbytes={n}", c.unwrap_or(0)),
                Err(e) => {
                    p!(s, "completed_recv ret={} ctx=99 nbytes=0", err_no(e));
                    break;
                }
            }
        }
        s.dump_pipe(2);
        for _round in 0..3 {
            let mut posted = 0u64;
            loop {
                let r = s.post(2, 1000 + posted, 0x1_9000_0000 + 0x40 * posted);
                if r != 0 {
                    p!(s, "post stop ret={r} after {posted}");
                    break;
                }
                posted += 1;
            }
            s.dump_pipe(2);
            let sw = s.ce.pipe(2).unwrap().dest_ring.as_ref().unwrap().sw_index;
            for k in 0..posted {
                let o = off(&s, (sw + k as u32) & 63);
                s.mem.write16(o + 8, (100 + k) as u16);
            }
            let mut k = 0;
            while let Ok((_c, _n)) = s.ce.completed_recv_next(&mut s.mem, 2) {
                k += 1;
            }
            p!(s, "drained={k} last_ctx=0");
            s.dump_pipe(2);
        }
        p!(s, "-- rx_update_write_idx");
        s.dev.regs[(0x24_0000 + 0x1000 * 2 + 0x40) / 4] = 60;
        s.ce.rx_update_write_idx(&mut s.dev, 2, 3).unwrap();
        s.dump_pipe(2);
        s.ce.rx_update_write_idx(&mut s.dev, 2, 10).unwrap();
        s.dump_pipe(2);
    }

    // ---- interrupts
    p!(s, "== service");
    for id in [1usize, 3, 6] {
        let svc = s.ce.per_engine_service(&mut s.dev, id).unwrap();
        s.cb_lines(id, svc);
    }
    s.dev.regs[hw::CE_WRAPPER_BASE / 4] = (1 << 12) | (1 << 15) | (1 << 23);
    {
        let out = s.out.clone();
        let (ce, dev) = (&mut s.ce, &mut s.dev);
        ce.service_any(dev, |id, svc| {
            let a = &HOST_CE_CONFIG[id];
            if svc.run_recv_cb {
                writeln!(
                    out.borrow_mut(),
                    "CB {} ce={}",
                    a.recv_cb.unwrap().name(),
                    id
                )
                .unwrap();
            }
            if svc.run_send_cb {
                writeln!(
                    out.borrow_mut(),
                    "CB {} ce={}",
                    a.send_cb.unwrap().name(),
                    id
                )
                .unwrap();
            }
        });
    }
    p!(s, "== interrupts");
    s.ce.disable_interrupt(&mut s.dev, 1).unwrap();
    s.ce.disable_interrupt(&mut s.dev, 4).unwrap();
    s.ce.enable_interrupt(&mut s.dev, 4).unwrap();
    s.ce.enable_interrupt(&mut s.dev, 1).unwrap();
    s.ce.enable_interrupt(&mut s.dev, 6).unwrap();
    s.ce.disable_interrupts(&mut s.dev);
    p!(s, "== deinit");
    for id in 0..12 {
        s.ce.deinit_pipe(&mut s.dev, id).unwrap();
    }

    let got = s.out.borrow().clone();
    let want = include_str!("fixtures/ce_oracle.txt");
    let (g, w): (Vec<&str>, Vec<&str>) = (got.lines().collect(), want.lines().collect());
    for (i, (a, b)) in g.iter().zip(w.iter()).enumerate() {
        assert_eq!(a, b, "first difference at line {}", i + 1);
    }
    assert_eq!(g.len(), w.len(), "trace length");
}
