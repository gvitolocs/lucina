// Verus proof: the device tree header check LucinaBoot and the host tools run before reading any block.
// `check_header` is the same code as in kernel/lucina-fdt/src/reader.rs (its `proof_model_matches_code` test
// compares the bodies token by token).
//
// Proved for every buffer length and every value of the ten header words:
//   - it accepts exactly the headers described by `accepted` (devicetree specification v0.4, section 5.2, plus
//     "totalsize fits the buffer");
//   - on success the structure and strings blocks lie inside `totalsize`, which lies inside the buffer, the
//     reservation block starts inside it, and the alignments hold, so the reader's slices cannot go out of bounds;
//   - no arithmetic over- or underflow.
// Trusted: Verus, Z3, vstd, and the textual comparison test (rustc and Verus compile the same tokens to the same
// semantics). Not covered: the walk of the reservation block and the overlap check after it, which index the
// buffer through checked `get` calls.

use vstd::prelude::*;

verus! {

global size_of usize == 8;

pub const MAGIC: u32 = 0xd00d_feed;
pub const HEADER_SIZE: usize = 40;

pub enum Error {
    Truncated,
    BadMagic,
    BadTotalSize,
    UnsupportedVersion(u32),
    IncompatibleVersion(u32),
    Misaligned,
    OutOfBounds,
}

pub struct Header {
    pub total: usize,
    pub off_struct: usize,
    pub size_struct: usize,
    pub off_strings: usize,
    pub size_strings: usize,
    pub off_rsv: usize,
}

/// The headers `check_header` must accept: words are magic, totalsize, off_dt_struct, off_dt_strings,
/// off_mem_rsvmap, version, last_comp_version, boot_cpuid_phys, size_dt_strings, size_dt_struct.
pub open spec fn accepted(buf_len: usize, w: [u32; 10]) -> bool {
    &&& buf_len >= 8
    &&& w@[0] == MAGIC
    &&& 40 <= w@[1] && w@[1] <= buf_len
    &&& w@[5] >= 17
    &&& w@[6] <= 16
    &&& w@[2] % 4 == 0 && w@[9] % 4 == 0 && w@[4] % 8 == 0
    &&& w@[2] as int + w@[9] as int <= w@[1] as int
    &&& w@[3] as int + w@[8] as int <= w@[1] as int
    &&& w@[4] < w@[1]
}

pub fn check_header(buf_len: usize, w: [u32; 10]) -> (r: Result<Header, Error>)
    ensures
        r is Ok <==> accepted(buf_len, w),
        r matches Ok(h) ==> {
            &&& h.total == w@[1] && h.off_struct == w@[2] && h.off_strings == w@[3]
            &&& h.off_rsv == w@[4] && h.size_strings == w@[8] && h.size_struct == w@[9]
            &&& HEADER_SIZE <= h.total && h.total <= buf_len
            &&& h.off_struct + h.size_struct <= h.total
            &&& h.off_strings + h.size_strings <= h.total
            &&& h.off_rsv < h.total
            &&& h.off_struct % 4 == 0 && h.size_struct % 4 == 0 && h.off_rsv % 8 == 0
        },
{
    if buf_len < 8 {
        return Err(Error::Truncated);
    }
    if w[0] != MAGIC {
        return Err(Error::BadMagic);
    }
    let total = w[1] as usize;
    if total < HEADER_SIZE || total > buf_len {
        return Err(Error::BadTotalSize);
    }
    if w[5] < 17 {
        return Err(Error::UnsupportedVersion(w[5]));
    }
    if w[6] > 16 {
        return Err(Error::IncompatibleVersion(w[6]));
    }
    let off_struct = w[2] as usize;
    let off_strings = w[3] as usize;
    let off_rsv = w[4] as usize;
    let size_strings = w[8] as usize;
    let size_struct = w[9] as usize;
    if off_struct % 4 != 0 || size_struct % 4 != 0 || off_rsv % 8 != 0 {
        return Err(Error::Misaligned);
    }
    if off_struct > total || size_struct > total - off_struct {
        return Err(Error::OutOfBounds);
    }
    if off_strings > total || size_strings > total - off_strings {
        return Err(Error::OutOfBounds);
    }
    if off_rsv >= total {
        return Err(Error::OutOfBounds);
    }
    Ok(Header {
        total,
        off_struct,
        size_struct,
        off_strings,
        size_strings,
        off_rsv,
    })
}

fn main() {}

} // verus!
