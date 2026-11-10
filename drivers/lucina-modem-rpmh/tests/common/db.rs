//! A command DB image in the format of Linux's `cmd-db.c`, built for the tests.
//!
//! **The addresses and ARC levels here are invented.** The real values have never been dumped from the machine; they
//! are unknown until the kernel's `lucina.wifi=report` prints them. The format (header, per accelerator headers,
//! 24-byte entries, aux data) is Linux's `struct cmd_db_header`, `rsc_hdr` and `entry_header`.

use lucina_qcom_soc::cmd_db::CmdDb;
use lucina_qcom_soc::io::RegIo;

pub const HEADER: usize = 8 + 8 * 16 + 8;

/// The image as 32-bit words, read-only like the real region.
pub struct ImageIo(pub Vec<u32>);

impl RegIo for ImageIo {
    fn read32(&self, off: u32) -> u32 {
        self.0[off as usize / 4]
    }
    fn write32(&self, off: u32, _value: u32) {
        panic!("write to the command DB at {off:#x}");
    }
}

pub struct Entry {
    pub name: &'static str,
    pub addr: u32,
    pub aux: Vec<u8>,
}

pub fn entry(name: &'static str, addr: u32, aux: Vec<u8>) -> Entry {
    Entry { name, addr, aux }
}

/// Little-endian bytes of a `u16` table.
pub fn u16s(v: &[u16]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

/// `slaves`: (slave id, entries). Returns the bytes of the image.
pub fn image(slaves: &[(u32, Vec<Entry>)]) -> Vec<u8> {
    let mut hdr = vec![0u8; HEADER];
    hdr[0..4].copy_from_slice(&1u32.to_le_bytes());
    hdr[4..8].copy_from_slice(&[0xdb, 0x30, 0x03, 0x0c]);
    let mut entries = Vec::new();
    let mut data = Vec::new();
    let mut layout = Vec::new();
    for (id, list) in slaves {
        let header_offset = entries.len() as u32;
        let data_offset = data.len() as u32;
        for e in list {
            let mut rec = [0u8; 24];
            rec[..e.name.len()].copy_from_slice(e.name.as_bytes());
            rec[16..20].copy_from_slice(&e.addr.to_le_bytes());
            rec[20..22].copy_from_slice(&(e.aux.len() as u16).to_le_bytes());
            let off = data.len() as u32 - data_offset;
            rec[22..24].copy_from_slice(&(off as u16).to_le_bytes());
            entries.extend_from_slice(&rec);
            data.extend_from_slice(&e.aux);
        }
        layout.push((*id, header_offset, data_offset, list.len() as u32));
    }
    let entries_len = entries.len() as u32;
    for (i, (id, header_offset, data_offset, cnt)) in layout.iter().enumerate() {
        let at = 8 + 16 * i;
        let w0 = id | (header_offset << 16);
        // data follows all the entries, so its offset (relative to the end of the header) is shifted by their size
        let w1 = (data_offset + entries_len) | (cnt << 16);
        hdr[at..at + 4].copy_from_slice(&w0.to_le_bytes());
        hdr[at + 4..at + 8].copy_from_slice(&w1.to_le_bytes());
    }
    hdr.extend_from_slice(&entries);
    hdr.extend_from_slice(&data);
    while !hdr.len().is_multiple_of(4) {
        hdr.push(0);
    }
    hdr
}

pub fn open(bytes: &[u8]) -> CmdDb<ImageIo> {
    let words = bytes
        .chunks(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    CmdDb::probe(ImageIo(words), bytes.len() as u32).expect("image has the magic")
}

/// The ARC level table every invented domain gets: 8 levels, padded with zeros to 16 entries.
pub const STD_LEVELS: [u16; 16] = [16, 48, 64, 128, 192, 256, 320, 384, 0, 0, 0, 0, 0, 0, 0, 0];

pub const CX: u32 = 0x30000;
pub const MX: u32 = 0x30010;
pub const MSS: u32 = 0x30020;
pub const XO: u32 = 0x30030;
pub const LCX: u32 = 0x30040;
pub const LMX: u32 = 0x30050;
pub const LDOA9: u32 = 0x40900;
pub const LDOC1: u32 = 0x40c10;
pub const LDOC2: u32 = 0x40c20;
pub const LDOC10: u32 = 0x40d00;
pub const LDOC11: u32 = 0x40d10;

/// A stand-in command DB with the ARC resources and LDOs the plans need.
pub fn standin() -> Vec<u8> {
    image(&[
        (
            3,
            vec![
                entry("cx.lvl", CX, u16s(&STD_LEVELS)),
                entry("mx.lvl", MX, u16s(&STD_LEVELS)),
                entry("mss.lvl", MSS, u16s(&STD_LEVELS)),
                entry("lcx.lvl", LCX, u16s(&STD_LEVELS)),
                entry("lmx.lvl", LMX, u16s(&STD_LEVELS)),
                entry("xo.lvl", XO, u16s(&[1, 2, 3, 0])),
            ],
        ),
        (
            4,
            vec![
                entry("ldoa9", LDOA9, vec![]),
                entry("ldoc1", LDOC1, vec![]),
                entry("ldoc2", LDOC2, vec![]),
                entry("ldoc10", LDOC10, vec![]),
                entry("ldoc11", LDOC11, vec![]),
            ],
        ),
    ])
}
