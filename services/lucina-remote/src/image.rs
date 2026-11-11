//! An ext2 image on its way to a partition: the blocks its bitmaps say are used, as a stream, with zero runs not sent.
//!
//! `lucina-rootimg pack` makes the stream on the build machine and `lucina-install-disk` applies it. Free blocks are not written:
//! ext2 never reads them, and a /data partition of 100 GiB takes a few megabytes.
//!
//! Block 0 holds the superblock, so it goes last: a partition that lost power half way has no valid superblock of this image.
//! Before the first extent the applying side overwrites what is in block 0 for the same reason.
//!
//! The stream: magic, block count, extent count, UUID, SHA-256; then each extent: first block, block count, kind (0 data, 1 zero)
//! and for data its bytes. The sum covers each extent's first block and count and its bytes, zeros included, so it can be
//! computed again from what the disk holds.

use std::io::{self, Read, Write};

use sha2::{Digest, Sha256};

pub const BLOCK: usize = 4096;
const MAGIC: &[u8; 16] = b"LUCINA-EXT2IMG1\n";
const SUPER_OFFSET: usize = 1024;
const EXT2_MAGIC: u16 = 0xef53;
/// `s_state`: cleanly unmounted.
pub const STATE_VALID: u16 = 1;
/// `s_state` bit: the kernel found errors in the file system (`EXT2_ERROR_FS`).
pub const STATE_ERROR: u16 = 2;
const OFFSET_STATE: usize = SUPER_OFFSET + 58;

/// Whether a file system with this `s_state` goes through e2fsck before it is mounted. Only exactly "cleanly unmounted" skips
/// the check: "mounted, not marked clean" (0), the error flag (2, also with VALID: 3) and the orphan bit (4) all force it.
/// /data has no journal, so this check is the only protection after an unclean end.
pub fn needs_check(state: u16) -> bool {
    state != STATE_VALID
}

/// What the superblock says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ext2Info {
    pub blocks: u64,
    pub uuid: [u8; 16],
    pub label: String,
    pub state: u16,
}

fn le16(d: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(d[at..at + 2].try_into().unwrap())
}

fn le32(d: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(d[at..at + 4].try_into().unwrap())
}

/// Reads the superblock from a partition's first block. `None` if it is not an ext2 file system with 4096-byte blocks.
pub fn parse_superblock(block0: &[u8]) -> Option<Ext2Info> {
    let sb = block0.get(SUPER_OFFSET..SUPER_OFFSET + 1024)?;
    if le16(sb, 56) != EXT2_MAGIC || le32(sb, 24) != 2 {
        return None;
    }
    let mut uuid = [0u8; 16];
    uuid.copy_from_slice(&sb[104..120]);
    let label = &sb[120..136];
    let end = label.iter().position(|b| *b == 0).unwrap_or(label.len());
    Some(Ext2Info {
        blocks: le32(sb, 4) as u64,
        uuid,
        label: String::from_utf8_lossy(&label[..end]).into_owned(),
        state: le16(sb, 58),
    })
}

/// `block0` with `s_state` set.
pub fn with_state(block0: &[u8], state: u16) -> Vec<u8> {
    let mut b = block0.to_vec();
    b[OFFSET_STATE..OFFSET_STATE + 2].copy_from_slice(&state.to_le_bytes());
    b
}

pub fn uuid_hex(uuid: &[u8; 16]) -> String {
    uuid.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Extent {
    pub first: u64,
    pub count: u64,
    pub zero: bool,
}

/// The stream's header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub blocks: u64,
    pub extents: u64,
    pub uuid: [u8; 16],
    pub sha256: [u8; 32],
}

impl Header {
    pub fn write(&self, out: &mut impl Write) -> io::Result<()> {
        out.write_all(MAGIC)?;
        out.write_all(&self.blocks.to_le_bytes())?;
        out.write_all(&self.extents.to_le_bytes())?;
        out.write_all(&self.uuid)?;
        out.write_all(&self.sha256)
    }

    pub fn read(input: &mut impl Read) -> Result<Header, String> {
        let mut b = [0u8; 16 + 8 + 8 + 16 + 32];
        input
            .read_exact(&mut b)
            .map_err(|e| format!("the image's header: {e}"))?;
        if &b[..16] != MAGIC {
            return Err("the stream has no image here".into());
        }
        let mut uuid = [0u8; 16];
        uuid.copy_from_slice(&b[32..48]);
        let mut sha256 = [0u8; 32];
        sha256.copy_from_slice(&b[48..80]);
        Ok(Header {
            blocks: u64::from_le_bytes(b[16..24].try_into().unwrap()),
            extents: u64::from_le_bytes(b[24..32].try_into().unwrap()),
            uuid,
            sha256,
        })
    }
}

/// Adds an extent to a running sum: its address, then its bytes through `bytes`.
pub fn sum_extent_head(sum: &mut Sha256, e: &Extent) {
    sum.update(e.first.to_le_bytes());
    sum.update(e.count.to_le_bytes());
}

/// Which blocks of an ext2 image are used, from its group descriptors and block bitmaps. `read` gives whole blocks of the image.
pub fn used_blocks(mut read: impl FnMut(u64) -> io::Result<Vec<u8>>) -> Result<Vec<bool>, String> {
    let io = |e: io::Error| format!("reading the image: {e}");
    let block0 = read(0).map_err(io)?;
    let info = parse_superblock(&block0).ok_or("not an ext2 image with 4096-byte blocks")?;
    let sb = &block0[SUPER_OFFSET..SUPER_OFFSET + 1024];
    let first_data = le32(sb, 20) as u64;
    let per_group = le32(sb, 32) as u64;
    if first_data != 0 || per_group == 0 {
        return Err("an unexpected ext2 geometry".into());
    }
    let groups = info.blocks.div_ceil(per_group);
    let mut used = vec![false; info.blocks as usize];
    let mut descriptors = Vec::new();
    for i in 0..(groups * 32).div_ceil(BLOCK as u64) {
        descriptors.extend(read(1 + i).map_err(io)?);
    }
    for g in 0..groups {
        let bitmap_at = le32(&descriptors, g as usize * 32) as u64;
        let bitmap = read(bitmap_at).map_err(io)?;
        let start = g * per_group;
        let n = per_group.min(info.blocks - start);
        for i in 0..n {
            if bitmap[(i / 8) as usize] & (1 << (i % 8)) != 0 {
                used[(start + i) as usize] = true;
            }
        }
    }
    // The superblock and the descriptors, whatever the bitmap says.
    used[0] = true;
    Ok(used)
}

/// The extents to send: the used blocks in runs of the same kind, block 0 last.
pub fn extents(used: &[bool], is_zero: impl Fn(u64) -> bool) -> Vec<Extent> {
    let mut out: Vec<Extent> = Vec::new();
    for (i, u) in used.iter().enumerate().skip(1) {
        if !*u {
            continue;
        }
        let (i, zero) = (i as u64, is_zero(i as u64));
        match out.last_mut() {
            Some(e) if e.zero == zero && e.first + e.count == i => e.count += 1,
            _ => out.push(Extent {
                first: i,
                count: 1,
                zero,
            }),
        }
    }
    out.push(Extent {
        first: 0,
        count: 1,
        zero: false,
    });
    out
}

/// Reads one extent's head from a stream.
pub fn read_extent(input: &mut impl Read) -> Result<Extent, String> {
    let mut b = [0u8; 17];
    input
        .read_exact(&mut b)
        .map_err(|e| format!("an extent's head: {e}"))?;
    Ok(Extent {
        first: u64::from_le_bytes(b[..8].try_into().unwrap()),
        count: u64::from_le_bytes(b[8..16].try_into().unwrap()),
        zero: match b[16] {
            0 => false,
            1 => true,
            k => return Err(format!("an extent of unknown kind {k}")),
        },
    })
}

pub fn write_extent(out: &mut impl Write, e: &Extent) -> io::Result<()> {
    out.write_all(&e.first.to_le_bytes())?;
    out.write_all(&e.count.to_le_bytes())?;
    out.write_all(&[e.zero as u8])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image() -> Vec<Vec<u8>> {
        // 40 blocks, one group of 32768: descriptor in block 1, bitmap in block 2.
        let mut blocks = vec![vec![0u8; BLOCK]; 40];
        let sb = &mut blocks[0][SUPER_OFFSET..];
        sb[4..8].copy_from_slice(&40u32.to_le_bytes());
        sb[24..28].copy_from_slice(&2u32.to_le_bytes());
        sb[32..36].copy_from_slice(&32768u32.to_le_bytes());
        sb[56..58].copy_from_slice(&EXT2_MAGIC.to_le_bytes());
        sb[58..60].copy_from_slice(&STATE_VALID.to_le_bytes());
        sb[104..120].copy_from_slice(&[5; 16]);
        sb[120..127].copy_from_slice(b"lucina-");
        blocks[1][0..4].copy_from_slice(&2u32.to_le_bytes());
        // Used: 0-3, and 10-13 (11 and 12 are zero).
        blocks[2][0] = 0b0000_1111;
        blocks[2][1] = 0b0011_1100;
        blocks[3][0] = 1;
        blocks[10][0] = 1;
        blocks[13][0] = 1;
        blocks
    }

    #[test]
    fn the_superblock_says_what_it_should() {
        let i = parse_superblock(&image()[0]).unwrap();
        assert_eq!(
            (i.blocks, i.uuid, i.label.as_str(), i.state),
            (40, [5; 16], "lucina-", 1)
        );
        assert!(parse_superblock(&vec![0u8; BLOCK]).is_none());
        let dirty = with_state(&image()[0], 0);
        assert_eq!(parse_superblock(&dirty).unwrap().state, 0);
    }

    #[test]
    fn only_used_blocks_go_and_block_0_goes_last() {
        let img = image();
        let used = used_blocks(|b| Ok(img[b as usize].clone())).unwrap();
        assert_eq!(used.iter().filter(|u| **u).count(), 8);
        let e = extents(&used, |b| img[b as usize].iter().all(|x| *x == 0));
        assert_eq!(
            e,
            [
                Extent {
                    first: 1,
                    count: 3,
                    zero: false
                },
                Extent {
                    first: 10,
                    count: 1,
                    zero: false
                },
                Extent {
                    first: 11,
                    count: 2,
                    zero: true
                },
                Extent {
                    first: 13,
                    count: 1,
                    zero: false
                },
                Extent {
                    first: 0,
                    count: 1,
                    zero: false
                },
            ]
        );
    }

    #[test]
    fn the_stream_reads_back() {
        let h = Header {
            blocks: 9,
            extents: 3,
            uuid: [1; 16],
            sha256: [2; 32],
        };
        let mut b = Vec::new();
        h.write(&mut b).unwrap();
        assert_eq!(Header::read(&mut b.as_slice()).unwrap(), h);
        assert!(Header::read(&mut [0u8; 80].as_slice()).is_err());
        let e = Extent {
            first: 7,
            count: 2,
            zero: true,
        };
        let mut b = Vec::new();
        write_extent(&mut b, &e).unwrap();
        assert_eq!(read_extent(&mut b.as_slice()).unwrap(), e);
    }
}

#[cfg(test)]
mod needs_check_tests {
    use super::*;

    #[test]
    fn only_a_cleanly_unmounted_state_skips_the_check() {
        assert!(!needs_check(STATE_VALID));
        // Mounted and not marked clean, as /data is from the first mount until `lucina-data clean`.
        assert!(needs_check(0));
    }

    #[test]
    fn the_error_flag_forces_the_check_even_with_the_valid_bit() {
        assert!(needs_check(STATE_ERROR));
        assert!(needs_check(STATE_VALID | STATE_ERROR));
    }

    #[test]
    fn any_other_bit_forces_the_check() {
        for state in 0..=u16::MAX {
            assert_eq!(needs_check(state), state != 1, "{state}");
        }
    }
}
