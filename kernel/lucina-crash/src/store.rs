//! The saved log: a region of RAM that the kernel does not use as RAM and that survives a warm reset, as Linux's ramoops
//! does (`fs/pstore/ram.c`). Nothing here is written to a disk.
//!
//! ```text
//! 0        header, 3 copies: magic, version, geometry, checksum. Never changes once written.
//! 128      state, 2 protected copies: boot counter, crashes in a row, how the boot ended, active console, checksum.
//! 4096     crash records, each in a slot of its own: 2 protected copies of its header, then its text in blocks.
//! ...      two consoles, one for this boot and one kept from the last boot, each with 2 protected copies of its header, then blocks.
//! ```
//!
//! **This RAM loses bits.** On the Galaxy Book Go a firmware reset flips 1.57% of them, evenly (2026-11-02, `pattern-reset`; DRAM goes
//! unrefreshed while firmware restarts it). So wrong bits are normal here, and Linux's documentation warns of the same. All but the header
//! is therefore in blocks of a Reed-Solomon code ([`crate::ecc`]) that correct a third of their bytes, a good deal more than the 12% a reset
//! changes. A protected copy is one such block; there are two because the parity of a block rewritten in place does not survive a reset in
//! the middle of it. Each boot writes back what it corrected in the records, in the background and one block at a time, so that flips do not
//! add up over the resets a record stays for and the boot does not wait.
//! A block with too many wrong bytes is read as it is, and the record is marked damaged: a report with wrong letters beats none.
//!
//! What the header should say is known from the region's size, so that one a few bits from it is taken as ours and rewritten. A header
//! further than that means the region is new, was overwritten or has another size: it starts over. A state that fails its check costs the
//! counters and nothing else. A record header that fails costs that record. All of this is in the tests.

use crate::{
    ecc::{self, Encoder, MAX_DATA as BLOCK_DATA, PARITY},
    layout,
    report::Kind,
};

// The layout's arithmetic lives in `crate::layout` (Verus); these must agree with it.
const _: () = assert!(
    FIRST_ZONE == layout::FIRST_ZONE && RECORDS == layout::RECORDS && MIN_SIZE == layout::MIN_SIZE
);

const MAGIC: &[u8; 8] = b"LUCINAPS";
/// Format 4 (patch 1213): smaller consoles, a 64 KiB larger tail (`crate::layout`). Format 3 regions are read once and upgraded.
pub const VERSION: u32 = 4;
const RECORD_MAGIC: u32 = u32::from_le_bytes(*b"LCRS");
const CONSOLE_MAGIC: u32 = u32::from_le_bytes(*b"LCON");

const HEADER_LEN: usize = 32;
const STATE_AT: usize = 128;
const STATE_LEN: usize = 24;
const FIRST_ZONE: usize = 4096;
/// Copies of the header.
const COPIES: usize = 3;
/// Copies of what is rewritten in place.
const PROTECTED_COPIES: usize = 2;
/// A block of text: its data, then its parity.
const BLOCK: usize = BLOCK_DATA + PARITY;
const RECORD_HEADER: usize = 32;
/// The start of a slot: the copies of its header.
const RECORD_PREFIX: usize = protected_len(RECORD_HEADER);
const CONSOLE_FIELDS: usize = 20;
/// The start of a console zone: the copies of its magic, boot, count and checksum.
const CONSOLE_HEADER: usize = protected_len(CONSOLE_FIELDS);
/// How many bits of the header, read by majority, may be wrong for it to count as ours, of its 256. The header of a region of another size
/// differs in its fields and in about half of its checksum, some 20 bits.
const HEADER_TOLERANCE: u32 = 4;

/// How many crash records fit. The oldest goes first.
pub const RECORDS: usize = 8;

/// The smallest region that holds anything useful.
pub const MIN_SIZE: usize = 64 * 1024;

/// How many bytes of the console an unexplained reset's record keeps, at most.
const UNEXPLAINED_TAIL: usize = 48 * 1024;

/// The epoch block's tag (patch 1370).
const EPOCH_TAG: u32 = u32::from_le_bytes(*b"EPCH");
/// Its data: the tag, the id, a checksum over both.
const EPOCH_FIELDS: usize = 16;
// The epoch block sits after the state, and its three copies end below the first record.
const _: () = assert!(
    layout::EPOCH_AT >= STATE_AT + protected_len(STATE_LEN)
        && layout::EPOCH_COPY >= EPOCH_FIELDS + PARITY
        && layout::EPOCH_AT + layout::EPOCH_COPIES * layout::EPOCH_COPY <= FIRST_ZONE
);

/// The longest reason a program can give for a restart.
const MAX_REASON: usize = 256;

/// A region named on the kernel command line, `<address>+<size>`, both in hexadecimal with an optional `0x`.
///
/// Returns its start and end. It must be whole pages, as big as [`MIN_SIZE`] and not wrap around.
pub fn parse_region(arg: &str) -> Option<(usize, usize)> {
    let (start, size) = arg.split_once('+')?;
    let hex = |text: &str| usize::from_str_radix(text.strip_prefix("0x").unwrap_or(text), 16).ok();
    let (start, size) = (hex(start)?, hex(size)?);
    let end = start.checked_add(size)?;
    (start % 4096 == 0 && size % 4096 == 0 && size >= MIN_SIZE).then_some((start, end))
}

/// Whether a region named on the command line may be used: all of it RAM, and none of it anything else. A loader put the
/// kernel's initramfs and device tree wherever it liked, and writing over them would break this boot.
pub fn region_is_free(
    region: (usize, usize),
    mut ram: impl Iterator<Item = (usize, usize)>,
    mut in_use: impl Iterator<Item = (usize, usize)>,
) -> Result<(), &'static str> {
    if !ram.any(|(start, end)| start <= region.0 && region.1 <= end) {
        return Err("it is not all in one RAM bank");
    }
    if in_use.any(|(start, end)| start < region.1 && region.0 < end) {
        return Err("it overlaps the kernel, its initramfs, its device tree or a firmware region");
    }
    Ok(())
}

/// What a region held that has no valid header, for the log: wiped, overwritten, or partly ours.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ForeignContents {
    pub pages: usize,
    pub zero_pages: usize,
    pub ones_pages: usize,
    pub other_pages: usize,
    pub first: [u8; 32],
    /// How many bits of the header, read by majority, differ from what a good one would be.
    pub header_bit_errors: u32,
    /// How many console zones and record slots still start with our marks, so that the header alone was hit.
    pub console_marks: usize,
    pub record_marks: usize,
}

/// Says what a region held, if its header is not valid. A valid one gives `None`, as does a region too small to use.
pub fn foreign_contents<M: Mem>(mem: &M) -> Option<ForeignContents> {
    let geometry = Geometry::for_size(mem.size())?;
    let errors = majority_header_errors(mem, &geometry);
    if errors <= HEADER_TOLERANCE {
        return None;
    }
    let mut header = [0u8; HEADER_LEN];
    mem.read(0, &mut header);
    let mut found = ForeignContents {
        pages: 0,
        zero_pages: 0,
        ones_pages: 0,
        other_pages: 0,
        first: header,
        header_bit_errors: errors,
        console_marks: 0,
        record_marks: 0,
    };
    let mut page = [0u8; 4096];
    for at in (0..mem.size()).step_by(page.len()) {
        mem.read(at, &mut page);
        found.pages += 1;
        if page.iter().all(|&b| b == 0) {
            found.zero_pages += 1;
        } else if page.iter().all(|&b| b == 0xff) {
            found.ones_pages += 1;
        } else {
            found.other_pages += 1;
        }
    }
    let mark_at = |at: usize| {
        let mut word = [0u8; 4];
        mem.read(at, &mut word);
        u32::from_le_bytes(word)
    };
    found.console_marks = (0..2)
        .filter(|&zone| mark_at(geometry.console_at(zone)) == CONSOLE_MAGIC)
        .count();
    found.record_marks = (0..RECORDS)
        .filter(|&slot| mark_at(geometry.record_at(slot)) == RECORD_MAGIC)
        .count();
    Some(found)
}

const PATTERN_MAGIC: &[u8; 16] = b"LUCINA PATTERN 1";

fn pattern_byte(offset: usize) -> u8 {
    // A mix of the offset, so that no two nearby bytes agree and half of the bits are ones.
    let x = (offset as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    (x >> 56) as u8 ^ (x >> 32) as u8
}

/// Fills the region with a known pattern, for finding out what a reset does to RAM. It replaces what was saved.
pub fn fill_pattern<M: Mem>(mem: &mut M) {
    let mut chunk = [0u8; 256];
    let mut at = PATTERN_MAGIC.len();
    let size = mem.size();
    while at < size {
        let n = chunk.len().min(size - at);
        for (i, b) in chunk[..n].iter_mut().enumerate() {
            *b = pattern_byte(at + i);
        }
        mem.write(at, &chunk[..n]);
        at += n;
    }
    // The mark last: a reset in the middle leaves no pattern.
    mem.write(0, PATTERN_MAGIC);
    mem.sync();
}

/// What a reset did to the pattern.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PatternResult {
    pub bytes: usize,
    pub bit_errors: u64,
    /// Whole 4 KiB pages that differ in more than a quarter of their bytes: overwritten by somebody, not decayed.
    pub overwritten_pages: usize,
    pub first_error_at: Option<usize>,
    /// Bits that went from 1 to 0, of the errors. Decay goes mostly one way per cell type; a wipe goes all ways.
    pub ones_lost: u64,
}

/// Compares the region with the pattern, if it holds one. Bit errors in the mark itself are forgiven by a majority of its bytes.
pub fn check_pattern<M: Mem>(mem: &M) -> Option<PatternResult> {
    let mut mark = [0u8; 16];
    mem.read(0, &mut mark);
    let same = mark
        .iter()
        .zip(PATTERN_MAGIC)
        .filter(|(a, b)| a == b)
        .count();
    if same < 12 {
        return None;
    }
    let mut result = PatternResult {
        bytes: mem.size(),
        bit_errors: 0,
        overwritten_pages: 0,
        first_error_at: None,
        ones_lost: 0,
    };
    let mut page = [0u8; 4096];
    let mut at = 0;
    while at < mem.size() {
        let n = page.len().min(mem.size() - at);
        mem.read(at, &mut page[..n]);
        let mut different = 0;
        for (i, &have) in page[..n].iter().enumerate() {
            let offset = at + i;
            let want = if offset < PATTERN_MAGIC.len() {
                PATTERN_MAGIC[offset]
            } else {
                pattern_byte(offset)
            };
            let diff = have ^ want;
            if diff != 0 {
                different += 1;
                result.bit_errors += u64::from(diff.count_ones());
                result.ones_lost += u64::from((diff & want).count_ones());
                result.first_error_at.get_or_insert(offset);
            }
        }
        if different > n / 4 {
            result.overwritten_pages += 1;
        }
        at += n;
    }
    Some(result)
}

/// The region's memory. The kernel's side maps it so that writes reach the RAM without the caches, which a reset drops.
pub trait Mem {
    fn size(&self) -> usize;
    fn read(&self, offset: usize, out: &mut [u8]);
    fn write(&mut self, offset: usize, data: &[u8]);
    /// Gets what was written out to the RAM, so that a reset right after this finds it there. It takes time, so a log line does it once.
    fn sync(&mut self) {}
}

/// CRC-32 (IEEE), by nibbles: small, and fast enough for a record per crash.
fn crc32_update(crc: u32, data: &[u8]) -> u32 {
    const TABLE: [u32; 16] = {
        let mut table = [0u32; 16];
        let mut i = 0;
        while i < 16 {
            let mut c = i as u32;
            let mut bit = 0;
            while bit < 4 {
                c = if c & 1 != 0 {
                    c >> 1 ^ 0xedb8_8320
                } else {
                    c >> 1
                };
                bit += 1;
            }
            table[i] = c;
            i += 1;
        }
        table
    };
    let mut c = crc;
    for &b in data {
        c = TABLE[((c ^ u32::from(b)) & 15) as usize] ^ c >> 4;
        c = TABLE[((c ^ u32::from(b >> 4)) & 15) as usize] ^ c >> 4;
    }
    c
}

/// The checksum of some pieces taken together.
pub fn crc32(parts: &[&[u8]]) -> u32 {
    !parts.iter().fold(!0, |crc, part| crc32_update(crc, part))
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    let mut word = [0; 4];
    word.copy_from_slice(&bytes[at..at + 4]);
    u32::from_le_bytes(word)
}

/// Reads `COPIES` copies, `stride` apart, as one, by the majority of each bit.
fn read_majority<M: Mem>(mem: &M, offset: usize, stride: usize, out: &mut [u8]) {
    let mut chunk = [[0u8; 64]; COPIES];
    let mut done = 0;
    while done < out.len() {
        let n = (out.len() - done).min(chunk[0].len());
        for (copy, bytes) in chunk.iter_mut().enumerate() {
            mem.read(offset + copy * stride + done, &mut bytes[..n]);
        }
        for i in 0..n {
            let (a, b, c) = (chunk[0][i], chunk[1][i], chunk[2][i]);
            out[done + i] = a & b | a & c | b & c;
        }
        done += n;
    }
}

/// The room that `len` bytes take when protected.
const fn protected_len(len: usize) -> usize {
    PROTECTED_COPIES * (len + PARITY)
}

/// Writes something small that is rewritten in place.
fn write_protected<M: Mem>(mem: &mut M, at: usize, data: &[u8]) {
    let parity = ecc::parity_of(data);
    for copy in 0..PROTECTED_COPIES {
        let base = at + copy * (data.len() + PARITY);
        mem.write(base, data);
        mem.write(base + data.len(), &parity);
    }
}

/// Reads what [`write_protected`] wrote: the first copy that decodes and that `valid` likes. `out` means nothing if neither does.
fn read_protected<M: Mem>(
    mem: &M,
    at: usize,
    out: &mut [u8],
    valid: impl Fn(&[u8]) -> bool,
) -> bool {
    let mut parity = [0u8; PARITY];
    for copy in 0..PROTECTED_COPIES {
        let base = at + copy * (out.len() + PARITY);
        mem.read(base, out);
        mem.read(base + out.len(), &mut parity);
        if ecc::decode(out, &mut parity).is_some() && valid(out) {
            return true;
        }
    }
    false
}

/// Reads the first `len` bytes of the block at `at`, corrected if it can be. Returns how many were wrong, `None` for a block
/// beyond repair, whose bytes are as they came.
fn read_block<M: Mem>(mem: &M, at: usize, len: usize, out: &mut [u8; BLOCK_DATA]) -> Option<usize> {
    let mut parity = [0u8; PARITY];
    mem.read(at, &mut out[..len]);
    mem.read(at + BLOCK_DATA, &mut parity);
    ecc::decode(&mut out[..len], &mut parity)
}

/// Text on its way into blocks.
struct TextWriter {
    base: usize,
    len: usize,
    encoder: Encoder,
}

impl TextWriter {
    fn new(base: usize) -> Self {
        Self {
            base,
            len: 0,
            encoder: Encoder::new(),
        }
    }

    fn push<M: Mem>(&mut self, mem: &mut M, mut text: &[u8]) {
        while !text.is_empty() {
            let (block, offset) = (self.len / BLOCK_DATA, self.len % BLOCK_DATA);
            let n = text.len().min(BLOCK_DATA - offset);
            mem.write(self.base + block * BLOCK + offset, &text[..n]);
            self.encoder.push(&text[..n]);
            self.len += n;
            text = &text[n..];
            if offset + n == BLOCK_DATA {
                mem.write(
                    self.base + block * BLOCK + BLOCK_DATA,
                    self.encoder.parity(),
                );
                self.encoder = Encoder::new();
            }
        }
    }

    /// Writes the parity of the last block, if it is not full.
    fn finish<M: Mem>(&mut self, mem: &mut M) {
        let (block, offset) = (self.len / BLOCK_DATA, self.len % BLOCK_DATA);
        if offset != 0 {
            mem.write(
                self.base + block * BLOCK + BLOCK_DATA,
                self.encoder.parity(),
            );
        }
    }
}

/// What a boot found in the records that were there.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Scrub {
    /// Wrong bytes corrected and written back.
    pub corrected: usize,
    /// Blocks with too many wrong bytes, left as they are.
    pub lost_blocks: usize,
}

/// A new epoch id from the words the hardware gave: the counter, the region's address and a random word, mixed (splitmix64)
/// with the size. Never 0, which stands for "no epoch yet".
fn new_epoch_id(seed: u64, size: usize) -> u64 {
    let mut x = seed ^ (size as u64).rotate_left(32);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^= x >> 31;
    x.max(1)
}

fn epoch_fields(id: u64) -> [u8; EPOCH_FIELDS] {
    let mut f = [0u8; EPOCH_FIELDS];
    f[..4].copy_from_slice(&EPOCH_TAG.to_le_bytes());
    f[4..12].copy_from_slice(&id.to_le_bytes());
    let crc = crc32(&[&f[..12]]);
    f[12..16].copy_from_slice(&crc.to_le_bytes());
    f
}

fn epoch_fields_ok(f: &[u8; EPOCH_FIELDS]) -> bool {
    u32_at(f, 0) == EPOCH_TAG && crc32(&[&f[..12]]) == u32_at(f, 12)
}

/// Writes three copies of the epoch block, each with its parity, so that a reset in the middle of one leaves the others.
fn write_epoch<M: Mem>(mem: &mut M, id: u64) {
    let fields = epoch_fields(id);
    let parity = ecc::parity_of(&fields);
    for copy in 0..layout::EPOCH_COPIES {
        let base = layout::EPOCH_AT + copy * layout::EPOCH_COPY;
        mem.write(base, &fields);
        mem.write(base + EPOCH_FIELDS, &parity);
    }
    mem.sync();
}

/// Reads the epoch block. A copy that its parity cannot correct, or whose tag or checksum is wrong, does not vote; the rest
/// vote bit by bit (as `mmio.rs` does) and the result must pass the checksum too. `Err` says whether any copy had the tag.
fn read_epoch_block<M: Mem>(mem: &M) -> Result<u64, EpochOrigin> {
    let mut good: [[u8; EPOCH_FIELDS]; layout::EPOCH_COPIES] =
        [[0; EPOCH_FIELDS]; layout::EPOCH_COPIES];
    let mut count = 0;
    let mut tagged = false;
    for copy in 0..layout::EPOCH_COPIES {
        let base = layout::EPOCH_AT + copy * layout::EPOCH_COPY;
        let mut fields = [0u8; EPOCH_FIELDS];
        let mut parity = [0u8; PARITY];
        mem.read(base, &mut fields);
        mem.read(base + EPOCH_FIELDS, &mut parity);
        tagged |= u32_at(&fields, 0) == EPOCH_TAG;
        if ecc::decode(&mut fields, &mut parity).is_some() && epoch_fields_ok(&fields) {
            good[count] = fields;
            count += 1;
        }
    }
    let voted = match count {
        0 => {
            return Err(if tagged {
                EpochOrigin::Damaged
            } else {
                EpochOrigin::NoBlock
            });
        }
        3 => core::array::from_fn(|i| {
            let (a, b, c) = (good[0][i], good[1][i], good[2][i]);
            a & b | a & c | b & c
        }),
        _ => good[0],
    };
    let fields = if epoch_fields_ok(&voted) {
        voted
    } else {
        good[0]
    };
    let mut id = [0u8; 8];
    id.copy_from_slice(&fields[4..12]);
    Ok(u64::from_le_bytes(id))
}

/// Why the store laid a region out afresh instead of using it (patch 1372), as far as a format-4 kernel can tell.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RestartCause {
    /// Every page was zero: nobody had used it (or RAM was wiped, which looks the same).
    FirstUse,
    /// Not our header: the pages by what they hold.
    NoHeader {
        zero_pages: usize,
        ones_pages: usize,
        other_pages: usize,
    },
    /// Our magic, another format: older than [`VERSION`] (2 or less), or newer.
    OtherFormat(u32),
    /// Our magic and a known format, but too many bits wrong after the vote.
    WrongBits(u32),
    /// Our header for a region of another size.
    SizeChanged,
    /// A format-3 header over an epoch block: a kernel from before patch 1213 laid out the region we had made.
    OldKernel(u32),
}

impl core::fmt::Display for RestartCause {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match *self {
            RestartCause::FirstUse => f.write_str("first use"),
            RestartCause::NoHeader {
                zero_pages,
                ones_pages,
                other_pages,
            } => write!(
                f,
                "no header: {zero_pages} pages zero, {ones_pages} all ones, {other_pages} other"
            ),
            RestartCause::OtherFormat(n) if n < VERSION => {
                write!(f, "header of format {n} from an older kernel")
            }
            RestartCause::OtherFormat(n) => write!(f, "header of format {n} from a newer kernel"),
            RestartCause::WrongBits(m) => {
                write!(f, "header with {m} wrong bits after the vote")
            }
            RestartCause::SizeChanged => f.write_str("region size changed"),
            RestartCause::OldKernel(n) => write!(
                f,
                "a kernel of format {n} laid it out again (a format {n} header over an epoch block)"
            ),
        }
    }
}

/// What a region that [`Store::attach`] refused held, for the log and for the first record of the new region.
fn classify_restart<M: Mem>(mem: &M) -> RestartCause {
    let mut h = [0u8; HEADER_LEN];
    read_majority(mem, 0, HEADER_LEN, &mut h);
    let magic_bits: u32 = h[..8]
        .iter()
        .zip(MAGIC)
        .map(|(a, b)| (a ^ b).count_ones())
        .sum();
    if magic_bits <= 8 {
        let version = u32_at(&h, 8);
        if version != 3 && version != VERSION {
            return RestartCause::OtherFormat(version);
        }
        if u32_at(&h, 12) as usize != mem.size() {
            return RestartCause::SizeChanged;
        }
        let bits = Geometry::for_size(mem.size()).map_or(0, |g| majority_header_errors(mem, &g));
        return RestartCause::WrongBits(bits);
    }
    match foreign_contents(mem) {
        Some(f) if f.zero_pages == f.pages => RestartCause::FirstUse,
        Some(f) => RestartCause::NoHeader {
            zero_pages: f.zero_pages,
            ones_pages: f.ones_pages,
            other_pages: f.other_pages,
        },
        None => RestartCause::FirstUse,
    }
}

/// How many bits differ between a header and what it should be.
fn header_errors(have: &[u8; HEADER_LEN], want: &[u8; HEADER_LEN]) -> u32 {
    have.iter()
        .zip(want)
        .map(|(have, want)| (have ^ want).count_ones())
        .sum()
}

/// How many bits of the header, read by majority, differ from what it should be.
fn majority_header_errors<M: Mem>(mem: &M, geometry: &Geometry) -> u32 {
    let mut header = [0u8; HEADER_LEN];
    read_majority(mem, 0, HEADER_LEN, &mut header);
    header_errors(&header, &geometry.header())
}

/// How many bits are wrong in all the copies of the header together, if it counts as ours. For the log: 768 bits from a
/// single reset say how much it flips.
pub fn repairable_header_errors<M: Mem>(mem: &M) -> Option<u32> {
    let geometry = Geometry::for_size(mem.size())?;
    (majority_header_errors(mem, &geometry) <= HEADER_TOLERANCE)
        .then(|| all_header_errors(mem, &geometry))
}

/// Which format (3 or 4) the region's header is, if it is within the tolerance of that format's geometry, as
/// [`repairable_header_errors`] measures: format 4 is tried first. `None` for a header that is neither.
pub fn header_format<M: Mem>(mem: &M) -> Option<u32> {
    [VERSION, 3].into_iter().find(|&version| {
        Geometry::for_version(mem.size(), version)
            .is_some_and(|g| majority_header_errors(mem, &g) <= HEADER_TOLERANCE)
    })
}

fn all_header_errors<M: Mem>(mem: &M, geometry: &Geometry) -> u32 {
    let want = geometry.header();
    (0..COPIES)
        .map(|copy| {
            let mut header = [0u8; HEADER_LEN];
            mem.read(copy * HEADER_LEN, &mut header);
            header_errors(&header, &want)
        })
        .sum()
}

/// Where things are in a region of a given size.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Geometry {
    size: usize,
    record_size: usize,
    console_size: usize,
    version: u32,
}

impl Geometry {
    fn for_size(size: usize) -> Option<Self> {
        Self::for_version(size, VERSION)
    }

    /// The layout of format `version` (3 or 4), from [`crate::layout`].
    fn for_version(size: usize, version: u32) -> Option<Self> {
        if !(layout::MIN_SIZE..=layout::MAX_SIZE).contains(&size) {
            return None;
        }
        Some(Self {
            size,
            record_size: layout::record_size(size),
            console_size: layout::console_size(size, version),
            version,
        })
    }

    fn record_at(&self, slot: usize) -> usize {
        FIRST_ZONE + slot * self.record_size
    }

    /// The bytes of text a record can hold.
    fn text_capacity(&self) -> usize {
        (self.record_size - RECORD_PREFIX) / BLOCK * BLOCK_DATA
    }

    fn console_at(&self, zone: u32) -> usize {
        FIRST_ZONE + RECORDS * self.record_size + zone as usize * self.console_size
    }

    /// The bytes of text a console holds.
    fn console_capacity(&self) -> usize {
        (self.console_size - CONSOLE_HEADER) / BLOCK * BLOCK_DATA
    }

    fn header(&self) -> [u8; HEADER_LEN] {
        let mut h = [0u8; HEADER_LEN];
        h[..8].copy_from_slice(MAGIC);
        h[8..12].copy_from_slice(&self.version.to_le_bytes());
        h[12..16].copy_from_slice(&(self.size as u32).to_le_bytes());
        h[16..20].copy_from_slice(&(self.record_size as u32).to_le_bytes());
        h[20..24].copy_from_slice(&(RECORDS as u32).to_le_bytes());
        h[24..28].copy_from_slice(&(self.console_size as u32).to_le_bytes());
        let crc = crc32(&[&h[..28]]);
        h[28..32].copy_from_slice(&crc.to_le_bytes());
        h
    }
}

/// How the last boot ended, as far as the region knows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum LastBoot {
    /// The region was new, or its contents were not ours.
    Unknown = 0,
    /// It asked for a restart, a power-off or a reload.
    Clean = 1,
    /// It wrote a crash record.
    Crashed = 2,
    /// It was still running when this boot began: nothing got to say why it stopped.
    Vanished = 3,
}

impl LastBoot {
    pub fn name(self) -> &'static str {
        match self {
            LastBoot::Unknown => "unknown",
            LastBoot::Clean => "clean",
            LastBoot::Crashed => "crashed",
            LastBoot::Vanished => "vanished",
        }
    }
}

const FLAG_RUNNING: u32 = 1;
const FLAG_CLEAN: u32 = 2;
const FLAG_CRASHED: u32 = 3;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct State {
    boot: u32,
    /// Crashes and unexplained resets since the system last stayed up for a while.
    crashes_in_a_row: u32,
    flags: u32,
    active_console: u32,
    next_seq: u32,
}

impl State {
    fn encode(&self) -> [u8; STATE_LEN] {
        let mut s = [0u8; STATE_LEN];
        s[..4].copy_from_slice(&self.boot.to_le_bytes());
        s[4..8].copy_from_slice(&self.crashes_in_a_row.to_le_bytes());
        s[8..12].copy_from_slice(&self.flags.to_le_bytes());
        s[12..16].copy_from_slice(&self.active_console.to_le_bytes());
        s[16..20].copy_from_slice(&self.next_seq.to_le_bytes());
        let crc = crc32(&[&s[..20]]);
        s[20..24].copy_from_slice(&crc.to_le_bytes());
        s
    }

    fn decode(s: &[u8; STATE_LEN]) -> Option<Self> {
        if crc32(&[&s[..20]]) != u32_at(s, 20) {
            return None;
        }
        Some(Self {
            boot: u32_at(s, 0),
            crashes_in_a_row: u32_at(s, 4),
            flags: u32_at(s, 8),
            active_console: u32_at(s, 12) & 1,
            next_seq: u32_at(s, 16),
        })
    }
}

/// What a slot holds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecordInfo {
    pub slot: usize,
    pub seq: u32,
    /// The boot that wrote it.
    pub boot: u32,
    pub kind: Kind,
    /// Bytes of text.
    pub len: usize,
    /// The text fails its checksum even after correction: some of it is wrong, most of it is not.
    pub damaged: bool,
}

/// A record's header.
struct RecordHeader {
    len: usize,
    seq: u32,
    boot: u32,
    kind: Kind,
    text_crc: u32,
}

/// The saved log in a region of memory.
/// The start of the record of a boot that vanished.
const VANISHED_TITLE: &[u8] = b"The machine reset with no crash report\n\
reason: nothing recorded a crash: a hardware watchdog, a firmware reset or a hang with interrupts off\n";

/// The start of the record that keeps a clean last boot's console across the format change (format 3 to 4).
const KEPT_CONSOLE_TITLE: &[u8] = b"the last boot's console, kept across the format change\n";

pub struct Store<M: Mem> {
    mem: M,
    geometry: Geometry,
    state: State,
    console_written: u64,
    /// The parity of the console's block being filled. A boot begins with [`Store::begin_boot`], so it starts empty.
    console_encoder: Encoder,
    scrub: Scrub,
    /// The slot and block that [`Store::scrub_step`] is at.
    scrub_at: (usize, usize),
    /// The record this boot wrote for the last one, which vanished, until it is amended: slot, sequence number, boot.
    vanished: Option<(usize, u32, u32)>,
    /// Lines for a vanished boot's record under its reason, such as the last register accesses (patch 1188).
    note: [u8; NOTE_MAX],
    note_len: usize,
    /// The PMIC's line once [`Store::amend_unexplained`] gave it, and lines other drivers added later in the boot with
    /// [`Store::append_unexplained`] (patch 1210): each rewrites the record with all of them.
    pmic: [u8; MAX_REASON],
    pmic_len: Option<usize>,
    extra: [u8; EXTRA_MAX],
    extra_len: usize,
    /// The format this region had when it was opened, if older than [`VERSION`]: [`Store::begin_boot`] upgrades it.
    upgrade_from: Option<u32>,
    /// On the boot that upgraded the region, the record written for a boot that vanished is not rewritten: its console has
    /// moved and may be partly overwritten by now (patch 1213).
    frozen: bool,
    /// What the hardware gave to make a new epoch id from (patch 1370): counter, address, a random word.
    seed: u64,
    /// This region's epoch id; 0 until the block has been read or made.
    epoch: u64,
    /// Why this boot made a new epoch, if it did.
    epoch_new: Option<EpochOrigin>,
    /// Why the region was laid out afresh, if it was (patch 1372).
    restart: Option<RestartCause>,
    /// The record of it is written by the first [`Store::begin_boot`].
    restart_pending: bool,
}

/// Why a region got a new epoch id.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EpochOrigin {
    /// The store formatted the region.
    Formatted,
    /// A region from before the epoch block (patch 1213), or one whose block was overwritten.
    NoBlock,
    /// The block was there but no copy passed its checks.
    Damaged,
}

impl EpochOrigin {
    pub fn name(self) -> &'static str {
        match self {
            EpochOrigin::Formatted => "formatted",
            EpochOrigin::NoBlock => "no epoch block",
            EpochOrigin::Damaged => "epoch block failed its vote",
        }
    }
}

/// The most that [`Store::append_unexplained`] adds to a vanished boot's record.
pub const EXTRA_MAX: usize = 1024;

/// The most a vanished boot's note holds.
pub const NOTE_MAX: usize = 4096;

/// The bytes at the end of a region of `size` that the store leaves alone, if any: the register trace's area (patch 1188),
/// 68 KiB in format 4's 2 MiB region (patch 1213).
pub fn spare_tail(size: usize) -> Option<core::ops::Range<usize>> {
    let g = Geometry::for_size(size)?;
    // layout::tail_start and console_at(2) are the same address; the layout is what the proofs check, and the
    // spare tail must start where the layout says the tail starts.
    let end = layout::tail_start(size, g.version);
    (end < size).then_some(end..size)
}

/// Where format 3 kept its spare tail (the register trace of a kernel before patch 1213): read once, on the boot that
/// upgrades the region.
pub fn format3_tail(size: usize) -> Option<core::ops::Range<usize>> {
    let g = Geometry::for_version(size, 3)?;
    let end = layout::tail_start(size, g.version);
    (end < size).then_some(end..size)
}

impl<M: Mem> Store<M> {
    /// Takes a region as it is, repairing a header with a few wrong bits. A region that is too small, or whose header is not
    /// close to what it should be, is given back.
    pub fn attach(mem: M) -> Result<Self, M> {
        Self::attach_seeded(mem, 0)
    }

    /// [`Store::attach`] with the words the hardware gives for a new epoch id (see [`Store::epoch`]).
    pub fn attach_seeded(mut mem: M, seed: u64) -> Result<Self, M> {
        let Some(mut geometry) = Geometry::for_size(mem.size()) else {
            return Err(mem);
        };
        let mut upgrade_from = None;
        if majority_header_errors(&mem, &geometry) > HEADER_TOLERANCE {
            // A format-3 region (a kernel before patch 1213): read it as it is, and upgrade it in `begin_boot`, so that the
            // boot that brings format 4 loses no record.
            match Geometry::for_version(mem.size(), 3) {
                Some(old) if majority_header_errors(&mem, &old) <= HEADER_TOLERANCE => {
                    geometry = old;
                    upgrade_from = Some(3);
                }
                _ => return Err(mem),
            }
        }
        if all_header_errors(&mem, &geometry) > 0 {
            Self::write_header(&mut mem, &geometry);
            mem.sync();
        }
        let mut store = Self {
            mem,
            geometry,
            state: State::default(),
            console_written: 0,
            console_encoder: Encoder::new(),
            scrub: Scrub::default(),
            scrub_at: (0, 0),
            vanished: None,
            note: [0; NOTE_MAX],
            note_len: 0,
            pmic: [0; MAX_REASON],
            pmic_len: None,
            extra: [0; EXTRA_MAX],
            extra_len: 0,
            upgrade_from: None,
            frozen: false,
            seed,
            epoch: 0,
            epoch_new: None,
            restart: None,
            restart_pending: false,
        };
        store.upgrade_from = upgrade_from;
        store.epoch = store.read_epoch().unwrap_or(0);
        if upgrade_from == Some(3) && store.epoch != 0 {
            // Our epoch block under a format-3 header: a kernel from before patch 1213 started the region over.
            store.restart = Some(RestartCause::OldKernel(3));
            store.restart_pending = true;
        }
        store.state = store.read_state().unwrap_or_default();
        store.console_written = store.read_console_written(store.state.active_console);
        Ok(store)
    }

    /// The format this region was in before this boot upgraded it, if it did (patch 1213): the kernel says so in its log.
    pub fn upgraded_from(&self) -> Option<u32> {
        if self.geometry.version == VERSION {
            self.upgrade_from
        } else {
            None
        }
    }

    fn write_header(mem: &mut M, geometry: &Geometry) {
        for copy in 0..COPIES {
            mem.write(copy * HEADER_LEN, &geometry.header());
        }
    }

    /// Writes an empty region's header. Gives the region back if it is too small to use.
    pub fn format(mem: M) -> Result<Self, M> {
        Self::format_seeded(mem, 0)
    }

    /// [`Store::format`] with the words the hardware gives for a new epoch id (see [`Store::epoch`]).
    pub fn format_seeded(mut mem: M, seed: u64) -> Result<Self, M> {
        let Some(geometry) = Geometry::for_size(mem.size()) else {
            return Err(mem);
        };
        let epoch = new_epoch_id(seed, mem.size());
        write_epoch(&mut mem, epoch);
        for slot in 0..RECORDS {
            mem.write(geometry.record_at(slot), &[0; RECORD_PREFIX]);
        }
        for zone in 0..2 {
            mem.write(geometry.console_at(zone), &[0; CONSOLE_HEADER]);
        }
        write_protected(&mut mem, STATE_AT, &State::default().encode());
        Self::write_header(&mut mem, &geometry);
        mem.sync();
        Ok(Self {
            mem,
            geometry,
            state: State::default(),
            console_written: 0,
            console_encoder: Encoder::new(),
            scrub: Scrub::default(),
            scrub_at: (0, 0),
            vanished: None,
            note: [0; NOTE_MAX],
            note_len: 0,
            pmic: [0; MAX_REASON],
            pmic_len: None,
            extra: [0; EXTRA_MAX],
            extra_len: 0,
            upgrade_from: None,
            frozen: false,
            seed,
            epoch,
            epoch_new: Some(EpochOrigin::Formatted),
            restart: None,
            restart_pending: false,
        })
    }

    /// Takes a region at the start of a boot: keeps what it holds if its header is good, starts over if not. `None` if it
    /// is too small.
    pub fn open(mem: M) -> Option<Self> {
        let mut store = Self::open_seeded(mem, 0)?;
        // The unseeded form is for tools and tests: it says why (`restart_cause`) but keeps the new region empty.
        store.restart_pending = false;
        Some(store)
    }

    /// [`Store::open`] with the words the hardware gives for a new epoch id (see [`Store::epoch`]).
    pub fn open_seeded(mem: M, seed: u64) -> Option<Self> {
        match Self::attach_seeded(mem, seed) {
            Ok(store) => Some(store),
            Err(mem) => {
                let cause = Geometry::for_size(mem.size()).map(|_| classify_restart(&mem));
                let mut store = Self::format_seeded(mem, seed).ok()?;
                store.restart = cause;
                store.restart_pending = cause.is_some();
                Some(store)
            }
        }
    }

    /// Why this store laid the region out afresh, if it did (patch 1372).
    pub fn restart_cause(&self) -> Option<RestartCause> {
        self.restart
    }

    /// The region's epoch id: it changes when the region is formatted or its epoch block is lost, and only then. A restart
    /// that keeps the region keeps the id, and the boot counter counts within it (patch 1370). 0 before [`Store::begin_boot`]
    /// on a region that has no block yet.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Why this boot made a new epoch, if it did.
    pub fn epoch_new(&self) -> Option<EpochOrigin> {
        self.epoch_new
    }

    /// Reads the epoch block: the copies that pass their error correction and checksum vote, bit by bit.
    fn read_epoch(&self) -> Option<u64> {
        read_epoch_block(&self.mem).ok()
    }

    /// Makes a new epoch if the block fails its vote.
    fn ensure_epoch(&mut self) {
        match read_epoch_block(&self.mem) {
            Ok(id) => self.epoch = id,
            Err(origin) => {
                self.epoch = new_epoch_id(self.seed, self.mem.size());
                write_epoch(&mut self.mem, self.epoch);
                self.epoch_new = Some(origin);
            }
        }
    }

    fn read_state(&self) -> Option<State> {
        let mut s = [0u8; STATE_LEN];
        let valid = |bytes: &[u8]| crc32(&[&bytes[..20]]) == u32_at(bytes, 20);
        read_protected(&self.mem, STATE_AT, &mut s, valid).then(|| State::decode(&s))?
    }

    fn save_state(&mut self) {
        let state = self.state.encode();
        write_protected(&mut self.mem, STATE_AT, &state);
        self.mem.sync();
    }

    /// Writes back one block of what can be corrected in the records, so that the next reset's flips do not add to this one's. Returns
    /// whether there is more. One block can take a fraction of a millisecond, and a record has hundreds: this is for the background.
    pub fn scrub_step(&mut self) -> bool {
        let (mut slot, mut block) = self.scrub_at;
        while slot < RECORDS {
            let Some(raw) = self.raw_record_header(slot) else {
                (slot, block) = (slot + 1, 0);
                continue;
            };
            let len = u32_at(&raw, 4) as usize;
            if block >= len.div_ceil(BLOCK_DATA) {
                (slot, block) = (slot + 1, 0);
                continue;
            }
            if block == 0 {
                write_protected(&mut self.mem, self.geometry.record_at(slot), &raw);
            }
            let at = self.text_base(slot) + block * BLOCK;
            let data_len = (len - block * BLOCK_DATA).min(BLOCK_DATA);
            let mut data = [0u8; BLOCK_DATA];
            match read_block(&self.mem, at, data_len, &mut data) {
                None => self.scrub.lost_blocks += 1,
                Some(0) => {}
                Some(wrong) => {
                    self.scrub.corrected += wrong;
                    self.mem.write(at, &data[..data_len]);
                    self.mem
                        .write(at + BLOCK_DATA, &ecc::parity_of(&data[..data_len]));
                }
            }
            self.mem.sync();
            self.scrub_at = (slot, block + 1);
            return true;
        }
        self.scrub_at = (RECORDS, 0);
        false
    }

    /// What [`Store::scrub_step`] has found so far.
    pub fn scrub_result(&self) -> Scrub {
        self.scrub
    }

    /// Begins a boot: works out how the last one ended and moves on to a fresh console, keeping the last boot's. If the last
    /// boot vanished, this writes a record for it with the end of its console. Returns how it ended.
    pub fn begin_boot(&mut self) -> LastBoot {
        self.scrub = Scrub::default();
        self.scrub_at = (0, 0);
        let valid = self.read_state();
        let last = match valid.map(|s| s.flags) {
            Some(FLAG_RUNNING) => LastBoot::Vanished,
            Some(FLAG_CLEAN) => LastBoot::Clean,
            Some(FLAG_CRASHED) => LastBoot::Crashed,
            _ => LastBoot::Unknown,
        };
        match valid {
            Some(state) => self.state = state,
            None => {
                // The records have the sequence numbers; only the counters are lost.
                let next_seq = self
                    .records()
                    .flatten()
                    .map(|r| r.seq.wrapping_add(1))
                    .max()
                    .unwrap_or(0);
                self.state = State {
                    next_seq,
                    ..State::default()
                };
            }
        }
        // This console becomes the last one, read many times: it should not take a correction each time.
        let console = self.geometry.console_at(self.state.active_console);
        if let Some(fields) = self.read_console_fields(self.state.active_console) {
            write_protected(&mut self.mem, console, &fields);
            self.mem.sync();
        }
        match last {
            LastBoot::Vanished => {
                self.state.crashes_in_a_row += 1;
                self.record_vanished_boot();
            }
            LastBoot::Clean => {
                self.state.crashes_in_a_row = 0;
                // The format change moves the consoles: format 4's zone 1 overlaps the end of format 3's zone 0, and the
                // last boot's console is read from the wrong place after the switch. Its end is kept as a record now, read
                // with the format-3 geometry (review of patch 1213, finding 8).
                if self.upgrade_from.is_some()
                    && self.geometry.version != VERSION
                    && self.last_console_len_now() > 0
                {
                    self.record_console_tail(
                        Kind::UnexplainedReset,
                        &[KEPT_CONSOLE_TITLE, b"last kernel log:\n"],
                    );
                }
            }
            LastBoot::Crashed | LastBoot::Unknown => {}
        }
        self.ensure_epoch();
        self.state.active_console ^= 1;
        self.state.boot = self.state.boot.wrapping_add(1);
        self.state.flags = FLAG_RUNNING;
        if let Some(cause) = self.restart.filter(|_| self.restart_pending) {
            self.restart_pending = false;
            self.record_restart(cause);
        }
        self.save_state();
        if self.upgrade_from.is_some() && self.geometry.version != VERSION {
            // The upgrade (patch 1213): the records stay where they are; this boot's console starts at its format-4 place.
            // The vanished boot's record, written above from its format-3 console, is not rewritten later this boot.
            if let Some(new) = Geometry::for_size(self.geometry.size) {
                self.geometry = new;
                Self::write_header(&mut self.mem, &self.geometry);
                self.frozen = true;
            }
        }
        self.console_written = 0;
        self.console_encoder = Encoder::new();
        self.save_console_header();
        self.mem.sync();
        last
    }

    /// Writes the record that says why the region was started over, with the same words as the console line.
    fn record_restart(&mut self, cause: RestartCause) {
        use core::fmt::Write;
        struct Line([u8; 320], usize);
        impl Write for Line {
            fn write_str(&mut self, s: &str) -> core::fmt::Result {
                let n = s.len().min(self.0.len() - self.1);
                self.0[self.1..self.1 + n].copy_from_slice(&s.as_bytes()[..n]);
                self.1 += n;
                Ok(())
            }
        }
        let mut line = Line([0; 320], 0);
        let _ = write!(line, "{cause}");
        self.write_record(
            Kind::RegionRestarted,
            &[
                b"The saved log's region was started over\nreason: ",
                &line.0[..line.1],
                b"\nthe records it held are lost\n",
            ],
        );
    }

    /// Sets lines that the record of a vanished boot carries under its reason, before [`Store::begin_boot`] writes it: the
    /// last boot's register trace (patch 1188). Cut at [`NOTE_MAX`]; each line ends with a newline.
    pub fn set_vanished_note(&mut self, note: &[u8]) {
        let n = note.len().min(NOTE_MAX);
        self.note[..n].copy_from_slice(&note[..n]);
        self.note_len = n;
    }

    fn record_vanished_boot(&mut self) {
        let (seq, boot) = (self.state.next_seq, self.state.boot);
        let note = self.note;
        let slot = self.record_console_tail(
            Kind::UnexplainedReset,
            &[
                VANISHED_TITLE,
                &note[..self.note_len],
                b"last kernel log:\n",
            ],
        );
        self.vanished = Some((slot, seq, boot));
    }

    /// Adds `line` (the PMIC's word on how the last boot ended) to the record [`Store::begin_boot`] wrote for a boot that
    /// vanished, as `pmic: <line>` under its reason. The record keeps its slot, sequence number and boot, and the last
    /// console it quotes is still there. It is written whole again, its header last: a reset in the middle leaves the record
    /// as it was or the new one, as any record's writing does. Returns whether there was such a record; once amended it is
    /// not amended again.
    pub fn amend_unexplained(&mut self, line: &[u8]) -> bool {
        if self.vanished.is_none() || self.pmic_len.is_some() || self.frozen {
            return false;
        }
        let n = line.len().min(MAX_REASON);
        self.pmic[..n].copy_from_slice(&line[..n]);
        self.pmic_len = Some(n);
        self.rewrite_vanished();
        true
    }

    /// Adds `lines` (each ending with a newline) to the record [`Store::begin_boot`] wrote for a boot that vanished, under the
    /// PMIC's line and the register trace (patch 1210): what a driver found later in this boot about how the last one ended,
    /// such as a remote processor's crash reason left in SMEM. Kept to whole lines within [`EXTRA_MAX`] in all. Returns
    /// whether there was such a record.
    pub fn append_unexplained(&mut self, lines: &[u8]) -> bool {
        if self.vanished.is_none() || self.frozen {
            return false;
        }
        let room = EXTRA_MAX - self.extra_len;
        let fit = if lines.len() <= room {
            lines.len()
        } else {
            lines[..room]
                .iter()
                .rposition(|&b| b == b'\n')
                .map_or(0, |i| i + 1)
        };
        if fit > 0 {
            self.extra[self.extra_len..self.extra_len + fit].copy_from_slice(&lines[..fit]);
            self.extra_len += fit;
            self.rewrite_vanished();
        }
        true
    }

    /// Writes the vanished boot's record again with everything it has: reason, PMIC line, note, added lines, last console.
    fn rewrite_vanished(&mut self) {
        let Some((slot, seq, boot)) = self.vanished else {
            return;
        };
        let zone = self.state.active_console ^ 1;
        let (note, note_len) = (self.note, self.note_len);
        let (pmic, extra, extra_len) = (self.pmic, self.extra, self.extra_len);
        let (prefix, pmic, newline): (&[u8], &[u8], &[u8]) = match self.pmic_len {
            Some(n) => (b"pmic: ", &pmic[..n], b"\n"),
            None => (b"", b"", b""),
        };
        self.write_console_tail(
            slot,
            zone,
            Kind::UnexplainedReset,
            &[
                VANISHED_TITLE,
                prefix,
                pmic,
                newline,
                &note[..note_len],
                &extra[..extra_len],
                b"last kernel log:\n",
            ],
            Some((seq, boot)),
        );
    }

    /// Writes a record for a restart that a program asked for, with the end of this boot's console.
    pub fn record_requested_reset(&mut self, reason: &[u8]) {
        let reason = &reason[..reason.len().min(MAX_REASON)];
        self.record_console_tail(
            Kind::RequestedReset,
            &[
                b"A program asked for this restart\nreason: ",
                reason,
                b"\nlast kernel log:\n",
            ],
        );
    }

    /// Writes a record of kind [`Kind::Modem`]: the modem DSP failed, `cause` says what told (its watchdog interrupt or its
    /// fatal bit) and `reason` is its own crash reason (SMEM item 421), with the end of this boot's console (patch 1057).
    /// The boot goes on, so its state stays `running`; only the next sequence number is kept at once, so that a reset right
    /// after finds the record and numbers the next one after it.
    pub fn record_modem(&mut self, cause: &[u8], reason: &[u8]) {
        let cause = &cause[..cause.len().min(MAX_REASON)];
        let reason = &reason[..reason.len().min(MAX_REASON)];
        self.record_console_tail(
            Kind::Modem,
            &[
                b"The modem DSP failed\ncause: ",
                cause,
                b"\nreason: ",
                reason,
                b"\nlast kernel log:\n",
            ],
        );
        self.save_state();
    }

    /// Writes a record: the title's pieces, then the end of the active console, as much as fits. Returns its slot.
    fn record_console_tail(&mut self, kind: Kind, title: &[&[u8]]) -> usize {
        let slot = self.next_slot();
        self.write_console_tail(slot, self.state.active_console, kind, title, None);
        slot
    }

    /// Writes a record in `slot`: the title's pieces, then the end of console `zone`. `again`: the sequence number and boot of
    /// the record being written again, else the next ones.
    fn write_console_tail(
        &mut self,
        slot: usize,
        zone: u32,
        kind: Kind,
        title: &[&[u8]],
        again: Option<(u32, u32)>,
    ) {
        let total = self.console_len_of(zone);
        let title_len: usize = title.iter().map(|part| part.len()).sum();
        let tail = total
            .min(UNEXPLAINED_TAIL)
            .min(self.text_capacity() - title_len);
        // Invalidate the slot first, so that a reset in the middle does not leave an old header on new text.
        self.clear_slot(slot);
        let mut crc = !0;
        let mut writer = TextWriter::new(self.text_base(slot));
        for part in title {
            writer.push(&mut self.mem, part);
            crc = crc32_update(crc, part);
        }
        let mut chunk = [0u8; 256];
        let mut done = 0;
        while done < tail {
            let n = chunk.len().min(tail - done);
            self.read_console_of(zone, total - tail + done, &mut chunk[..n]);
            writer.push(&mut self.mem, &chunk[..n]);
            crc = crc32_update(crc, &chunk[..n]);
            done += n;
        }
        writer.finish(&mut self.mem);
        match again {
            Some((seq, boot)) => {
                self.write_record_header(slot, kind, title_len + tail, !crc, seq, boot)
            }
            None => self.finish_record(slot, kind, title_len + tail, !crc),
        }
    }

    /// The most text a record holds.
    pub fn text_capacity(&self) -> usize {
        self.geometry.text_capacity()
    }

    fn text_base(&self, slot: usize) -> usize {
        self.geometry.record_at(slot) + RECORD_PREFIX
    }

    fn clear_slot(&mut self, slot: usize) {
        self.mem
            .write(self.geometry.record_at(slot), &[0; RECORD_PREFIX]);
        self.mem.sync();
    }

    /// The slot for the next record: an empty one, or else the oldest.
    fn next_slot(&self) -> usize {
        let mut oldest = (0, u32::MAX);
        for slot in 0..RECORDS {
            match self.header_in(slot) {
                None => return slot,
                // Sequence numbers do not wrap in the life of this machine.
                Some(h) if h.seq < oldest.1 => oldest = (slot, h.seq),
                Some(_) => {}
            }
        }
        oldest.0
    }

    /// Writes a crash record from pieces of text, its headers last. What does not fit leaves the beginning of the last
    /// piece, the kernel log: its newest lines are the ones a crash is about.
    pub fn write_record(&mut self, kind: Kind, parts: &[&[u8]]) {
        let slot = self.next_slot();
        self.clear_slot(slot);
        let capacity = self.text_capacity();
        let before_last: usize = parts.iter().rev().skip(1).map(|part| part.len()).sum();
        let room_for_last = capacity.saturating_sub(before_last);
        let mut crc = !0;
        let mut writer = TextWriter::new(self.text_base(slot));
        for (i, part) in parts.iter().enumerate() {
            let part = if i + 1 == parts.len() {
                &part[part.len().saturating_sub(room_for_last)..]
            } else {
                *part
            };
            let n = part.len().min(capacity - writer.len);
            writer.push(&mut self.mem, &part[..n]);
            crc = crc32_update(crc, &part[..n]);
        }
        writer.finish(&mut self.mem);
        let len = writer.len;
        self.finish_record(slot, kind, len, !crc);
    }

    /// Writes the headers of a record whose text is in place, with the next sequence number.
    fn finish_record(&mut self, slot: usize, kind: Kind, len: usize, text_crc: u32) {
        self.write_record_header(
            slot,
            kind,
            len,
            text_crc,
            self.state.next_seq,
            self.state.boot,
        );
        self.state.next_seq = self.state.next_seq.wrapping_add(1);
    }

    /// Writes the headers of a record whose text is in place.
    fn write_record_header(
        &mut self,
        slot: usize,
        kind: Kind,
        len: usize,
        text_crc: u32,
        seq: u32,
        boot: u32,
    ) {
        self.mem.sync();
        let mut h = [0u8; RECORD_HEADER];
        h[..4].copy_from_slice(&RECORD_MAGIC.to_le_bytes());
        h[4..8].copy_from_slice(&(len as u32).to_le_bytes());
        h[8..12].copy_from_slice(&seq.to_le_bytes());
        h[12..16].copy_from_slice(&boot.to_le_bytes());
        h[16..20].copy_from_slice(&(kind as u32).to_le_bytes());
        h[20..24].copy_from_slice(&text_crc.to_le_bytes());
        let crc = crc32(&[&h[..28]]);
        h[28..32].copy_from_slice(&crc.to_le_bytes());
        write_protected(&mut self.mem, self.geometry.record_at(slot), &h);
        self.mem.sync();
    }

    /// A slot's header as it was written, if it can be read and passes its check.
    fn raw_record_header(&self, slot: usize) -> Option<[u8; RECORD_HEADER]> {
        let mut h = [0u8; RECORD_HEADER];
        let capacity = self.text_capacity();
        let valid = |h: &[u8]| {
            u32_at(h, 0) == RECORD_MAGIC
                && crc32(&[&h[..28]]) == u32_at(h, 28)
                && u32_at(h, 4) as usize <= capacity
        };
        read_protected(&self.mem, self.geometry.record_at(slot), &mut h, valid).then_some(h)
    }

    /// Reads a slot's header. The text is not looked at.
    fn header_in(&self, slot: usize) -> Option<RecordHeader> {
        let h = self.raw_record_header(slot)?;
        Some(RecordHeader {
            len: u32_at(&h, 4) as usize,
            seq: u32_at(&h, 8),
            boot: u32_at(&h, 12),
            kind: Kind::from_u32(u32_at(&h, 16))?,
            text_crc: u32_at(&h, 20),
        })
    }

    /// Reads a slot's record, checking its text too.
    fn record_in(&self, slot: usize) -> Option<RecordInfo> {
        let header = self.header_in(slot)?;
        let mut crc = !0;
        let mut data = [0u8; BLOCK_DATA];
        for block in 0..header.len.div_ceil(BLOCK_DATA) {
            let n = (header.len - block * BLOCK_DATA).min(BLOCK_DATA);
            read_block(
                &self.mem,
                self.text_base(slot) + block * BLOCK,
                n,
                &mut data,
            );
            crc = crc32_update(crc, &data[..n]);
        }
        Some(RecordInfo {
            slot,
            seq: header.seq,
            boot: header.boot,
            kind: header.kind,
            len: header.len,
            damaged: !crc != header.text_crc,
        })
    }

    fn read_text(&self, slot: usize, len: usize, offset: usize, out: &mut [u8]) {
        let mut data = [0u8; BLOCK_DATA];
        let mut done = 0;
        while done < out.len() {
            let at = offset + done;
            let (block, in_block) = (at / BLOCK_DATA, at % BLOCK_DATA);
            let block_len = (len - block * BLOCK_DATA).min(BLOCK_DATA);
            read_block(
                &self.mem,
                self.text_base(slot) + block * BLOCK,
                block_len,
                &mut data,
            );
            let n = (out.len() - done).min(block_len - in_block);
            out[done..done + n].copy_from_slice(&data[in_block..in_block + n]);
            done += n;
        }
    }

    /// The newest record, going by the headers alone: `damaged` says nothing here, and the boot does not wait for its text to be
    /// corrected.
    pub fn newest_record(&self) -> Option<RecordInfo> {
        (0..RECORDS)
            .filter_map(|slot| {
                let header = self.header_in(slot)?;
                Some(RecordInfo {
                    slot,
                    seq: header.seq,
                    boot: header.boot,
                    kind: header.kind,
                    len: header.len,
                    damaged: false,
                })
            })
            .max_by_key(|record| record.seq)
    }

    /// What each slot holds. A slot that is empty or whose header fails gives `None`.
    pub fn records(&self) -> impl Iterator<Item = Option<RecordInfo>> + '_ {
        (0..RECORDS).map(|slot| self.record_in(slot))
    }

    /// Copies part of a record's text. Returns the bytes copied; a slot without a valid header has none.
    pub fn read_record(&self, slot: usize, offset: usize, out: &mut [u8]) -> usize {
        let Some(header) = (slot < RECORDS).then(|| self.header_in(slot)).flatten() else {
            return 0;
        };
        let n = out.len().min(header.len.saturating_sub(offset));
        self.read_text(slot, header.len, offset, &mut out[..n]);
        n
    }

    /// Empties a slot.
    pub fn erase_record(&mut self, slot: usize) {
        if slot < RECORDS {
            self.clear_slot(slot);
        }
    }

    /// The boot's number, counted from 1. 0 before [`Store::begin_boot`] on a new region.
    pub fn boot(&self) -> u32 {
        self.state.boot
    }

    /// Crashes and unexplained resets since the system last stayed up for a while.
    pub fn crashes_in_a_row(&self) -> u32 {
        self.state.crashes_in_a_row
    }

    /// Records that this boot crashed. Call it after writing the record.
    pub fn note_crash(&mut self) {
        self.state.crashes_in_a_row = self.state.crashes_in_a_row.saturating_add(1);
        self.state.flags = FLAG_CRASHED;
        self.save_state();
    }

    /// Records that this boot ends as asked (a restart, a power-off, a reload).
    pub fn note_clean_end(&mut self) {
        self.state.flags = FLAG_CLEAN;
        self.save_state();
    }

    /// Records that the system has stayed up: the next crash is not part of a loop.
    pub fn note_stable(&mut self) {
        self.state.crashes_in_a_row = 0;
        self.save_state();
    }

    fn save_console_header(&mut self) {
        let mut h = [0u8; CONSOLE_FIELDS];
        h[..4].copy_from_slice(&CONSOLE_MAGIC.to_le_bytes());
        h[4..8].copy_from_slice(&self.state.boot.to_le_bytes());
        h[8..16].copy_from_slice(&self.console_written.to_le_bytes());
        let crc = crc32(&[&h[..16]]);
        h[16..20].copy_from_slice(&crc.to_le_bytes());
        let at = self.geometry.console_at(self.state.active_console);
        write_protected(&mut self.mem, at, &h);
    }

    fn read_console_fields(&self, zone: u32) -> Option<[u8; CONSOLE_FIELDS]> {
        let mut h = [0u8; CONSOLE_FIELDS];
        let valid = |h: &[u8]| u32_at(h, 0) == CONSOLE_MAGIC && crc32(&[&h[..16]]) == u32_at(h, 16);
        read_protected(&self.mem, self.geometry.console_at(zone), &mut h, valid).then_some(h)
    }

    fn read_console_written(&self, zone: u32) -> u64 {
        self.read_console_fields(zone).map_or(0, |h| {
            u64::from(u32_at(&h, 8)) | u64::from(u32_at(&h, 12)) << 32
        })
    }

    /// Adds text to this boot's console, overwriting its oldest text when full. A machine that dies right after still leaves it.
    pub fn console_write(&mut self, text: &[u8]) {
        self.console_append(text);
        self.console_commit();
    }

    /// Adds a piece of text. It counts from [`Store::console_commit`] on: this way a log line of many pieces writes its parity once.
    pub fn console_append(&mut self, text: &[u8]) {
        let capacity = self.geometry.console_capacity();
        let data = self.geometry.console_at(self.state.active_console) + CONSOLE_HEADER;
        // Only the end of something longer than the console stays. The position must not change: the blocks' parity hangs on it.
        let laps = (text.len() / capacity).saturating_sub(1);
        self.console_written += (laps * capacity) as u64;
        let mut text = &text[laps * capacity..];
        while !text.is_empty() {
            let at = (self.console_written % capacity as u64) as usize;
            let (block, offset) = (at / BLOCK_DATA, at % BLOCK_DATA);
            let n = text.len().min(BLOCK_DATA - offset);
            self.mem.write(data + block * BLOCK + offset, &text[..n]);
            self.console_encoder.push(&text[..n]);
            self.console_written += n as u64;
            text = &text[n..];
            if offset + n == BLOCK_DATA {
                self.mem.write(
                    data + block * BLOCK + BLOCK_DATA,
                    self.console_encoder.parity(),
                );
                self.console_encoder = Encoder::new();
            }
        }
    }

    /// Writes the parity of the block being filled, and how much there is.
    pub fn console_commit(&mut self) {
        let capacity = self.geometry.console_capacity();
        let data = self.geometry.console_at(self.state.active_console) + CONSOLE_HEADER;
        let at = (self.console_written % capacity as u64) as usize;
        let (block, offset) = (at / BLOCK_DATA, at % BLOCK_DATA);
        if offset != 0 {
            self.mem.write(
                data + block * BLOCK + BLOCK_DATA,
                self.console_encoder.parity(),
            );
        }
        self.save_console_header();
        self.mem.sync();
    }

    /// Where a console's text starts in its ring, and how long it is. A block that is being overwritten has parity only for its
    /// new part, so what is left of its old text is not offered.
    fn console_span(&self, written: u64) -> (usize, usize) {
        let capacity = self.geometry.console_capacity();
        if written <= capacity as u64 {
            return (0, written as usize);
        }
        let at = (written % capacity as u64) as usize;
        match at % BLOCK_DATA {
            0 => (at, capacity),
            new => (
                (at - new + BLOCK_DATA) % capacity,
                capacity - BLOCK_DATA + new,
            ),
        }
    }

    fn console_len_of(&self, zone: u32) -> usize {
        self.console_span(self.read_console_written(zone)).1
    }

    /// Copies console text, `offset` counting from its oldest byte that is still there.
    fn read_console_of(&self, zone: u32, offset: usize, out: &mut [u8]) -> usize {
        let capacity = self.geometry.console_capacity();
        let written = self.read_console_written(zone);
        let (start, len) = self.console_span(written);
        let last = (written % capacity as u64) as usize;
        let base = self.geometry.console_at(zone) + CONSOLE_HEADER;
        let mut data = [0u8; BLOCK_DATA];
        let mut done = 0;
        let total = out.len().min(len.saturating_sub(offset));
        while done < total {
            let at = (start + offset + done) % capacity;
            let (block, in_block) = (at / BLOCK_DATA, at % BLOCK_DATA);
            // The block being filled is as long as what it has so far.
            let block_len = match last % BLOCK_DATA {
                new @ 1.. if block == last / BLOCK_DATA => new,
                _ => BLOCK_DATA,
            };
            read_block(&self.mem, base + block * BLOCK, block_len, &mut data);
            let n = (total - done).min(block_len - in_block);
            out[done..done + n].copy_from_slice(&data[in_block..in_block + n]);
            done += n;
        }
        total
    }

    /// The length of the console of the boot that `state` still says is active: before [`Store::begin_boot`] flips it, the
    /// last boot's.
    fn last_console_len_now(&self) -> usize {
        self.console_len_of(self.state.active_console)
    }

    /// The length of the last boot's console.
    pub fn last_console_len(&self) -> usize {
        self.console_len_of(self.state.active_console ^ 1)
    }

    /// Copies text of the last boot's console: all the kernel logged, as far as it fits.
    pub fn read_last_console(&self, offset: usize, out: &mut [u8]) -> usize {
        self.read_console_of(self.state.active_console ^ 1, offset, out)
    }

    /// Copies text of this boot's console.
    pub fn read_console(&self, offset: usize, out: &mut [u8]) -> usize {
        self.read_console_of(self.state.active_console, offset, out)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use std::{format, string::String, vec, vec::Vec};

    use super::*;

    const SIZE: usize = 2 * 1024 * 1024;

    /// The region; a test keeps it across "resets" by taking it back out of the store.
    #[derive(Clone)]
    struct Ram(Vec<u8>);

    impl Mem for Ram {
        fn size(&self) -> usize {
            self.0.len()
        }

        fn read(&self, offset: usize, out: &mut [u8]) {
            out.copy_from_slice(&self.0[offset..offset + out.len()]);
        }

        fn write(&mut self, offset: usize, data: &[u8]) {
            self.0[offset..offset + data.len()].copy_from_slice(data);
        }
    }

    fn fresh() -> Ram {
        // Not zeros: what a machine's RAM holds before anyone wrote it.
        Ram((0..SIZE).map(|i| (i * 31 % 251) as u8).collect())
    }

    /// A reset: the RAM stays and nothing else does.
    fn reset(store: Store<Ram>) -> Store<Ram> {
        Store::open(store.mem).unwrap()
    }

    fn scrubbed(store: &mut Store<Ram>) -> Scrub {
        while store.scrub_step() {}
        store.scrub_result()
    }

    fn text_of(store: &Store<Ram>, slot: usize) -> String {
        let mut out = vec![0u8; store.text_capacity()];
        let n = store.read_record(slot, 0, &mut out);
        String::from_utf8(out[..n].to_vec()).unwrap()
    }

    fn last_console(store: &Store<Ram>) -> String {
        let mut out = vec![0u8; store.last_console_len()];
        let n = store.read_last_console(0, &mut out);
        assert_eq!(n, out.len());
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn a_region_argument_is_read() {
        assert_eq!(
            parse_region("0x9fe00000+0x200000"),
            Some((0x9fe0_0000, 0xa000_0000))
        );
        assert_eq!(
            parse_region("9fe00000+200000"),
            Some((0x9fe0_0000, 0xa000_0000))
        );
        for bad in [
            "",
            "off",
            "0x9fe00000",
            "0x9fe00001+0x200000",
            "0x9fe00000+0x200001",
            "0x9fe00000+0x1000",
            "xyz+0x200000",
            "0xffffffffffff0000+0x200000",
        ] {
            assert_eq!(parse_region(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_region_must_be_ram_and_free() {
        let region = (0x1000_0000, 0x1020_0000);
        let ram = || [(0x0800_0000, 0x2000_0000)].into_iter();
        let none = || core::iter::empty();
        assert_eq!(region_is_free(region, ram(), none()), Ok(()));
        // Next to what is in use, not on it.
        let touching = [(0x0f00_0000, 0x1000_0000), (0x1020_0000, 0x1030_0000)];
        assert_eq!(region_is_free(region, ram(), touching.into_iter()), Ok(()));
        for overlap in [
            (0x0f00_0000, 0x1000_1000),
            (0x101f_f000, 0x1030_0000),
            (0x1001_0000, 0x1002_0000),
            (0x0f00_0000, 0x1100_0000),
        ] {
            assert!(region_is_free(region, ram(), [overlap].into_iter()).is_err());
        }
        // Outside RAM, or across the end of a bank.
        assert!(region_is_free(region, [(0, 0x1000_0000)].into_iter(), none()).is_err());
        assert!(region_is_free(region, [(0x1000_0000, 0x1010_0000)].into_iter(), none()).is_err());
        assert!(region_is_free(region, core::iter::empty(), none()).is_err());
    }

    #[test]
    fn an_intact_pattern_has_no_errors() {
        let mut ram = fresh();
        fill_pattern(&mut ram);
        let result = check_pattern(&ram).unwrap();
        assert_eq!(result.bit_errors, 0);
        assert_eq!(result.first_error_at, None);
        assert_eq!(result.bytes, SIZE);
    }

    #[test]
    fn a_region_without_the_pattern_has_none_to_check() {
        assert_eq!(check_pattern(&fresh()), None);
        assert_eq!(check_pattern(&Ram(vec![0; SIZE])), None);
    }

    #[test]
    fn flipped_bits_are_counted() {
        let mut ram = fresh();
        fill_pattern(&mut ram);
        ram.0[5000] ^= 0b0000_0101;
        ram.0[9000] ^= 0b1000_0000;
        let want_lost = u64::from((0b0000_0101u8 & pattern_byte(5000)).count_ones())
            + u64::from((0b1000_0000u8 & pattern_byte(9000)).count_ones());
        let result = check_pattern(&ram).unwrap();
        assert_eq!(result.bit_errors, 3);
        assert_eq!(result.first_error_at, Some(5000));
        assert_eq!(result.overwritten_pages, 0);
        assert_eq!(result.ones_lost, want_lost);
    }

    #[test]
    fn a_flip_in_the_mark_still_counts_as_the_pattern() {
        let mut ram = fresh();
        fill_pattern(&mut ram);
        ram.0[3] ^= 1;
        let result = check_pattern(&ram).unwrap();
        assert_eq!((result.bit_errors, result.first_error_at), (1, Some(3)));
    }

    #[test]
    fn an_overwritten_page_is_told_from_flips() {
        let mut ram = fresh();
        fill_pattern(&mut ram);
        ram.0[8192..12288].fill(0);
        let result = check_pattern(&ram).unwrap();
        assert_eq!(result.overwritten_pages, 1);
        assert!(result.bit_errors > 1000);
    }

    #[test]
    fn foreign_contents_count_the_header_bits() {
        let mut s = Store::open(fresh()).unwrap();
        s.begin_boot();
        let mut ram = s.mem;
        for copy in 0..COPIES {
            let at = copy * HEADER_LEN;
            ram.0[at + 16..at + 32].iter_mut().for_each(|b| *b ^= 0xff);
        }
        let found = foreign_contents(&ram).unwrap();
        assert_eq!(found.header_bit_errors, 128);
        assert_eq!(&found.first[..8], MAGIC);
    }

    #[test]
    fn a_valid_region_is_not_foreign() {
        let mut s = Store::open(fresh()).unwrap();
        s.begin_boot();
        assert_eq!(foreign_contents(&s.mem), None);
    }

    #[test]
    fn a_wiped_region_shows_as_zeros() {
        let found = foreign_contents(&Ram(vec![0; SIZE])).unwrap();
        assert_eq!(found.pages, SIZE / 4096);
        assert_eq!((found.zero_pages, found.other_pages), (found.pages, 0));
        assert_eq!((found.console_marks, found.record_marks), (0, 0));
    }

    #[test]
    fn a_region_of_ones_shows_as_ones() {
        let found = foreign_contents(&Ram(vec![0xff; SIZE])).unwrap();
        assert_eq!(found.ones_pages, found.pages);
        assert_eq!(found.first, [0xff; 32]);
    }

    #[test]
    fn a_region_that_lost_only_its_header_shows_its_marks() {
        let mut s = Store::open(fresh()).unwrap();
        s.begin_boot();
        s.console_write(b"text");
        s.write_record(Kind::Panic, &[b"x"]);
        s.write_record(Kind::Panic, &[b"y"]);
        let mut ram = s.mem;
        ram.0[..4096].fill(0);
        let found = foreign_contents(&ram).expect("a zeroed header is far from ours");
        assert_eq!(found.first, [0; 32]);
        assert_eq!(found.record_marks, 2);
        assert_eq!(found.console_marks, 1);
        assert!(found.other_pages > 0);
    }

    #[test]
    fn crc_matches_the_standard_check_value() {
        assert_eq!(crc32(&[b"123456789"]), 0xcbf4_3926);
        assert_eq!(crc32(&[b"1234", b"56789"]), 0xcbf4_3926);
    }

    #[test]
    fn geometry_fits_the_region() {
        for size in [MIN_SIZE, 256 * 1024, 1024 * 1024, SIZE, 16 * 1024 * 1024] {
            let g = Geometry::for_size(size).unwrap();
            assert!(g.console_at(1) + g.console_size <= size, "{size}");
            assert!(g.console_size > CONSOLE_HEADER, "{size}");
            assert!(g.record_size > RECORD_PREFIX + BLOCK, "{size}");
            assert_eq!(g.console_capacity() % BLOCK_DATA, 0, "{size}");
            assert!(g.text_capacity() > 1024, "{size}");
        }
        assert_eq!(Geometry::for_size(MIN_SIZE - 1), None);
    }

    #[test]
    fn a_region_too_small_is_refused() {
        assert!(Store::open(Ram(vec![0; 4096])).is_none());
    }

    #[test]
    fn new_region_starts_empty() {
        let mut s = Store::open(fresh()).unwrap();
        assert_eq!(s.begin_boot(), LastBoot::Unknown);
        assert_eq!(s.boot(), 1);
        assert!(s.records().all(|r| r.is_none()));
        assert_eq!(s.last_console_len(), 0);
    }

    #[test]
    fn a_crash_survives_a_reset() {
        let mut s = Store::open(fresh()).unwrap();
        s.begin_boot();
        s.console_write(b"[ 1.0] booted\n");
        s.write_record(Kind::Panic, &[b"Juno crashed\n", b"reason: boom\n"]);
        s.note_crash();

        let mut s = reset(s);
        assert_eq!(s.begin_boot(), LastBoot::Crashed);
        assert_eq!(s.boot(), 2);
        assert_eq!(s.crashes_in_a_row(), 1);
        let r = s.records().flatten().next().unwrap();
        assert_eq!((r.kind, r.boot, r.seq), (Kind::Panic, 1, 0));
        assert_eq!(text_of(&s, r.slot), "Juno crashed\nreason: boom\n");
        assert_eq!(last_console(&s), "[ 1.0] booted\n");
    }

    #[test]
    fn a_boot_that_vanished_gets_a_record_with_its_last_lines() {
        let mut s = Store::open(fresh()).unwrap();
        s.begin_boot();
        s.console_write(b"[ 5.0] xhci: controller fault\n");

        let mut s = reset(s);
        assert_eq!(s.begin_boot(), LastBoot::Vanished);
        assert_eq!(s.crashes_in_a_row(), 1);
        let r = s.records().flatten().next().unwrap();
        assert_eq!(r.kind, Kind::UnexplainedReset);
        let text = text_of(&s, r.slot);
        assert!(text.contains("no crash report"), "{text}");
        assert!(text.ends_with("[ 5.0] xhci: controller fault\n"), "{text}");
    }

    #[test]
    fn a_format_3_region_is_read_once_and_upgraded() {
        // A format-3 region as a kernel before patch 1213 left it: a record, and a boot that vanished.
        let mut mem = fresh();
        let old = Geometry::for_version(mem.size(), 3).unwrap();
        Store::<Ram>::write_header(&mut mem, &old);
        let mut s = Store::attach(mem).ok().unwrap();
        assert_eq!(s.geometry.version, 3);
        // Played as the old kernel would: it never upgrades.
        s.upgrade_from = None;
        s.begin_boot();
        assert_eq!(s.geometry.version, 3);
        s.record_requested_reset(b"before the upgrade");
        s.console_write(b"[ 7.0] the old boot's last line\n");
        let mut s = reset(s);
        // The kernel with format 4 opens it.
        assert_eq!(s.begin_boot(), LastBoot::Vanished);
        assert_eq!(s.upgraded_from(), Some(3));
        assert_eq!(s.geometry.version, 4);
        let records: Vec<_> = s.records().flatten().collect();
        assert_eq!(records.len(), 2, "the old record and the vanished boot's");
        let vanished = records
            .iter()
            .find(|r| r.kind == Kind::UnexplainedReset)
            .unwrap();
        assert!(text_of(&s, vanished.slot).ends_with("[ 7.0] the old boot's last line\n"));
        assert!(records.iter().any(|r| r.kind == Kind::RequestedReset));
        // That record is not rewritten on this boot; its console has moved.
        assert!(!s.amend_unexplained(b"PS_HOLD"));
        assert!(!s.append_unexplained(b"mpss: ...\n"));
        // The next boot finds format 4, upgrades nothing, and still has the records.
        s.console_write(b"[ 1.0] the first format-4 boot\n");
        let mut s = reset(s);
        assert_eq!(s.begin_boot(), LastBoot::Vanished);
        assert_eq!(s.upgraded_from(), None);
        assert_eq!(s.records().flatten().count(), 3);
        let newest = s.records().flatten().max_by_key(|r| r.seq).unwrap();
        assert!(text_of(&s, newest.slot).ends_with("[ 1.0] the first format-4 boot\n"));
        assert!(s.amend_unexplained(b"PS_HOLD"), "rewrites work again");
    }

    #[test]
    fn lines_added_later_go_under_the_pmic_line_in_order() {
        let mut s = Store::open(fresh()).unwrap();
        s.begin_boot();
        s.console_write(b"[ 5.0] wifi: scanning\n");
        let mut s = reset(s);
        s.set_vanished_note(b"last register accesses: ...\n");
        assert_eq!(s.begin_boot(), LastBoot::Vanished);
        assert!(s.append_unexplained(b"mpss: smem items from before this boot\n"));
        assert!(s.amend_unexplained(b"PS_HOLD"));
        assert!(s.append_unexplained(b"mpss: no crash reason left\n"));
        let records: Vec<_> = s.records().flatten().collect();
        assert_eq!(records.len(), 1, "rewritten in place");
        let text = text_of(&s, records[0].slot);
        assert!(
            text.contains("pmic: PS_HOLD\nlast register accesses: ...\nmpss: smem items from before this boot\nmpss: no crash reason left\nlast kernel log:\n"),
            "{text}"
        );
        assert!(text.ends_with("[ 5.0] wifi: scanning\n"), "{text}");
        // Lines that do not fit are left out whole.
        let long = [b'x'; EXTRA_MAX];
        assert!(s.append_unexplained(&long));
        assert!(!text_of(&s, records[0].slot).contains("xxxx"));
    }

    #[test]
    fn nothing_vanished_nothing_appended() {
        let mut s = Store::open(fresh()).unwrap();
        s.begin_boot();
        assert!(!s.append_unexplained(b"line\n"));
    }

    #[test]
    fn the_pmic_line_is_added_to_the_vanished_boots_record() {
        let mut s = Store::open(fresh()).unwrap();
        s.begin_boot();
        s.console_write(b"[ 5.0] xhci: controller fault\n");
        let mut s = reset(s);
        assert_eq!(s.begin_boot(), LastBoot::Vanished);
        let before = s.records().flatten().next().unwrap();
        // This boot's console has its own lines; the record keeps quoting the last boot's.
        s.console_write(b"[ 0.1] this boot\n");
        assert!(s.amend_unexplained(b"last power-off by PS_HOLD; this boot by hard reset, cold"));
        let records: Vec<_> = s.records().flatten().collect();
        assert_eq!(records.len(), 1, "amended in place, not a second record");
        let r = records[0];
        assert_eq!(
            (r.slot, r.seq, r.boot, r.kind),
            (before.slot, before.seq, before.boot, Kind::UnexplainedReset)
        );
        let text = text_of(&s, r.slot);
        assert!(
            text.contains("reason: nothing recorded a crash: a hardware watchdog, a firmware reset or a hang with interrupts off\npmic: last power-off by PS_HOLD; this boot by hard reset, cold\nlast kernel log:\n"),
            "{text}"
        );
        assert!(text.ends_with("[ 5.0] xhci: controller fault\n"), "{text}");
        assert!(
            !text.contains("this boot\n[") && !text.contains("[ 0.1] this boot"),
            "{text}"
        );
        assert!(!s.amend_unexplained(b"again"), "once");
        // The next record still numbers after it, and survives a reset.
        s.record_requested_reset(b"later");
        let s = reset(s);
        let seqs: Vec<_> = s.records().flatten().map(|r| r.seq).collect();
        assert_eq!(seqs.len(), 2);
        assert!(
            seqs.contains(&before.seq) && seqs.contains(&(before.seq + 1)),
            "{seqs:?}"
        );
    }

    #[test]
    fn no_vanished_boot_nothing_to_amend() {
        let mut s = Store::open(fresh()).unwrap();
        s.begin_boot();
        assert!(!s.amend_unexplained(b"x"));
        s.note_clean_end();
        let mut s = reset(s);
        assert_eq!(s.begin_boot(), LastBoot::Clean);
        assert!(!s.amend_unexplained(b"x"));
        assert_eq!(s.records().flatten().count(), 0);
    }

    #[test]
    fn a_requested_reset_says_why_and_keeps_the_log() {
        let mut s = Store::open(fresh()).unwrap();
        s.begin_boot();
        s.console_write(b"[ 9.0] xhci: halted\n");
        s.record_requested_reset(b"the network is dead");
        s.note_clean_end();

        let mut s = reset(s);
        assert_eq!(s.begin_boot(), LastBoot::Clean);
        assert_eq!(s.crashes_in_a_row(), 0);
        let r = s.records().flatten().next().unwrap();
        assert_eq!((r.kind, r.boot), (Kind::RequestedReset, 1));
        let text = text_of(&s, r.slot);
        assert!(text.contains("reason: the network is dead\n"), "{text}");
        assert!(text.ends_with("[ 9.0] xhci: halted\n"), "{text}");
    }

    #[test]
    fn a_modem_record_keeps_the_boot_running_and_a_reset_after_it_is_still_unexplained() {
        let mut s = Store::open(fresh()).unwrap();
        s.begin_boot();
        s.console_write(b"[ 3.0] wifi: wlfw: the service is up\n");
        s.record_modem(
            b"watchdog",
            b"err_qdi.c:456:EF:wlan_process:1:cmnos_thread.c:3986:Asserted",
        );
        // The board resets before anything else: the next boot finds this one still running.
        let mut s = reset(s);
        assert_eq!(s.begin_boot(), LastBoot::Vanished);
        assert_eq!(s.crashes_in_a_row(), 1);
        let records: Vec<_> = s.records().flatten().collect();
        assert_eq!(records.len(), 2);
        let modem = records.iter().find(|r| r.kind == Kind::Modem).unwrap();
        let vanished = records
            .iter()
            .find(|r| r.kind == Kind::UnexplainedReset)
            .unwrap();
        assert_eq!((modem.boot, vanished.boot), (1, 1));
        // The modem's record was numbered before the reset and kept: the next one comes after it.
        assert!(vanished.seq > modem.seq, "{} {}", vanished.seq, modem.seq);
        let text = text_of(&s, modem.slot);
        assert!(
            text.starts_with("The modem DSP failed\ncause: watchdog\nreason: err_qdi.c:456:"),
            "{text}"
        );
        assert!(
            text.ends_with("[ 3.0] wifi: wlfw: the service is up\n"),
            "{text}"
        );
        assert_eq!(Kind::Modem.name(), "modem");
    }

    #[test]
    fn a_long_reason_is_cut() {
        let mut s = Store::open(fresh()).unwrap();
        s.begin_boot();
        s.record_requested_reset(&vec![b'x'; 100_000]);
        let text = text_of(&s, 0);
        assert!(text.len() < 1000, "{}", text.len());
    }

    #[test]
    fn a_clean_end_is_no_crash_and_clears_the_count() {
        let mut s = Store::open(fresh()).unwrap();
        s.begin_boot();
        s.write_record(Kind::Panic, &[b"x"]);
        s.note_crash();
        let mut s = reset(s);
        s.begin_boot();
        s.note_clean_end();
        let mut s = reset(s);
        assert_eq!(s.begin_boot(), LastBoot::Clean);
        assert_eq!(s.crashes_in_a_row(), 0);
        assert_eq!(s.records().flatten().count(), 1);
    }

    #[test]
    fn crashes_count_until_the_system_stays_up() {
        let mut s = Store::open(fresh()).unwrap();
        for expected in 1..=3 {
            s.begin_boot();
            s.write_record(Kind::Panic, &[b"x"]);
            s.note_crash();
            assert_eq!(s.crashes_in_a_row(), expected);
            s = reset(s);
        }
        s.begin_boot();
        s.note_stable();
        assert_eq!(s.crashes_in_a_row(), 0);
        s.write_record(Kind::Panic, &[b"x"]);
        s.note_crash();
        assert_eq!(s.crashes_in_a_row(), 1);
    }

    #[test]
    fn the_oldest_record_goes_first() {
        let mut s = Store::open(fresh()).unwrap();
        s.begin_boot();
        for i in 0..RECORDS + 2 {
            let text = std::format!("crash {i}");
            s.write_record(Kind::Panic, &[text.as_bytes()]);
        }
        let mut seqs: Vec<u32> = s.records().flatten().map(|r| r.seq).collect();
        seqs.sort();
        assert_eq!(seqs, [2, 3, 4, 5, 6, 7, 8, 9]);
        let newest = s.records().flatten().find(|r| r.seq == 9).unwrap();
        assert_eq!(text_of(&s, newest.slot), "crash 9");
    }

    #[test]
    fn a_long_record_is_cut_at_the_slot() {
        let mut s = Store::open(fresh()).unwrap();
        s.begin_boot();
        let long = vec![b'x'; s.text_capacity() + 1000];
        s.write_record(Kind::Panic, &[b"head\n", &long]);
        let r = s.records().flatten().next().unwrap();
        assert_eq!(r.len, s.text_capacity());
        assert!(text_of(&s, 0).starts_with("head\n"));
    }

    #[test]
    fn a_long_record_keeps_the_newest_lines_of_its_log() {
        let mut s = Store::open(fresh()).unwrap();
        s.begin_boot();
        let mut log = vec![b'o'; s.text_capacity()];
        log.extend_from_slice(b"the newest line\n");
        s.write_record(Kind::Panic, &[b"head\n", b"last kernel log:\n", &log]);
        let r = s.records().flatten().next().unwrap();
        assert_eq!(r.len, s.text_capacity());
        let text = text_of(&s, 0);
        assert!(text.starts_with("head\nlast kernel log:\no"));
        assert!(text.ends_with("the newest line\n"));
    }

    #[test]
    fn a_report_longer_than_a_record_is_cut_at_the_slot() {
        let mut s = Store::open(fresh()).unwrap();
        s.begin_boot();
        let report = vec![b'r'; s.text_capacity() + 1000];
        s.write_record(Kind::Panic, &[&report, b"the log"]);
        let r = s.records().flatten().next().unwrap();
        assert_eq!(r.len, s.text_capacity());
        assert!(!text_of(&s, 0).contains("the log"));
    }

    #[test]
    fn records_read_in_pieces() {
        let mut s = Store::open(fresh()).unwrap();
        s.begin_boot();
        s.write_record(Kind::Panic, &[b"0123456789"]);
        let mut out = [0u8; 4];
        assert_eq!(s.read_record(0, 8, &mut out), 2);
        assert_eq!(&out[..2], b"89");
        assert_eq!(s.read_record(0, 10, &mut out), 0);
        assert_eq!(s.read_record(0, 100, &mut out), 0);
        assert_eq!(s.read_record(RECORDS, 0, &mut out), 0);
    }

    #[test]
    fn an_erased_record_is_gone_and_its_slot_is_used_again() {
        let mut s = Store::open(fresh()).unwrap();
        s.begin_boot();
        s.write_record(Kind::Panic, &[b"one"]);
        s.write_record(Kind::Panic, &[b"two"]);
        s.erase_record(0);
        assert!(s.records().next().unwrap().is_none());
        s.write_record(Kind::Panic, &[b"three"]);
        assert_eq!(text_of(&s, 0), "three");
        assert_eq!(text_of(&s, 1), "two");
    }

    #[test]
    fn the_console_wraps_and_keeps_its_end() {
        let mut s = Store::open(fresh()).unwrap();
        s.begin_boot();
        let capacity = s.geometry.console_capacity();
        let mut expected = Vec::new();
        for i in 0..capacity / 8 {
            let line = std::format!("{i:011}\n");
            s.console_write(line.as_bytes());
            expected.extend_from_slice(line.as_bytes());
        }
        s.begin_boot();
        let len = s.last_console_len();
        assert!(len > capacity - BLOCK_DATA && len <= capacity, "{len}");
        let tail = &expected[expected.len() - len..];
        let mut out = vec![0u8; len];
        assert_eq!(s.read_last_console(0, &mut out), len);
        assert_eq!(out, tail);
        // Read across the seam, in pieces.
        let mut piece = [0u8; 100];
        let at = len - 50;
        assert_eq!(s.read_last_console(at, &mut piece), 50);
        assert_eq!(piece[..50], tail[at..]);
        let seam = capacity - s.console_span(capacity as u64 * 2 - 1).0;
        let mut piece = [0u8; 300];
        if seam + 300 <= len {
            assert_eq!(s.read_last_console(seam, &mut piece), 300);
            assert_eq!(piece, tail[seam..seam + 300]);
        }
    }

    #[test]
    fn a_console_of_many_short_writes_reads_back() {
        let mut s = Store::open(fresh()).unwrap();
        s.begin_boot();
        let mut expected = Vec::new();
        for i in 0..600 {
            let line = std::format!("[{i}] {}\n", "x".repeat(i % 300));
            s.console_write(line.as_bytes());
            expected.extend_from_slice(line.as_bytes());
        }
        let mut out = vec![0u8; expected.len()];
        assert_eq!(s.read_console(0, &mut out), expected.len());
        assert_eq!(out, expected);
    }

    #[test]
    fn a_console_write_longer_than_the_console_keeps_the_end() {
        let mut s = Store::open(fresh()).unwrap();
        s.begin_boot();
        let capacity = s.geometry.console_capacity();
        for extra in [100, 3 * capacity + 100] {
            let text: Vec<u8> = (0..capacity + extra).map(|i| (i % 251) as u8).collect();
            s.begin_boot();
            s.console_write(&text);
            let mut out = vec![0u8; capacity];
            let n = s.read_console(0, &mut out);
            assert_eq!(n, capacity - BLOCK_DATA + 100 % BLOCK_DATA);
            assert_eq!(out[..n], text[text.len() - n..]);
        }
    }

    #[test]
    fn the_last_console_stays_for_one_boot() {
        let mut s = Store::open(fresh()).unwrap();
        s.begin_boot();
        s.console_write(b"one\n");
        s.note_clean_end();
        let mut s = reset(s);
        s.begin_boot();
        s.console_write(b"two\n");
        assert_eq!(last_console(&s), "one\n");
        s.note_clean_end();
        let mut s = reset(s);
        s.begin_boot();
        assert_eq!(last_console(&s), "two\n");
    }

    #[test]
    fn a_header_far_from_ours_starts_over() {
        let mut s = Store::open(fresh()).unwrap();
        s.begin_boot();
        s.write_record(Kind::Panic, &[b"x"]);
        s.note_crash();
        let mut ram = s.mem;
        for copy in 0..COPIES {
            let at = copy * HEADER_LEN;
            ram.0[at + 8..at + 32].iter_mut().for_each(|b| *b ^= 0xa5);
        }
        let mut s = Store::open(ram).unwrap();
        assert_eq!(s.begin_boot(), LastBoot::Unknown);
        assert_eq!(s.boot(), 1);
        assert!(s.records().all(|r| r.is_none()));
    }

    /// What the laptop's first firmware reset did: the version byte lost its one bit.
    #[test]
    fn a_header_with_a_flipped_bit_is_repaired() {
        let mut s = Store::open(fresh()).unwrap();
        s.begin_boot();
        s.write_record(Kind::Panic, &[b"kept"]);
        s.note_crash();
        let mut ram = s.mem;
        ram.0[8] ^= 1;
        assert_eq!(foreign_contents(&ram), None);
        assert_eq!(repairable_header_errors(&ram), Some(1));
        let mut s = Store::open(ram).unwrap();
        assert_eq!(repairable_header_errors(&s.mem), Some(0));
        assert_eq!(s.begin_boot(), LastBoot::Crashed);
        assert_eq!(text_of(&s, 0), "kept");
    }

    #[test]
    fn a_header_that_lost_the_same_bit_everywhere_is_repaired() {
        let mut s = Store::open(fresh()).unwrap();
        s.begin_boot();
        s.write_record(Kind::Panic, &[b"kept"]);
        let mut ram = s.mem;
        for copy in 0..COPIES {
            ram.0[copy * HEADER_LEN + 8] ^= 1;
        }
        assert_eq!(repairable_header_errors(&ram), Some(3));
        let s = Store::open(ram).unwrap();
        assert_eq!(repairable_header_errors(&s.mem), Some(0));
        assert_eq!(text_of(&s, 0), "kept");
    }

    #[test]
    fn one_overwritten_copy_of_the_header_is_outvoted() {
        let mut s = Store::open(fresh()).unwrap();
        s.begin_boot();
        s.write_record(Kind::Panic, &[b"kept"]);
        let mut ram = s.mem;
        ram.0[HEADER_LEN..2 * HEADER_LEN].fill(0x55);
        let s = Store::open(ram).unwrap();
        assert_eq!(text_of(&s, 0), "kept");
    }

    #[test]
    fn a_region_of_the_last_version_starts_over() {
        let mut s = Store::open(fresh()).unwrap();
        s.begin_boot();
        let mut header = s.geometry.header();
        header[8] = 2;
        let crc = crc32(&[&header[..28]]);
        header[28..32].copy_from_slice(&crc.to_le_bytes());
        let mut ram = s.mem;
        for copy in 0..COPIES {
            ram.write(copy * HEADER_LEN, &header);
        }
        assert!(foreign_contents(&ram).is_some());
    }

    /// A region written by the first version of this format holds one header and then its state.
    #[test]
    fn a_region_of_the_first_version_starts_over() {
        let mut s = Store::open(fresh()).unwrap();
        s.begin_boot();
        let geometry = s.geometry;
        let mut ram = s.mem;
        ram.0[..4096].fill(0);
        ram.0[..HEADER_LEN].copy_from_slice(&geometry.header());
        ram.0[8] = 1;
        ram.0[64..64 + STATE_LEN].fill(0x5a);
        assert!(foreign_contents(&ram).is_some());
        let mut s = Store::open(ram).unwrap();
        assert_eq!(s.begin_boot(), LastBoot::Unknown);
    }

    #[test]
    fn a_region_of_another_size_starts_over() {
        let mut s = Store::open(fresh()).unwrap();
        s.begin_boot();
        s.write_record(Kind::Panic, &[b"x"]);
        let mut ram = s.mem;
        ram.0.truncate(1024 * 1024);
        let mut s = Store::open(ram).unwrap();
        assert_eq!(s.begin_boot(), LastBoot::Unknown);
        assert!(s.records().all(|r| r.is_none()));
    }

    #[test]
    fn one_bad_copy_of_the_state_is_outvoted() {
        let mut s = Store::open(fresh()).unwrap();
        s.begin_boot();
        s.note_crash();
        let mut ram = s.mem;
        ram.0[STATE_AT..STATE_AT + STATE_LEN + PARITY].fill(0x55);
        let mut s = Store::open(ram).unwrap();
        assert_eq!(s.begin_boot(), LastBoot::Crashed);
        assert_eq!(s.boot(), 2);
    }

    #[test]
    fn a_state_lost_in_all_copies_costs_only_the_counters() {
        let mut s = Store::open(fresh()).unwrap();
        s.begin_boot();
        s.write_record(Kind::Panic, &[b"kept"]);
        s.note_crash();
        let mut ram = s.mem;
        ram.0[STATE_AT..STATE_AT + protected_len(STATE_LEN)].fill(0x55);
        let mut s = Store::open(ram).unwrap();
        assert_eq!(s.begin_boot(), LastBoot::Unknown);
        assert_eq!(s.boot(), 1);
        assert_eq!(text_of(&s, 0), "kept");
        s.write_record(Kind::Panic, &[b"next"]);
        let seqs: Vec<u32> = s.records().flatten().map(|r| r.seq).collect();
        assert_eq!(seqs, [0, 1], "new records go on after the old ones");
    }

    #[test]
    fn wrong_bytes_in_a_record_are_corrected() {
        let mut s = Store::open(fresh()).unwrap();
        s.begin_boot();
        s.write_record(Kind::Panic, &[b"one"]);
        let at = s.text_base(0);
        let header = s.geometry.record_at(0);
        s.mem.0[at] ^= 0xff;
        s.mem.0[header + 5] ^= 0xff;
        let r = s.records().next().unwrap().unwrap();
        assert!(!r.damaged);
        assert_eq!(text_of(&s, 0), "one");
    }

    #[test]
    fn a_block_beyond_repair_marks_the_text_damaged_and_keeps_it() {
        let mut s = Store::open(fresh()).unwrap();
        s.begin_boot();
        s.write_record(Kind::Panic, &[b"one"]);
        s.write_record(Kind::Panic, &[b"two"]);
        let at = s.text_base(0);
        s.mem.0[at] ^= 0x01;
        s.mem.0[at + BLOCK_DATA..at + BLOCK]
            .iter_mut()
            .for_each(|b| *b ^= 0xa5);
        let r = s.records().next().unwrap().unwrap();
        assert!(r.damaged);
        assert_eq!(text_of(&s, 0), "nne");
        assert!(!s.records().nth(1).unwrap().unwrap().damaged);
        s.begin_boot();
        assert_eq!(scrubbed(&mut s).lost_blocks, 1);
    }

    #[test]
    fn a_torn_console_header_falls_back_on_its_other_copy() {
        let mut s = Store::open(fresh()).unwrap();
        s.begin_boot();
        s.console_write(b"text");
        let at = s.geometry.console_at(s.state.active_console);
        s.mem.0[at..at + CONSOLE_FIELDS].fill(0x55);
        s.mem.0[at + CONSOLE_FIELDS..at + CONSOLE_FIELDS + PARITY].fill(0xaa);
        s.begin_boot();
        assert_eq!(last_console(&s), "text");
    }

    #[test]
    fn the_end_of_a_console_written_over_the_parity_shows_as_it_is() {
        let mut s = Store::open(fresh()).unwrap();
        s.begin_boot();
        s.console_write(b"text");
        let at = s.geometry.console_at(s.state.active_console) + CONSOLE_HEADER;
        s.mem.0[at + BLOCK_DATA..at + BLOCK].fill(0x55);
        s.begin_boot();
        assert_eq!(last_console(&s), "text");
    }

    #[test]
    fn a_record_header_lost_in_all_copies_costs_that_record() {
        let mut s = Store::open(fresh()).unwrap();
        s.begin_boot();
        s.write_record(Kind::Panic, &[b"one"]);
        s.write_record(Kind::Panic, &[b"two"]);
        let base = s.geometry.record_at(0);
        s.mem.0[base..base + RECORD_PREFIX].fill(0x55);
        assert!(s.records().next().unwrap().is_none());
        assert_eq!(s.read_record(0, 0, &mut [0; 8]), 0);
        assert_eq!(text_of(&s, 1), "two");
        // That slot is used again like an empty one.
        s.write_record(Kind::Panic, &[b"three"]);
        assert_eq!(text_of(&s, 0), "three");
    }

    /// Flips about one bit in `one_in`, all over the region, by a fixed sequence that `seed` picks.
    fn decay(ram: &mut Ram, one_in: u64, seed: u64) {
        let mut x = 0x2545_f491_4f6c_dd1d ^ seed;
        for byte in ram.0.iter_mut() {
            for bit in 0..8 {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                if x.is_multiple_of(one_in) {
                    *byte ^= 1 << bit;
                }
            }
        }
    }

    fn a_report() -> Vec<u8> {
        (0..30_000)
            .map(|i| b"reason: boom at kernel/core/src/x.rs:12\n"[i % 40])
            .collect()
    }

    /// 1.57% is what the laptop's reset did when it was measured, and 3.1% leaves that a margin.
    #[test]
    fn a_crash_comes_back_exact_from_a_reset_that_flips_bits() {
        let report = a_report();
        for (one_in, seed) in [(64, 0), (64, 77), (64, 1234), (32, 0), (32, 99)] {
            let mut s = Store::open(fresh()).unwrap();
            s.begin_boot();
            s.write_record(Kind::Panic, &[b"older"]);
            s.console_write(b"[ 1.0] a line of the console\n");
            s.write_record(Kind::Panic, &[&report]);
            s.note_crash();
            let mut ram = s.mem;
            decay(&mut ram, one_in, seed);
            assert!(
                repairable_header_errors(&ram).unwrap() > 0,
                "the header counts as ours"
            );
            let mut s = Store::open(ram).unwrap();
            assert_eq!(s.begin_boot(), LastBoot::Crashed, "the state holds");
            assert_eq!((s.boot(), s.crashes_in_a_row()), (2, 1));
            let scrub = scrubbed(&mut s);
            assert!(
                scrub.corrected > 1000 && scrub.lost_blocks == 0,
                "{scrub:?}"
            );
            let r = s.records().flatten().find(|r| r.len == report.len());
            let r = r.expect("the record is found");
            assert_eq!(r.kind, Kind::Panic);
            assert!(!r.damaged);
            let mut out = vec![0u8; r.len];
            s.read_record(r.slot, 0, &mut out);
            assert!(out == report, "the text is exact");
            assert_eq!(last_console(&s), "[ 1.0] a line of the console\n");
        }
    }

    /// A watchdog leaves only the console, with the last line's block not yet full.
    #[test]
    fn a_vanished_boot_comes_back_exact_from_a_reset_that_flips_bits() {
        let mut s = Store::open(fresh()).unwrap();
        s.begin_boot();
        let mut expected = Vec::new();
        for i in 0..1000 {
            let line = std::format!("[{i}] a line of the kernel log\n");
            s.console_write(line.as_bytes());
            expected.extend_from_slice(line.as_bytes());
        }
        let mut ram = s.mem;
        decay(&mut ram, 64, 5);
        let mut s = Store::open(ram).unwrap();
        assert_eq!(s.begin_boot(), LastBoot::Vanished);
        let r = s.records().flatten().next().unwrap();
        assert_eq!(r.kind, Kind::UnexplainedReset);
        assert!(!r.damaged);
        let text = text_of(&s, r.slot);
        assert!(text.ends_with(std::str::from_utf8(&expected[expected.len() - 1000..]).unwrap()));
        assert!(last_console(&s).as_bytes() == expected);
    }

    /// Without writing back what it corrected, each reset would add to the last.
    #[test]
    fn flips_do_not_add_up_over_many_resets() {
        let report = a_report();
        let mut s = Store::open(fresh()).unwrap();
        s.begin_boot();
        s.write_record(Kind::Panic, &[&report]);
        s.note_crash();
        for reset_number in 0..30 {
            let mut ram = s.mem;
            decay(&mut ram, 64, reset_number);
            s = Store::open(ram).unwrap();
            assert_eq!(s.begin_boot(), LastBoot::Crashed, "reset {reset_number}");
            assert_eq!(scrubbed(&mut s).lost_blocks, 0, "reset {reset_number}");
            s.note_crash();
        }
        assert_eq!(s.crashes_in_a_row(), 31);
        let mut out = vec![0u8; report.len()];
        s.read_record(0, 0, &mut out);
        assert!(out == report);
    }

    /// A second crash before the background got to it.
    #[test]
    fn two_resets_before_any_write_back_still_come_back_exact() {
        let report = a_report();
        let mut s = Store::open(fresh()).unwrap();
        s.begin_boot();
        s.write_record(Kind::Panic, &[&report]);
        s.note_crash();
        for reset_number in 0..2 {
            let mut ram = s.mem;
            decay(&mut ram, 64, reset_number);
            s = Store::open(ram).unwrap();
            assert_eq!(s.begin_boot(), LastBoot::Crashed);
            s.note_crash();
        }
        assert_eq!(scrubbed(&mut s).lost_blocks, 0);
        let mut out = vec![0u8; report.len()];
        s.read_record(0, 0, &mut out);
        assert!(out == report);
    }

    #[test]
    fn writing_back_once_is_enough() {
        let mut s = Store::open(fresh()).unwrap();
        s.begin_boot();
        s.write_record(Kind::Panic, &[&a_report()]);
        s.note_crash();
        let mut ram = s.mem;
        decay(&mut ram, 64, 1);
        let mut s = Store::open(ram).unwrap();
        s.begin_boot();
        assert!(scrubbed(&mut s).corrected > 1000);
        let mem = s.mem;
        let mut s = Store::attach(mem).unwrap_or_else(|_| panic!());
        s.begin_boot();
        assert_eq!(scrubbed(&mut s), Scrub::default());
    }

    #[test]
    fn the_newest_record_is_known_from_the_headers() {
        let mut s = Store::open(fresh()).unwrap();
        s.begin_boot();
        assert_eq!(s.newest_record(), None);
        for i in 0..RECORDS + 3 {
            s.write_record(Kind::Panic, &[std::format!("{i}").as_bytes()]);
        }
        let newest = s.newest_record().unwrap();
        assert_eq!(text_of(&s, newest.slot), std::format!("{}", RECORDS + 2));
    }

    #[test]
    fn pieces_of_a_line_count_from_the_commit_on() {
        let mut s = Store::open(fresh()).unwrap();
        s.begin_boot();
        s.console_write(b"whole\n");
        s.console_append(b"half");
        let mut ram = Ram(s.mem.0.clone());
        let mut after = Store::open(ram.clone()).unwrap();
        after.begin_boot();
        assert_eq!(last_console(&after), "whole\n");
        s.console_append(b" and the rest\n");
        s.console_commit();
        ram.0.copy_from_slice(&s.mem.0);
        let mut after = Store::open(ram).unwrap();
        after.begin_boot();
        assert_eq!(last_console(&after), "whole\nhalf and the rest\n");
    }

    #[test]
    fn far_too_many_flips_cost_the_log_and_nothing_else() {
        for one_in in [2, 3, 5, 8, 12, 16, 24] {
            let mut s = Store::open(fresh()).unwrap();
            s.begin_boot();
            s.write_record(Kind::Panic, &[&a_report()]);
            s.console_write(b"text");
            s.note_crash();
            let mut ram = s.mem;
            decay(&mut ram, one_in, 3);
            let mut s = Store::open(ram).unwrap();
            s.begin_boot();
            s.console_write(b"next boot");
            let mut out = vec![0u8; 512];
            for slot in 0..RECORDS {
                s.read_record(slot, 0, &mut out);
            }
            s.read_last_console(0, &mut out);
            s.write_record(Kind::Panic, &[b"after"]);
            assert!(s.records().flatten().any(|r| r.len == 5));
        }
    }

    #[test]
    fn a_reset_during_a_record_loses_at_most_that_record() {
        let mut s = Store::open(fresh()).unwrap();
        s.begin_boot();
        for i in 0..RECORDS {
            s.write_record(Kind::Panic, &[std::format!("{i}").as_bytes()]);
        }
        // The next record goes over slot 0: its headers are cleared first, and they come last.
        let mem = Ram(s.mem.0.clone());
        let g = s.geometry;
        let mut torn = mem;
        torn.0[g.record_at(0)..g.record_at(0) + RECORD_PREFIX].fill(0);
        torn.0[g.record_at(0) + RECORD_PREFIX] = b'!';
        let s = Store::open(torn).unwrap();
        assert!(s.records().next().unwrap().is_none());
        assert_eq!(s.records().flatten().count(), RECORDS - 1);
    }

    #[test]
    fn garbage_in_the_console_stays_inside_it() {
        let mut s = Store::open(fresh()).unwrap();
        s.begin_boot();
        s.console_write(b"text");
        let g = s.geometry;
        s.console_written = u64::MAX / 2;
        s.save_console_header();
        s.begin_boot();
        // The written count says more than fits: the reader stays within the zone, whatever the bytes are.
        let mut out = vec![0u8; g.console_capacity() + 10];
        let n = s.read_last_console(0, &mut out);
        assert_eq!(n, s.last_console_len());
        assert!(n <= g.console_capacity());
    }
    fn region_of(version: u32) -> Ram {
        let mut mem = fresh();
        let g = Geometry::for_version(mem.size(), version).unwrap();
        Store::<Ram>::write_header(&mut mem, &g);
        mem
    }

    #[test]
    fn the_header_survey_tells_format_3_from_4_and_neither() {
        assert_eq!(header_format(&region_of(3)), Some(3));
        assert_eq!(header_format(&region_of(4)), Some(4));
        // A few flipped bits still count.
        let mut mem = region_of(3);
        mem.0[1] ^= 0x01;
        mem.0[40] ^= 0x80;
        assert_eq!(header_format(&mem), Some(3));
        // Random bytes are neither.
        let mut x = 0x1234_5678_9abc_def0u64;
        let random = Ram((0..SIZE)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 24) as u8
            })
            .collect());
        assert_eq!(header_format(&random), None);
        assert_eq!(header_format(&Ram(vec![0; SIZE])), None);
        // A region of another size.
        assert_eq!(header_format(&Ram(vec![0; 1024])), None);
        // The existing survey functions are as they were: a format-3 header is foreign to format 4's.
        assert!(foreign_contents(&region_of(3)).is_some());
        assert!(foreign_contents(&region_of(4)).is_none());
    }

    #[test]
    fn a_fresh_format_makes_an_epoch_block() {
        let s = Store::format_seeded(fresh(), 0xdead_beef).ok().unwrap();
        assert_ne!(s.epoch(), 0);
        assert_eq!(s.epoch_new(), Some(EpochOrigin::Formatted));
        let mut s = reset(s);
        assert_eq!(s.begin_boot(), LastBoot::Unknown);
        assert_eq!(
            s.epoch_new(),
            None,
            "a reset that keeps the region keeps its id"
        );
        let id = s.epoch();
        s.begin_boot();
        let s = reset(s);
        assert_eq!(s.epoch(), id);
        assert_eq!(read_epoch_block(&s.mem), Ok(id));
    }

    #[test]
    fn two_formats_give_different_ids() {
        let a = Store::format_seeded(fresh(), 1000).ok().unwrap().epoch();
        let b = Store::format_seeded(fresh(), 1001).ok().unwrap().epoch();
        assert_ne!(a, b);
        let c = Store::open_seeded(Ram(vec![0; SIZE]), 77).unwrap().epoch();
        assert_ne!(c, a);
    }

    #[test]
    fn a_region_from_1213_gets_an_epoch_and_keeps_everything() {
        // A format-4 region with a record, as 1213 left it: no epoch block, only whatever RAM held there.
        let mut old = Store::format(fresh()).ok().unwrap();
        old.begin_boot();
        old.write_record(Kind::Panic, &[b"a crash from before epochs"]);
        old.console_write(b"[ 1.0] a line\n");
        let mut mem = old.mem;
        let junk: Vec<u8> = (0..FIRST_ZONE - layout::EPOCH_AT)
            .map(|i| (i * 7 + 3) as u8)
            .collect();
        mem.0[layout::EPOCH_AT..layout::EPOCH_AT + junk.len()].copy_from_slice(&junk);
        let mut s = Store::attach_seeded(mem, 4242).ok().unwrap();
        assert_eq!(s.epoch(), 0);
        s.begin_boot();
        assert_ne!(s.epoch(), 0);
        assert_eq!(s.epoch_new(), Some(EpochOrigin::NoBlock));
        let id = s.epoch();
        let s = reset(s);
        assert_eq!(s.epoch(), id);
        // The record, and the one for the boot that vanished.
        let records: Vec<_> = s.records().flatten().collect();
        assert_eq!(records.len(), 2);
        let old = records.iter().find(|r| r.kind == Kind::Panic).unwrap();
        assert_eq!(text_of(&s, old.slot), "a crash from before epochs");
    }

    #[test]
    fn an_epoch_block_that_fails_the_vote_makes_a_new_epoch() {
        let s = Store::format_seeded(fresh(), 5).ok().unwrap();
        let id = s.epoch();
        let mut mem = s.mem;
        // Tag intact, but every copy overwritten past repair.
        for copy in 0..layout::EPOCH_COPIES {
            let at = layout::EPOCH_AT + copy * layout::EPOCH_COPY + 4;
            for b in &mut mem.0[at..at + 120] {
                *b ^= 0xa5;
            }
        }
        let mut s = Store::attach_seeded(mem, 6).ok().unwrap();
        s.begin_boot();
        assert_eq!(s.epoch_new(), Some(EpochOrigin::Damaged));
        assert_ne!(s.epoch(), id);
    }

    #[test]
    fn a_torn_epoch_block_is_outvoted() {
        let s = Store::format_seeded(fresh(), 5).ok().unwrap();
        let id = s.epoch();
        let mut mem = s.mem;
        let at = layout::EPOCH_AT + layout::EPOCH_COPY;
        mem.0[at..at + EPOCH_FIELDS + PARITY].fill(0);
        let mut s = Store::attach_seeded(mem, 6).ok().unwrap();
        s.begin_boot();
        assert_eq!(s.epoch(), id);
        assert_eq!(s.epoch_new(), None);
    }

    #[test]
    fn the_epoch_survives_the_flip_rate_the_records_see() {
        for seed in 0..40u64 {
            let s = Store::format_seeded(fresh(), seed).ok().unwrap();
            let id = s.epoch();
            let mut mem = s.mem;
            // 1 bit in 64 over the whole first zone, a good deal more than the 1.57% of a firmware reset.
            let mut zone = Ram(mem.0[..FIRST_ZONE].to_vec());
            decay(&mut zone, 64, seed);
            mem.0[..FIRST_ZONE].copy_from_slice(&zone.0);
            let mut s = Store::open_seeded(mem, 999).unwrap();
            s.begin_boot();
            assert_eq!(s.epoch(), id, "seed {seed}");
            assert_eq!(s.epoch_new(), None, "seed {seed}");
        }
    }

    #[test]
    fn the_epoch_block_moves_nothing_else() {
        let a = Store::format(fresh()).ok().unwrap();
        let mut b = Store::format(fresh()).ok().unwrap();
        b.begin_boot();
        let epoch_end = layout::EPOCH_AT + layout::EPOCH_COPIES * layout::EPOCH_COPY;
        let outside = |m: &Ram| {
            let mut v = m.0.clone();
            v[layout::EPOCH_AT..epoch_end].fill(0);
            v
        };
        let mut a = a;
        a.begin_boot();
        assert_eq!(outside(&a.mem), outside(&b.mem));
        // What the block does not use it leaves: the bytes after it are still what RAM held.
        let written = Store::format(fresh()).ok().unwrap();
        assert_eq!(
            written.mem.0[epoch_end..FIRST_ZONE],
            fresh().0[epoch_end..FIRST_ZONE]
        );
    }

    /// `spare_tail`/`format3_tail` are the store's own words for the layout's tail start, and the spare tail must start
    /// where the layout says the tail starts. layout::tail_start and console_at(2) are the same address.
    #[test]
    fn the_third_console_offset_is_the_layouts_tail_start() {
        for size in [
            layout::MIN_SIZE,
            128 * 1024,
            layout::RESERVE_FROM,
            2 * 1024 * 1024,
            16 * 1024 * 1024,
        ] {
            for version in [3, 4] {
                let g = Geometry::for_version(size, version).unwrap();
                assert_eq!(g.console_at(2), layout::tail_start(size, version));
            }
            let r = spare_tail(size).unwrap();
            assert!(r.end > r.start, "{size}");
            assert_eq!(r.end, size);
        }
    }

    /// A format-3 region as the old kernel leaves it after boots that ended clean, each writing `text` to its console.
    fn old_format_region(boots: &[&[u8]]) -> Ram {
        let mut mem = fresh();
        let old = Geometry::for_version(mem.size(), 3).unwrap();
        Store::<Ram>::write_header(&mut mem, &old);
        for text in boots {
            let mut s = Store::attach(mem).ok().unwrap();
            // Played as the old kernel would: it never upgrades.
            s.upgrade_from = None;
            s.begin_boot();
            s.console_write(text);
            s.note_clean_end();
            mem = s.mem;
        }
        mem
    }

    fn kept_console(s: &Store<Ram>) -> Option<String> {
        s.records()
            .flatten()
            .find(|r| r.kind == Kind::UnexplainedReset)
            .map(|r| text_of(s, r.slot))
    }

    #[test]
    fn a_clean_last_boot_in_zone_1_keeps_its_console_across_the_upgrade() {
        // The first boot of a region uses zone 1.
        let mem = old_format_region(&[b"[ 1.0] the old boot's last line\n"]);
        let mut s = Store::attach(mem).ok().unwrap();
        assert_eq!(s.begin_boot(), LastBoot::Clean);
        assert_eq!(s.upgraded_from(), Some(3));
        let text = kept_console(&s).expect("a record");
        assert!(
            text.starts_with(
                "the last boot's console, kept across the format change\nlast kernel log:\n"
            ),
            "{text}"
        );
        assert!(
            text.ends_with("[ 1.0] the old boot's last line\n"),
            "{text}"
        );
        // The next boot finds format 4 and the record still there.
        s.console_write(b"[ 2.0] new\n");
        s.note_clean_end();
        let mut s = reset(s);
        assert_eq!(s.begin_boot(), LastBoot::Clean);
        assert_eq!(s.upgraded_from(), None);
        assert!(
            kept_console(&s)
                .unwrap()
                .ends_with("[ 1.0] the old boot's last line\n")
        );
    }

    #[test]
    fn a_clean_last_boot_in_zone_0_keeps_its_console_across_the_upgrade() {
        let mem = old_format_region(&[b"[ 1.0] first\n", b"[ 2.0] the second boot's last line\n"]);
        let mut s = Store::attach(mem).ok().unwrap();
        assert_eq!(s.begin_boot(), LastBoot::Clean);
        let text = kept_console(&s).expect("a record");
        assert!(
            text.ends_with("[ 2.0] the second boot's last line\n"),
            "{text}"
        );
        // A long console is cut to the record's room, keeping its end.
        let mem = old_format_region(&[b"x", &b"line of text\n".repeat(60_000)]);
        let mut s = Store::attach(mem).ok().unwrap();
        s.begin_boot();
        let text = kept_console(&s).unwrap();
        assert!(
            text.ends_with("line of text\n"),
            "{}",
            &text[text.len() - 20..]
        );
    }

    #[test]
    fn a_region_that_is_already_format_4_makes_no_extra_record() {
        let mut s = Store::format(fresh()).ok().unwrap();
        s.begin_boot();
        s.console_write(b"a line\n");
        s.note_clean_end();
        let mut s = reset(s);
        assert_eq!(s.begin_boot(), LastBoot::Clean);
        assert_eq!(s.records().flatten().count(), 0);
    }

    fn restart_record(s: &Store<Ram>) -> Option<String> {
        s.records()
            .flatten()
            .find(|r| r.kind == Kind::RegionRestarted)
            .map(|r| text_of(s, r.slot))
    }

    fn header_in(mem: &mut Ram, header: &[u8; HEADER_LEN]) {
        for copy in 0..COPIES {
            mem.write(copy * HEADER_LEN, header);
        }
    }

    /// Opens, begins a boot and returns the cause, the record's text and the first record's slot count.
    fn restarted(mem: Ram) -> (RestartCause, String) {
        let mut s = Store::open_seeded(mem, 9).unwrap();
        let cause = s.restart_cause().expect("a cause");
        s.begin_boot();
        let all: Vec<_> = s.records().flatten().collect();
        assert_eq!(all.len(), 1, "the new region's first record, and no other");
        let text = restart_record(&s).expect("a restart record");
        assert!(
            text.starts_with("The saved log's region was started over\nreason: "),
            "{text}"
        );
        assert!(text.ends_with("\nthe records it held are lost\n"), "{text}");
        (cause, text)
    }

    #[test]
    fn a_zeroed_region_is_first_use() {
        let (cause, text) = restarted(Ram(vec![0; SIZE]));
        assert_eq!(cause, RestartCause::FirstUse);
        assert!(text.contains("reason: first use\n"), "{text}");
    }

    #[test]
    fn a_region_of_other_bytes_has_no_header_and_says_what_it_held() {
        let (cause, text) = restarted(fresh());
        let RestartCause::NoHeader {
            zero_pages,
            ones_pages,
            other_pages,
        } = cause
        else {
            panic!("{cause:?}");
        };
        assert_eq!((zero_pages, ones_pages, other_pages), (0, 0, SIZE / 4096));
        assert!(
            text.contains(&format!(
                "reason: no header: 0 pages zero, 0 all ones, {} other\n",
                SIZE / 4096
            )),
            "{text}"
        );
    }

    #[test]
    fn a_header_of_another_format_is_named() {
        for (version, older) in [(2u32, true), (5, false)] {
            let mut mem = fresh();
            let mut h = Geometry::for_size(SIZE).unwrap().header();
            h[8..12].copy_from_slice(&version.to_le_bytes());
            let crc = crc32(&[&h[..28]]);
            h[28..32].copy_from_slice(&crc.to_le_bytes());
            header_in(&mut mem, &h);
            let (cause, text) = restarted(mem);
            assert_eq!(cause, RestartCause::OtherFormat(version));
            let words = if older { "older" } else { "newer" };
            assert!(
                text.contains(&format!(
                    "reason: header of format {version} from an {words} kernel\n"
                )) || text.contains(&format!(
                    "reason: header of format {version} from a {words} kernel\n"
                )),
                "{text}"
            );
        }
    }

    #[test]
    fn a_header_with_too_many_wrong_bits_says_how_many() {
        let mut mem = fresh();
        let mut h = Geometry::for_size(SIZE).unwrap().header();
        h[16] ^= 0xff;
        h[17] ^= 0x0f;
        header_in(&mut mem, &h);
        let (cause, text) = restarted(mem);
        assert_eq!(cause, RestartCause::WrongBits(12));
        assert!(
            text.contains("reason: header with 12 wrong bits after the vote\n"),
            "{text}"
        );
    }

    #[test]
    fn a_header_for_another_size_says_so() {
        let mut mem = fresh();
        let h = Geometry::for_size(SIZE / 2).unwrap().header();
        header_in(&mut mem, &h);
        let (cause, text) = restarted(mem);
        assert_eq!(cause, RestartCause::SizeChanged);
        assert!(text.contains("reason: region size changed\n"), "{text}");
    }

    #[test]
    fn a_format_3_header_over_our_epoch_block_means_an_old_kernel_started_it_over() {
        // What a kernel from before patch 1213 leaves: it lays out format 3 and never touches the epoch block.
        let s = Store::format_seeded(fresh(), 5).ok().unwrap();
        let id = s.epoch();
        let mut mem = s.mem;
        let old = Geometry::for_version(SIZE, 3).unwrap();
        Store::<Ram>::write_header(&mut mem, &old);
        let mut s = Store::open_seeded(mem, 6).unwrap();
        assert_eq!(s.restart_cause(), Some(RestartCause::OldKernel(3)));
        assert_eq!(s.upgraded_from(), None, "before the upgrade");
        s.begin_boot();
        assert_eq!(s.upgraded_from(), Some(3));
        assert_eq!(s.epoch(), id, "the id is kept");
        let text = restart_record(&s).expect("a record");
        assert!(
            text.contains("reason: a kernel of format 3 laid it out again"),
            "{text}"
        );
        // A format-3 region that never had an epoch block is an upgrade, not a restart.
        let mut mem = fresh();
        Store::<Ram>::write_header(&mut mem, &old);
        let s = Store::open_seeded(mem, 6).unwrap();
        assert_eq!(s.restart_cause(), None);
    }

    #[test]
    fn a_region_that_is_ours_has_no_restart() {
        let s = Store::format(fresh()).ok().unwrap();
        let mut s = reset(s);
        assert_eq!(s.restart_cause(), None);
        s.begin_boot();
        assert!(restart_record(&s).is_none());
    }
}
