//! A RAM stand-in for the regions, shared by the tests and by `examples/model-image.rs` (the QEMU stand-in). The layout
//! is built from the C structs of `smem.c` and `cmd-db.c`, not from the module under test. No real SMEM or command DB
//! dump exists: the laptop's Linux was wiped before any was taken.

#![allow(dead_code, clippy::type_complexity)]

use std::cell::RefCell;

use lucina_qcom_soc::io::RegIo;

/// Little-endian words; a read inside `forbidden` panics, which proves the code stays out of a range.
pub struct Ram {
    pub bytes: RefCell<Vec<u8>>,
    pub forbidden: Vec<(u32, u32)>,
    pub writes: RefCell<u32>,
}

impl Ram {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self {
            bytes: RefCell::new(bytes),
            forbidden: Vec::new(),
            writes: RefCell::new(0),
        }
    }
    pub fn get(&self, off: u32) -> u32 {
        let b = self.bytes.borrow();
        let o = off as usize;
        u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
    }
    pub fn put(&self, off: u32, v: u32) {
        let mut b = self.bytes.borrow_mut();
        let o = off as usize;
        b[o..o + 4].copy_from_slice(&v.to_le_bytes());
    }
}

impl RegIo for Ram {
    fn read32(&self, off: u32) -> u32 {
        assert_eq!(off % 4, 0, "unaligned read at {off:#x}");
        for &(start, end) in &self.forbidden {
            assert!(
                off < start || off >= end,
                "read of a forbidden range at {off:#x}"
            );
        }
        if off as usize + 4 > self.bytes.borrow().len() {
            return 0xdead_beef;
        }
        self.get(off)
    }
    fn write32(&self, off: u32, value: u32) {
        assert_eq!(off % 4, 0, "unaligned write at {off:#x}");
        *self.writes.borrow_mut() += 1;
        self.put(off, value);
    }
}

pub const SMEM_SIZE: u32 = 0x20_0000;
pub const GLOBAL_OFF: u32 = 0x1000;
pub const GLOBAL_SIZE: u32 = 0x8000;
pub const MODEM_OFF: u32 = 0x9000;
pub const MODEM_SIZE: u32 = 0x8000;
/// A partition of two other processors, which we must never open.
pub const OTHER_OFF: u32 = 0x11000;
pub const OTHER_SIZE: u32 = 0x4000;

fn put32(b: &mut [u8], o: u32, v: u32) {
    b[o as usize..o as usize + 4].copy_from_slice(&v.to_le_bytes());
}

fn put_part_header(b: &mut [u8], off: u32, size: u32, h0: u16, h1: u16, free: u32, cached: u32) {
    b[off as usize..off as usize + 4].copy_from_slice(b"$PRT");
    put32(b, off + 4, h0 as u32 | (h1 as u32) << 16);
    put32(b, off + 8, size);
    put32(b, off + 12, free);
    put32(b, off + 16, cached);
}

/// An uncached entry: `struct smem_private_entry` then data. Returns the next free offset in the partition.
fn put_uncached(b: &mut [u8], part: u32, at: u32, item: u16, data: &[u8]) -> u32 {
    let asize = (data.len() as u32 + 7) & !7;
    let e = part + at;
    put32(b, e, 0xa5a5 | (item as u32) << 16);
    put32(b, e + 4, asize);
    put32(b, e + 8, asize - data.len() as u32);
    b[(e + 16) as usize..(e + 16) as usize + data.len()].copy_from_slice(data);
    at + 16 + asize
}

/// A cached entry at the partition's end, growing down. Returns the new cached free offset.
fn put_cached(b: &mut [u8], part: u32, cached: u32, line: u32, item: u16, data: &[u8]) -> u32 {
    let asize = (data.len() as u32 + 7) & !7;
    let step = (16 + line - 1) & !(line - 1);
    let e = part + cached - step;
    put32(b, e, 0xa5a5 | (item as u32) << 16);
    put32(b, e + 4, asize);
    put32(b, e + 8, asize - data.len() as u32);
    let item_at = e - asize;
    b[item_at as usize..item_at as usize + data.len()].copy_from_slice(data);
    cached - step - asize
}

fn smp2p_item(version: u32, valid: &[(&str, u32)]) -> Vec<u8> {
    let mut v = vec![0u8; 20 + 16 * 20];
    put32(&mut v, 0, 0x504d_5324);
    put32(&mut v, 4, version | 1 << 8);
    put32(&mut v, 8, 1);
    put32(&mut v, 12, 16 | (valid.len() as u32) << 16);
    for (i, (name, value)) in valid.iter().enumerate() {
        let e = 20 + i as u32 * 20;
        v[e as usize..e as usize + name.len()].copy_from_slice(name.as_bytes());
        put32(&mut v, e + 16, *value);
    }
    v
}

/// The state the kernel should find at boot if nothing started the modem: firmware made the partitions, and the
/// modem's own items do not exist. `running` adds the items a booted modem would have, for the other report path.
pub fn smem_image(running: bool) -> Vec<u8> {
    let mut b = vec![0u8; SMEM_SIZE as usize];
    put32(&mut b, 64 + 7 * 4, 12 << 16);
    put32(&mut b, 64 + 128, 1);
    let t = SMEM_SIZE - 0x1000;
    b[t as usize..t as usize + 4].copy_from_slice(b"$TOC");
    put32(&mut b, t + 4, 1);
    put32(&mut b, t + 8, 3);
    let rows = [
        (GLOBAL_OFF, GLOBAL_SIZE, 0xfffeu16, 0xfffeu16, 0x40u32),
        (MODEM_OFF, MODEM_SIZE, 0, 1, 0x40),
        (OTHER_OFF, OTHER_SIZE, 1, 2, 0x40),
    ];
    for (i, r) in rows.iter().enumerate() {
        let e = t + 32 + i as u32 * 48;
        put32(&mut b, e, r.0);
        put32(&mut b, e + 4, r.1);
        put32(&mut b, e + 12, r.2 as u32 | (r.3 as u32) << 16);
        put32(&mut b, e + 16, r.4);
    }
    let info = t + 32 + 3 * 48;
    b[info as usize..info as usize + 4].copy_from_slice(b"SIII");
    put32(&mut b, info + 16, 512);

    // The global partition: an item firmware leaves, in the uncached list.
    let free = put_uncached(&mut b, GLOBAL_OFF, 32, 137, b"socinfo-stand-in");
    put_part_header(
        &mut b,
        GLOBAL_OFF,
        GLOBAL_SIZE,
        0xfffe,
        0xfffe,
        free,
        GLOBAL_SIZE,
    );

    // The modem's partition.
    let mut free = 32;
    let mut cached = MODEM_SIZE;
    if running {
        free = put_uncached(
            &mut b,
            MODEM_OFF,
            free,
            428,
            &smp2p_item(2, &[("master-kernel", 0)]),
        );
        free = put_uncached(
            &mut b,
            MODEM_OFF,
            free,
            435,
            &smp2p_item(2, &[("slave-kernel", 0b110)]),
        );
        free = put_uncached(&mut b, MODEM_OFF, free, 478, &[0; 32]);
        cached = put_cached(
            &mut b,
            MODEM_OFF,
            cached,
            0x40,
            421,
            b"err_fatal.c:42:stand-in\0",
        );
    }
    put_part_header(&mut b, MODEM_OFF, MODEM_SIZE, 0, 1, free, cached);

    // Garbage in the other partition, to make a stray read visible in a report too.
    for w in (OTHER_OFF..OTHER_OFF + OTHER_SIZE).step_by(4) {
        put32(&mut b, w, 0xbadc_0ffe);
    }
    b
}

/// A command DB with one VRM and two ARC slaves, after `struct cmd_db_header` and `struct entry_header`.
pub fn cmd_db_image() -> Vec<u8> {
    let mut b = vec![0u8; 0x2_0000];
    put32(&mut b, 0, 1);
    b[4..8].copy_from_slice(&[0xdb, 0x30, 0x03, 0x0c]);
    let rows: [(u16, &[(&str, u32, &[u8])]); 2] = [
        (
            3,
            &[
                ("cx.lvl", 0x30010, &[16, 0, 48, 0, 64, 0, 128, 0]),
                ("mx.lvl", 0x30000, &[16, 0, 128, 0]),
                ("mss.lvl", 0x30040, &[16, 0, 128, 0]),
            ],
        ),
        (
            4,
            &[
                ("xo.lvl", 0x30000, &[]),
                ("ldoa9", 0x40900, &[]),
                ("ldoc1", 0x40a00, &[]),
                ("ldoc2", 0x40a10, &[]),
                ("ldoc10", 0x40a20, &[]),
                ("ldoc11", 0x40a30, &[]),
            ],
        ),
    ];
    let header = 8 + 8 * 16 + 8;
    let mut head_at = 0u32;
    let mut data_at = 0x1000u32;
    for (i, (id, entries)) in rows.iter().enumerate() {
        let r = 8 + i as u32 * 16;
        put32(&mut b, r, *id as u32 | head_at << 16);
        put32(&mut b, r + 4, data_at | (entries.len() as u32) << 16);
        let mut own_data = 0u32;
        for (j, (name, addr, data)) in entries.iter().enumerate() {
            let e = header + head_at + j as u32 * 24;
            b[e as usize..e as usize + name.len()].copy_from_slice(name.as_bytes());
            put32(&mut b, e + 16, *addr);
            put32(&mut b, e + 20, data.len() as u32 | own_data << 16);
            let d = header + data_at + own_data;
            b[d as usize..d as usize + data.len()].copy_from_slice(data);
            own_data += data.len() as u32;
        }
        head_at += entries.len() as u32 * 24;
        data_at += own_data;
    }
    b
}

/// The AOSS message RAM as the always-on processor shows it before anyone opens the link.
pub fn qmp_image() -> Vec<u8> {
    let mut b = vec![0u8; 0x400];
    put32(&mut b, 0, 0x4d41_494c);
    put32(&mut b, 4, 1);
    put32(&mut b, 0x0c, 0xffff); // ucore link up
    put32(&mut b, 0x34, 0x100); // mcore mailbox size
    put32(&mut b, 0x38, 0x100); // mcore mailbox offset
    b
}
