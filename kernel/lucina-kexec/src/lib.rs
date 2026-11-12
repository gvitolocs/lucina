// SPDX-License-Identifier: MPL-2.0

//! What the kernel decides before it reloads itself (`kexec_file_load`), free of hardware access so that it can be
//! tested on the host: the checks on a new arm64 kernel image, where things may be placed, and the device tree that
//! the new kernel gets.
//!
//! LucinaOS kernels cannot relocate themselves: an image runs only at the address it is linked at,
//! `DRAM base + text offset`. Its arm64 header therefore records the DRAM base it was built for in a reserved field
//! (kernel patch 0204, [`DRAM_BASE_OFFSET`]), and the running kernel takes only images built for its own base.

#![no_std]
#![deny(unsafe_code)]

pub mod pages;
pub mod segments;

extern crate alloc;

use alloc::vec::Vec;
use core::ops::Range;

/// Offset in the arm64 image header of the reserved field `res3`, which LucinaOS kernels set to their DRAM base.
pub const DRAM_BASE_OFFSET: usize = 0x28;

const ARM64_MAGIC: &[u8; 4] = b"ARM\x64";
const ARM64_MAGIC_OFFSET: usize = 0x38;
const ARM64_HEADER_LEN: usize = 64;

/// The largest device tree that the boot code maps (it refuses more).
const MAX_DTB: usize = 1 << 21;

const BLOCK_2M: u64 = 1 << 21;
const GIB: u64 = 1 << 30;

/// Why an image or a device tree was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// No arm64 image magic, or the file is shorter than the header.
    NotArm64Image,
    /// The header has no DRAM base: not built by LucinaOS with patch 0204.
    NoDramBase,
    /// Built for another DRAM base than the running kernel's, so it would not run here.
    OtherDramBase { image: u64, running: u64 },
    /// The header's effective size is smaller than the file, or the numbers overflow.
    BadSize,
    /// The device tree is malformed or uses an unsupported version.
    BadDeviceTree,
}

/// The fields of an arm64 image header that matter here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Image {
    /// Where the image runs, from its header: `dram_base + text_offset`.
    pub load_addr: u64,
    /// The bytes it occupies when running, BSS and boot stack included.
    pub effective_size: u64,
}

impl Image {
    /// Checks a whole image file for a kernel that runs at `running_dram_base`.
    pub fn parse(file: &[u8], running_dram_base: u64) -> Result<Image, Error> {
        Self::parse_header(file, file.len() as u64, running_dram_base)
    }

    /// As [`Image::parse`], from the start of the file (at least its 64-byte header) and the file's length, so that the
    /// kernel need not hold the whole file in one buffer (patch 1055).
    pub fn parse_header(
        header: &[u8],
        file_len: u64,
        running_dram_base: u64,
    ) -> Result<Image, Error> {
        let file = header;
        if file.len() < ARM64_HEADER_LEN
            || &file[ARM64_MAGIC_OFFSET..ARM64_MAGIC_OFFSET + 4] != ARM64_MAGIC
        {
            return Err(Error::NotArm64Image);
        }
        let word = |at: usize| u64::from_le_bytes(file[at..at + 8].try_into().unwrap());
        let (text_offset, effective_size, dram_base) = (word(8), word(16), word(DRAM_BASE_OFFSET));
        if dram_base == 0 {
            return Err(Error::NoDramBase);
        }
        if dram_base != running_dram_base {
            return Err(Error::OtherDramBase {
                image: dram_base,
                running: running_dram_base,
            });
        }
        if effective_size < file_len {
            return Err(Error::BadSize);
        }
        let load_addr = dram_base.checked_add(text_offset).ok_or(Error::BadSize)?;
        load_addr
            .checked_add(effective_size)
            .ok_or(Error::BadSize)?;
        Ok(Image {
            load_addr,
            effective_size,
        })
    }

    /// The physical addresses the running image occupies. Nothing may be staged here: the image is copied over it
    /// with the MMU off.
    pub fn window(&self) -> Range<u64> {
        self.load_addr..self.load_addr + self.effective_size
    }
}

fn overlaps(a: &Range<u64>, b: &Range<u64>) -> bool {
    a.start < b.end && b.start < a.end
}

/// Whether a buffer (the image's source, the initramfs, the relocation code) may stay at `range`.
pub fn clear_of_window(range: &Range<u64>, window: &Range<u64>) -> bool {
    !overlaps(range, window)
}

/// Whether the device tree may be at `range`.
///
/// The boot code of the new kernel maps it with 2 MiB tables that may not share a 2 MiB region with the kernel image,
/// and refuses one that crosses a GiB (`die_device_tree` in `bsp_boot.S`).
pub fn device_tree_fits(range: &Range<u64>, window: &Range<u64>) -> bool {
    if range.is_empty() || range.end - range.start > MAX_DTB as u64 {
        return false;
    }
    let round_down = |x: u64| x & !(BLOCK_2M - 1);
    let round_up = |x: u64| x.next_multiple_of(BLOCK_2M);
    let tree_blocks = round_down(range.start)..round_up(range.end);
    let image_blocks = round_down(window.start)..round_up(window.end);
    range.start / GIB == (range.end - 1) / GIB && !overlaps(&tree_blocks, &image_blocks)
}

const FDT_MAGIC: u32 = 0xd00d_feed;
const FDT_BEGIN_NODE: u32 = 1;
const FDT_END_NODE: u32 = 2;
const FDT_PROP: u32 = 3;
const FDT_NOP: u32 = 4;
const FDT_END: u32 = 9;
const FDT_HEADER_LEN: usize = 40;

const INITRD_START: &str = "linux,initrd-start";
const INITRD_END: &str = "linux,initrd-end";
const BOOTARGS: &str = "bootargs";
const RNG_SEED: &str = "rng-seed";
const RNG_SOURCE: &str = "lucina,rng-source";

/// Returns `dtb` with `/chosen` describing the new initramfs and, if given, a new command line; everything else, the
/// framebuffer below `/chosen` and the memory nodes included, stays. `initrd` is `start..end`; `None` removes the
/// properties. `bootargs` has no terminating NUL; `None` keeps the command line. `rng_seed` is the seed Linux's `kexec` hands
/// the next kernel; one already in the tree never goes along, since it has been used. The reload properties
/// ([`RELOAD_ROOT`], [`RELOAD_DATA`]) of the running tree never go along either: see [`edit_chosen_with`].
pub fn edit_chosen(
    dtb: &[u8],
    initrd: Option<Range<u64>>,
    bootargs: Option<&[u8]>,
    rng_seed: Option<&[u8]>,
) -> Result<Vec<u8>, Error> {
    edit_chosen_with(dtb, initrd, bootargs, rng_seed, &ReloadWords::default())
}

/// `/chosen` property of a reload that boots the stream's own user space: `overlay` or `initramfs`, NUL-terminated.
pub const RELOAD_ROOT: &str = "lucina,reload-root";
/// `/chosen` property of such a reload: what becomes of `/data`, `none`, `ro` or `rw`, NUL-terminated.
pub const RELOAD_DATA: &str = "lucina,reload-data";
/// The command-line words `lucina-reload --stream-root=<m> --data=<m>` passes to the running kernel's `kexec_file_load`.
pub const RELOAD_ROOT_WORD: &str = "lucina.reload.root=";
pub const RELOAD_DATA_WORD: &str = "lucina.reload.data=";

/// The reload words taken out of a kexec command line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReloadWords {
    /// `overlay` or `initramfs`; `None` without a valid word.
    pub root: Option<&'static str>,
    /// `none`, `ro` or `rw`; `None` without a word. Only set when `root` is.
    pub data: Option<&'static str>,
    /// A word was there with a value outside its set (or a data word without a root word): dropped, nothing set.
    pub refused: bool,
}

/// Splits the reload words off a kexec command line: returns the line without any `lucina.reload.root=`/`lucina.reload.data=`
/// word among the kernel's arguments (before ` -- `; the init's arguments stay as they are) and what they asked for. The words never
/// reach the new kernel's command line: only the `/chosen` properties [`edit_chosen_with`] writes from them do, so that a command
/// line from anywhere else (BOOT.CFG) cannot start a test root. A data word without a root word, or a value outside its set, sets
/// nothing (`refused`). A missing data word with a root word means `none`. The last word of a kind counts.
pub fn reload_words(line: &[u8]) -> (Vec<u8>, ReloadWords) {
    let (kernel, init) = split_init(line);
    let mut out: Vec<u8> = Vec::with_capacity(line.len());
    let (mut root, mut data, mut refused) = (None::<&[u8]>, None::<&[u8]>, false);
    for word in kernel.split(|b| *b == b' ').filter(|w| !w.is_empty()) {
        if let Some(v) = word.strip_prefix(RELOAD_ROOT_WORD.as_bytes()) {
            root = Some(v);
        } else if let Some(v) = word.strip_prefix(RELOAD_DATA_WORD.as_bytes()) {
            data = Some(v);
        } else {
            if !out.is_empty() {
                out.push(b' ');
            }
            out.extend_from_slice(word);
        }
    }
    if let Some(init) = init {
        if !out.is_empty() {
            out.push(b' ');
        }
        out.extend_from_slice(b"--");
        if !init.is_empty() {
            out.push(b' ');
            out.extend_from_slice(init);
        }
    }
    let root_value = match root {
        Some(b"overlay") => Some("overlay"),
        Some(b"initramfs") => Some("initramfs"),
        Some(_) => {
            refused = true;
            None
        }
        None => None,
    };
    let data_value = match (root_value, data) {
        (Some(_), None | Some(b"none")) => Some("none"),
        (Some(_), Some(b"ro")) => Some("ro"),
        (Some(_), Some(b"rw")) => Some("rw"),
        (Some(_), Some(_)) => {
            refused = true;
            None
        }
        (None, Some(_)) => {
            refused = true;
            None
        }
        (None, None) => None,
    };
    // A bad data value refuses the whole request: a test boot with /data other than asked is worse than none.
    let words = if root_value.is_some() && data_value.is_some() {
        ReloadWords {
            root: root_value,
            data: data_value,
            refused,
        }
    } else {
        ReloadWords {
            root: None,
            data: None,
            refused,
        }
    };
    (out, words)
}

/// The kernel's arguments and, after ` -- ` (or a leading `-- `, or a trailing ` --`), the init's.
fn split_init(line: &[u8]) -> (&[u8], Option<&[u8]>) {
    let line = line.strip_suffix(b"\0").unwrap_or(line);
    if line == b"--" {
        return (b"", Some(b""));
    }
    if let Some(rest) = line.strip_prefix(b"-- ") {
        return (b"", Some(rest));
    }
    if let Some(i) = line.windows(4).position(|w| w == b" -- ") {
        return (&line[..i], Some(&line[i + 4..]));
    }
    if let Some(kernel) = line.strip_suffix(b" --") {
        return (kernel, Some(b""));
    }
    (line, None)
}

/// [`edit_chosen`], and `/chosen` gets [`RELOAD_ROOT`] and [`RELOAD_DATA`] from `reload` when it asks for a test root. The
/// running tree's own reload properties are always left out, so that they exist only in the tree of the one reload that asked.
pub fn edit_chosen_with(
    dtb: &[u8],
    initrd: Option<Range<u64>>,
    bootargs: Option<&[u8]>,
    rng_seed: Option<&[u8]>,
    reload: &ReloadWords,
) -> Result<Vec<u8>, Error> {
    let bad = Error::BadDeviceTree;
    let be32 = |at: usize| -> Result<u32, Error> {
        let bytes = dtb.get(at..at + 4).ok_or(bad)?;
        Ok(u32::from_be_bytes(bytes.try_into().unwrap()))
    };
    if be32(0)? != FDT_MAGIC {
        return Err(bad);
    }
    let (total, off_struct, off_strings, off_rsv) = (
        be32(4)? as usize,
        be32(8)? as usize,
        be32(12)? as usize,
        be32(16)? as usize,
    );
    let (version, last_comp, cpuid, size_strings, size_struct) = (
        be32(20)?,
        be32(24)?,
        be32(28)?,
        be32(32)? as usize,
        be32(36)? as usize,
    );
    if version < 17 || total > dtb.len() {
        return Err(bad);
    }
    let structure = dtb
        .get(off_struct..off_struct.checked_add(size_struct).ok_or(bad)?)
        .ok_or(bad)?;
    let strings = dtb
        .get(off_strings..off_strings.checked_add(size_strings).ok_or(bad)?)
        .ok_or(bad)?;

    // The reservation map ends with an all-zero entry.
    let mut rsv_end = off_rsv;
    loop {
        let entry = dtb.get(rsv_end..rsv_end + 16).ok_or(bad)?;
        rsv_end += 16;
        if entry.iter().all(|b| *b == 0) {
            break;
        }
    }
    let rsv = &dtb[off_rsv..rsv_end];

    let mut new_strings = strings.to_vec();
    let mut props: Vec<(u32, Vec<u8>)> = Vec::new();
    if let Some(r) = &initrd {
        props.push((
            string_offset(&mut new_strings, INITRD_START),
            r.start.to_be_bytes().to_vec(),
        ));
        props.push((
            string_offset(&mut new_strings, INITRD_END),
            r.end.to_be_bytes().to_vec(),
        ));
    }
    if let Some(args) = bootargs {
        let mut value = args.to_vec();
        value.push(0);
        props.push((string_offset(&mut new_strings, BOOTARGS), value));
    }
    if let Some(seed) = rng_seed {
        props.push((string_offset(&mut new_strings, RNG_SEED), seed.to_vec()));
        props.push((
            string_offset(&mut new_strings, RNG_SOURCE),
            b"the kernel before this reload\0".to_vec(),
        ));
    }
    if let (Some(root), Some(data)) = (reload.root, reload.data) {
        for (name, value) in [(RELOAD_ROOT, root), (RELOAD_DATA, data)] {
            let mut v = value.as_bytes().to_vec();
            v.push(0);
            props.push((string_offset(&mut new_strings, name), v));
        }
    }
    let put_props = |out: &mut Vec<u8>| {
        for (name, value) in &props {
            put32(out, FDT_PROP);
            put32(out, value.len() as u32);
            put32(out, *name);
            out.extend_from_slice(value);
            pad4(out);
        }
    };

    let mut out = Vec::with_capacity(structure.len() + 128);
    let (mut at, mut depth, mut seen_chosen, mut in_chosen) = (0usize, 0usize, false, false);
    loop {
        let token = u32::from_be_bytes(structure.get(at..at + 4).ok_or(bad)?.try_into().unwrap());
        at += 4;
        match token {
            FDT_BEGIN_NODE => {
                let rest = structure.get(at..).ok_or(bad)?;
                let len = rest.iter().position(|b| *b == 0).ok_or(bad)?;
                let name = &rest[..len];
                put32(&mut out, FDT_BEGIN_NODE);
                out.extend_from_slice(name);
                out.push(0);
                pad4(&mut out);
                at += (len + 1).next_multiple_of(4);
                depth += 1;
                // Properties come before child nodes, so the new ones go first.
                if depth == 2 && name == b"chosen" {
                    seen_chosen = true;
                    in_chosen = true;
                    put_props(&mut out);
                }
            }
            FDT_END_NODE => {
                if depth == 0 {
                    return Err(bad);
                }
                if depth == 1 && !seen_chosen {
                    put32(&mut out, FDT_BEGIN_NODE);
                    out.extend_from_slice(b"chosen\0");
                    pad4(&mut out);
                    put_props(&mut out);
                    put32(&mut out, FDT_END_NODE);
                    seen_chosen = true;
                }
                if depth == 2 {
                    in_chosen = false;
                }
                depth -= 1;
                put32(&mut out, FDT_END_NODE);
            }
            FDT_PROP => {
                let len = be32(off_struct + at)? as usize;
                let name_off = be32(off_struct + at + 4)? as usize;
                let end = at
                    .checked_add(8)
                    .and_then(|x| x.checked_add(len.next_multiple_of(4)))
                    .ok_or(bad)?;
                let whole = structure.get(at + 8..end).ok_or(bad)?;
                let name = strings.get(name_off..).ok_or(bad)?;
                let name = &name[..name.iter().position(|b| *b == 0).ok_or(bad)?];
                let replaced = in_chosen
                    && depth == 2
                    && (name == INITRD_START.as_bytes()
                        || name == INITRD_END.as_bytes()
                        || name == RNG_SEED.as_bytes()
                        || name == RNG_SOURCE.as_bytes()
                        || name == RELOAD_ROOT.as_bytes()
                        || name == RELOAD_DATA.as_bytes()
                        || (bootargs.is_some() && name == BOOTARGS.as_bytes()));
                if !replaced {
                    put32(&mut out, FDT_PROP);
                    put32(&mut out, len as u32);
                    put32(&mut out, name_off as u32);
                    out.extend_from_slice(whole);
                }
                at = end;
            }
            FDT_NOP => {}
            FDT_END => {
                if depth != 0 {
                    return Err(bad);
                }
                put32(&mut out, FDT_END);
                break;
            }
            _ => return Err(bad),
        }
    }

    let off_new_rsv = FDT_HEADER_LEN;
    let off_new_struct = off_new_rsv + rsv.len();
    let off_new_strings = off_new_struct + out.len();
    let new_total = off_new_strings + new_strings.len();
    let mut tree = Vec::with_capacity(new_total);
    for field in [
        FDT_MAGIC,
        new_total as u32,
        off_new_struct as u32,
        off_new_strings as u32,
        off_new_rsv as u32,
        version,
        last_comp,
        cpuid,
        new_strings.len() as u32,
        out.len() as u32,
    ] {
        put32(&mut tree, field);
    }
    tree.extend_from_slice(rsv);
    tree.extend_from_slice(&out);
    tree.extend_from_slice(&new_strings);
    Ok(tree)
}

/// The offset of `name` as a whole string in the strings block, added if missing.
fn string_offset(strings: &mut Vec<u8>, name: &str) -> u32 {
    let mut at = 0;
    while at < strings.len() {
        let end = at
            + strings[at..]
                .iter()
                .position(|b| *b == 0)
                .unwrap_or(strings.len() - at);
        if &strings[at..end] == name.as_bytes() {
            return at as u32;
        }
        at = end + 1;
    }
    let offset = strings.len() as u32;
    strings.extend_from_slice(name.as_bytes());
    strings.push(0);
    offset
}

fn put32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn pad4(out: &mut Vec<u8>) {
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use alloc::{collections::BTreeMap, string::String, vec};

    use super::*;

    const QEMU_DTB: &[u8] = include_bytes!("../../../kernel/lucina-fdt/tests/data/qemu-virt-gicv3-8g.dtb");

    /// Reads a device tree into `path -> {property -> value}`, checking the structure on the way.
    fn read(dtb: &[u8]) -> BTreeMap<String, BTreeMap<String, Vec<u8>>> {
        let be = |at: usize| u32::from_be_bytes(dtb[at..at + 4].try_into().unwrap()) as usize;
        assert_eq!(be(0) as u32, FDT_MAGIC);
        assert_eq!(be(4), dtb.len(), "totalsize");
        let (s, strings) = (be(8), be(12));
        let (mut at, mut path, mut tree) = (0, vec![String::new()], BTreeMap::new());
        path.clear();
        loop {
            let token = be(s + at) as u32;
            at += 4;
            match token {
                FDT_BEGIN_NODE => {
                    let name = dtb[s + at..].iter().position(|b| *b == 0).unwrap();
                    path.push(String::from_utf8(dtb[s + at..s + at + name].to_vec()).unwrap());
                    at += (name + 1).next_multiple_of(4);
                    tree.entry(path.join("/")).or_insert_with(BTreeMap::new);
                }
                FDT_END_NODE => {
                    path.pop();
                }
                FDT_PROP => {
                    let (len, name) = (be(s + at), be(s + at + 4));
                    let n = dtb[strings + name..].iter().position(|b| *b == 0).unwrap();
                    let key = String::from_utf8(dtb[strings + name..strings + name + n].to_vec())
                        .unwrap();
                    let value = dtb[s + at + 8..s + at + 8 + len].to_vec();
                    let previous = tree
                        .get_mut(&path.join("/"))
                        .unwrap()
                        .insert(key.clone(), value);
                    assert!(previous.is_none(), "{key} twice");
                    at += 8 + len.next_multiple_of(4);
                }
                FDT_NOP => {}
                FDT_END => return tree,
                other => panic!("token {other}"),
            }
        }
    }

    #[test]
    fn real_tree_keeps_everything_else() {
        let before = read(QEMU_DTB);
        let edited = edit_chosen(
            QEMU_DTB,
            Some(0x1_2000_0000..0x1_2100_0000),
            Some(b"console=tty0 -- sh -l"),
            None,
        )
        .unwrap();
        let mut after = read(&edited);
        let chosen = after.get_mut("/chosen").unwrap();
        assert_eq!(
            chosen.remove(INITRD_START).unwrap(),
            0x1_2000_0000u64.to_be_bytes()
        );
        assert_eq!(
            chosen.remove(INITRD_END).unwrap(),
            0x1_2100_0000u64.to_be_bytes()
        );
        assert_eq!(chosen.remove(BOOTARGS).unwrap(), b"console=tty0 -- sh -l\0");
        let mut before = before;
        let chosen = before.get_mut("/chosen").unwrap();
        chosen.remove(INITRD_START);
        chosen.remove(INITRD_END);
        chosen.remove(BOOTARGS);
        // QEMU gives its tree a seed: used, so it does not go along.
        chosen.remove(RNG_SEED);
        assert_eq!(after, before);
    }

    #[test]
    fn editing_twice_replaces_and_none_keeps_or_removes() {
        let once = edit_chosen(QEMU_DTB, Some(1..2), Some(b"first"), None).unwrap();
        let twice = edit_chosen(&once, Some(3..4), None, None).unwrap();
        let tree = read(&twice);
        let chosen = &tree["/chosen"];
        assert_eq!(chosen[INITRD_START], 3u64.to_be_bytes());
        assert_eq!(chosen[BOOTARGS], b"first\0", "None keeps the command line");
        let without = edit_chosen(&twice, None, None, None).unwrap();
        assert!(!read(&without)["/chosen"].contains_key(INITRD_END));
        // The strings do not grow with each edit.
        assert_eq!(
            twice.len(),
            edit_chosen(&twice, Some(3..4), None, None).unwrap().len()
        );
    }

    /// A minimal tree: a root with `child`, with or without a `/chosen`, version 17.
    fn small_tree(chosen: bool) -> Vec<u8> {
        let mut s = Vec::new();
        put32(&mut s, FDT_BEGIN_NODE);
        s.extend_from_slice(&[0, 0, 0, 0]);
        put32(&mut s, FDT_NOP);
        put32(&mut s, FDT_BEGIN_NODE);
        s.extend_from_slice(b"child\0\0\0");
        put32(&mut s, FDT_PROP);
        put32(&mut s, 3);
        put32(&mut s, 0);
        s.extend_from_slice(b"abc\0");
        put32(&mut s, FDT_END_NODE);
        if chosen {
            put32(&mut s, FDT_BEGIN_NODE);
            s.extend_from_slice(b"chosen\0\0");
            // A child's bootargs is not /chosen's.
            put32(&mut s, FDT_BEGIN_NODE);
            s.extend_from_slice(b"fb\0\0");
            put32(&mut s, FDT_PROP);
            put32(&mut s, 1);
            put32(&mut s, 2);
            s.extend_from_slice(b"x\0\0\0");
            put32(&mut s, FDT_END_NODE);
            put32(&mut s, FDT_END_NODE);
        }
        put32(&mut s, FDT_END_NODE);
        put32(&mut s, FDT_END);
        let strings = b"p\0bootargs\0";
        let mut t = Vec::new();
        let (off_struct, off_strings) = (40 + 16, 40 + 16 + s.len());
        for f in [
            FDT_MAGIC,
            (off_strings + strings.len()) as u32,
            off_struct as u32,
            off_strings as u32,
            40,
            17,
            16,
            0,
            strings.len() as u32,
            s.len() as u32,
        ] {
            put32(&mut t, f);
        }
        t.extend_from_slice(&[0; 16]);
        t.extend_from_slice(&s);
        t.extend_from_slice(strings);
        t
    }

    #[test]
    fn chosen_is_created_and_child_properties_stay() {
        let tree = read(&edit_chosen(&small_tree(false), Some(5..6), None, None).unwrap());
        assert_eq!(tree["/chosen"][INITRD_END], 6u64.to_be_bytes());
        assert_eq!(tree["/child"]["p"], b"abc");

        let tree = read(&edit_chosen(&small_tree(true), None, Some(b"new"), None).unwrap());
        assert_eq!(tree["/chosen"][BOOTARGS], b"new\0");
        assert_eq!(
            tree["/chosen/fb"]["bootargs"], b"x",
            "only /chosen's own property is replaced"
        );
    }

    #[test]
    fn bad_trees_are_refused() {
        assert_eq!(
            edit_chosen(b"short", None, None, None),
            Err(Error::BadDeviceTree)
        );
        let good = small_tree(true);
        let mut wrong_magic = good.clone();
        wrong_magic[0] = 0;
        assert_eq!(
            edit_chosen(&wrong_magic, None, None, None),
            Err(Error::BadDeviceTree)
        );
        let mut old_version = good.clone();
        old_version[20..24].copy_from_slice(&16u32.to_be_bytes());
        assert_eq!(
            edit_chosen(&old_version, None, None, None),
            Err(Error::BadDeviceTree)
        );
        for len in 0..good.len() {
            assert_eq!(
                edit_chosen(&good[..len], None, None, None),
                Err(Error::BadDeviceTree),
                "cut at {len}"
            );
        }
        // A property that claims to run past the structure block.
        let mut long_prop = good.clone();
        let at = long_prop
            .windows(4)
            .position(|w| w == FDT_PROP.to_be_bytes())
            .unwrap();
        long_prop[at + 4..at + 8].copy_from_slice(&0x7fff_ffffu32.to_be_bytes());
        assert_eq!(
            edit_chosen(&long_prop, None, None, None),
            Err(Error::BadDeviceTree)
        );
    }

    fn image(text_offset: u64, size: u64, base: u64) -> Vec<u8> {
        let mut f = vec![0u8; 4096];
        f[8..16].copy_from_slice(&text_offset.to_le_bytes());
        f[16..24].copy_from_slice(&size.to_le_bytes());
        f[DRAM_BASE_OFFSET..DRAM_BASE_OFFSET + 8].copy_from_slice(&base.to_le_bytes());
        f[ARM64_MAGIC_OFFSET..ARM64_MAGIC_OFFSET + 4].copy_from_slice(ARM64_MAGIC);
        f
    }

    #[test]
    fn a_used_seed_never_goes_along_and_a_new_one_does() {
        assert!(read(QEMU_DTB)["/chosen"].contains_key(RNG_SEED));
        let without = read(&edit_chosen(QEMU_DTB, None, None, None).unwrap());
        assert!(!without["/chosen"].contains_key(RNG_SEED));
        let with = read(&edit_chosen(QEMU_DTB, None, None, Some(&[7; 32])).unwrap());
        assert_eq!(with["/chosen"][RNG_SEED], [7; 32]);
        let again = read(
            &edit_chosen(
                &edit_chosen(QEMU_DTB, None, None, Some(&[7; 32])).unwrap(),
                None,
                None,
                Some(&[8; 32]),
            )
            .unwrap(),
        );
        assert_eq!(again["/chosen"][RNG_SEED], [8; 32]);
    }

    #[test]
    fn parse_header_agrees_with_parse() {
        for (text_offset, size, base, len) in [
            (0x20_0000, 0x100_0000, 0xa000_0000, 0x80_0000),
            (0, 64, 0xa000_0000, 64),
        ] {
            let mut file = image(text_offset, size, base);
            file.resize(len, 0);
            assert_eq!(
                Image::parse(&file, 0xa000_0000),
                Image::parse_header(&file[..64], len as u64, 0xa000_0000)
            );
        }
        let file = image(0, 100, 0xa000_0000);
        assert_eq!(
            Image::parse_header(&file[..64], 101, 0xa000_0000),
            Err(Error::BadSize)
        );
        assert_eq!(
            Image::parse_header(&file[..63], 64, 0xa000_0000),
            Err(Error::NotArm64Image)
        );
    }

    #[test]
    fn image_checks() {
        const BASE: u64 = 0xa000_0000;
        let good = Image::parse(&image(0x20_0000, 0x60_0000, BASE), BASE).unwrap();
        assert_eq!(good.window(), 0xa020_0000..0xa080_0000);
        assert_eq!(Image::parse(&[0; 10], BASE), Err(Error::NotArm64Image));
        let mut no_magic = image(0x20_0000, 0x60_0000, BASE);
        no_magic[ARM64_MAGIC_OFFSET] = 0;
        assert_eq!(Image::parse(&no_magic, BASE), Err(Error::NotArm64Image));
        assert_eq!(
            Image::parse(&image(0x20_0000, 0x60_0000, 0), BASE),
            Err(Error::NoDramBase)
        );
        assert_eq!(
            Image::parse(&image(0x20_0000, 0x60_0000, 0x4000_0000), BASE),
            Err(Error::OtherDramBase {
                image: 0x4000_0000,
                running: BASE
            })
        );
        assert_eq!(
            Image::parse(&image(0x20_0000, 100, BASE), BASE),
            Err(Error::BadSize)
        );
        assert_eq!(
            Image::parse(&image(u64::MAX, 0x60_0000, BASE), BASE),
            Err(Error::BadSize)
        );
        assert_eq!(
            Image::parse(&image(0x20_0000, u64::MAX - 1, BASE), BASE),
            Err(Error::BadSize)
        );
    }

    #[test]
    fn placement() {
        let window = 0xa020_0000..0xa080_0000;
        assert!(clear_of_window(&(0xa080_0000..0xa090_0000), &window));
        assert!(clear_of_window(&(0xa010_0000..0xa020_0000), &window));
        assert!(!clear_of_window(&(0xa07f_f000..0xa080_1000), &window));

        // Directly after the image, in its own 2 MiB region.
        assert!(device_tree_fits(&(0xa080_0000..0xa080_1000), &window));
        // Before the image, sharing the first 2 MiB block with it.
        assert!(!device_tree_fits(&(0xa03f_0000..0xa03f_1000), &window));
        assert!(!device_tree_fits(&(0xa01f_f000..0xa020_1000), &window));
        // In the block below the image.
        assert!(device_tree_fits(&(0xa010_0000..0xa010_1000), &window));
        // A window that ends inside a block keeps the rest of that block away.
        assert!(!device_tree_fits(
            &(0xa090_0000..0xa090_1000),
            &(0xa020_0000..0xa080_0001)
        ));
        // Across a GiB.
        assert!(!device_tree_fits(&(0xbfff_f000..0xc000_1000), &window));
        // Empty or too large.
        assert!(!device_tree_fits(&(0xb000_0000..0xb000_0000), &window));
        assert!(!device_tree_fits(&(0xb000_0000..0xb020_1000), &window));
    }

    fn words(line: &str) -> (String, ReloadWords) {
        let (rest, w) = reload_words(line.as_bytes());
        (String::from_utf8(rest).unwrap(), w)
    }

    #[test]
    fn the_reload_words_leave_the_line_and_become_a_request() {
        let (rest, w) = words(
            "a=1 lucina.reload.root=overlay b lucina.reload.data=ro -- /etc/x lucina.reload.root=initramfs",
        );
        assert_eq!(rest, "a=1 b -- /etc/x lucina.reload.root=initramfs");
        assert_eq!(
            w,
            ReloadWords {
                root: Some("overlay"),
                data: Some("ro"),
                refused: false
            }
        );
    }

    #[test]
    fn a_root_word_alone_means_data_none() {
        let (rest, w) = words("lucina.reload.root=initramfs");
        assert_eq!(rest, "");
        assert_eq!(
            w,
            ReloadWords {
                root: Some("initramfs"),
                data: Some("none"),
                refused: false
            }
        );
    }

    #[test]
    fn bad_values_and_a_data_word_alone_set_nothing() {
        for line in [
            "lucina.reload.root=ram",
            "lucina.reload.root=overlay lucina.reload.data=yes",
            "lucina.reload.data=rw",
        ] {
            let (rest, w) = words(line);
            assert_eq!(rest, "", "{line}");
            assert_eq!(
                w,
                ReloadWords {
                    root: None,
                    data: None,
                    refused: true
                },
                "{line}"
            );
        }
    }

    #[test]
    fn a_line_without_words_is_unchanged() {
        for line in ["a b -- c d", "-- c", "a --", "a", ""] {
            let (rest, w) = words(line);
            assert_eq!(rest, line);
            assert_eq!(w, ReloadWords::default());
        }
    }

    #[test]
    fn the_request_becomes_chosen_properties() {
        let reload = ReloadWords {
            root: Some("overlay"),
            data: Some("rw"),
            refused: false,
        };
        let tree = edit_chosen_with(QEMU_DTB, None, Some(b"x"), None, &reload).unwrap();
        let t = read(&tree);
        assert_eq!(t["/chosen"][RELOAD_ROOT], b"overlay\0");
        assert_eq!(t["/chosen"][RELOAD_DATA], b"rw\0");
        assert_eq!(t["/chosen"][BOOTARGS], b"x\0");
    }

    #[test]
    fn a_running_trees_reload_properties_never_go_along() {
        let reload = ReloadWords {
            root: Some("initramfs"),
            data: Some("ro"),
            refused: false,
        };
        let test_boot = edit_chosen_with(QEMU_DTB, None, Some(b"x"), None, &reload).unwrap();
        // The next reload from that boot asks for nothing: no property is left.
        let next = edit_chosen(&test_boot, None, Some(b"y"), None).unwrap();
        let t = read(&next);
        assert!(!t["/chosen"].contains_key(RELOAD_ROOT));
        assert!(!t["/chosen"].contains_key(RELOAD_DATA));
        // And one that asks again has each once (`read` refuses a property twice).
        let again = edit_chosen_with(&test_boot, None, None, None, &reload).unwrap();
        assert_eq!(read(&again)["/chosen"][RELOAD_ROOT], b"initramfs\0");
    }
}
