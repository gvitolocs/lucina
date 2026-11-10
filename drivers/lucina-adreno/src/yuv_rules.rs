// Where the planes of a YUV picture may lie in a program's buffer, for the `yuv` command: everything the GPU may read of a plane is
// inside the buffer, and every address and line is 64-byte aligned, as the texture unit needs. Verus checks this very file: `//@`
// lines hold the proof, see scripts/verify-shipped.sh. Not covered: that `len` is the buffer's real size, which the kernel knows.

/// Larger than any picture this GPU samples (`TEX_CONST_1` has 15 bits).
pub const MAX_SIDE: u32 = 0x4000;
/// Larger than any buffer (`BUFFER_BYTES_LIMIT` is 512 MiB).
pub const MAX_LEN: u64 = 0x1_0000_0000;
//@ global size_of usize == 8;

/// How the chroma is laid out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)] //@omit
pub enum Planar {
    /// NV12: one plane of Cb and Cr, interleaved.
    Two,
    /// I420: a plane of Cb, then one of Cr.
    Three,
}

/// A picture in a buffer: its size in luma samples, and each plane's first byte and line, in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)] //@omit
pub struct Layout {
    pub planar: Planar,
    pub width: u32,
    pub height: u32,
    pub y_offset: u64,
    pub y_pitch: u32,
    pub c_offset: u64,
    pub c_pitch: u32,
    /// The Cr plane of [`Planar::Three`]; ignored for two planes.
    pub v_offset: u64,
}

//@ /// A plane of `rows` lines of `pitch` bytes from `offset`, at least `row` bytes used in each, inside `len` and aligned.
//@ pub open spec fn plane_ok(offset: int, pitch: int, rows: int, row: int, len: int) -> bool {
//@     offset % 64 == 0 && pitch % 64 == 0 && row <= pitch && 0 < rows && offset + rows * pitch <= len
//@ }

/// One plane: `rows` lines of `pitch` bytes from `offset`, each holding `row` bytes, all of it inside `len`.
pub fn plane_fits(offset: u64, pitch: u32, rows: u32, row: u32, len: u64) -> bool //@ret
//@ ensures ret ==> plane_ok(offset as int, pitch as int, rows as int, row as int, len as int),
{
    if len > MAX_LEN || offset > MAX_LEN || rows == 0 || rows > MAX_SIDE || pitch == 0 || pitch > 4 * MAX_SIDE {
        return false;
    }
    if !offset.is_multiple_of(64) || !pitch.is_multiple_of(64) || row > pitch {
        return false;
    }
    //@ assert((rows as u64) * (pitch as u64) <= 0x4000 * 0x10000) by (nonlinear_arith) requires rows <= 0x4000, pitch <= 0x10000;
    //@ assert(offset + (rows as u64) * (pitch as u64) <= 0x1_0000_0000 + 0x4000 * 0x10000);
    let end = offset + (rows as u64) * (pitch as u64);
    end <= len
}

/// Every plane of `l` inside a buffer of `len` bytes.
pub fn fits(l: &Layout, len: u64) -> bool //@ret
//@ ensures ret ==> 0 < l.width < MAX_SIDE && 0 < l.height < MAX_SIDE
//@     && plane_ok(l.y_offset as int, l.y_pitch as int, l.height as int, l.width as int, len as int)
//@     && plane_ok(l.c_offset as int, l.c_pitch as int, (l.height as int + 1) / 2,
//@         if l.planar == Planar::Two { 2 * ((l.width as int + 1) / 2) } else { (l.width as int + 1) / 2 }, len as int)
//@     && (l.planar == Planar::Three ==> plane_ok(l.v_offset as int, l.c_pitch as int, (l.height as int + 1) / 2,
//@         (l.width as int + 1) / 2, len as int)),
{
    if l.width == 0 || l.height == 0 || l.width >= MAX_SIDE || l.height >= MAX_SIDE {
        return false;
    }
    let chroma_rows = l.height / 2 + l.height % 2;
    let chroma_width = l.width / 2 + l.width % 2;
    let chroma_row = match l.planar {
        Planar::Two => 2 * chroma_width,
        Planar::Three => chroma_width,
    };
    if !plane_fits(l.y_offset, l.y_pitch, l.height, l.width, len) {
        return false;
    }
    if !plane_fits(l.c_offset, l.c_pitch, chroma_rows, chroma_row, len) {
        return false;
    }
    match l.planar {
        Planar::Two => true,
        Planar::Three => plane_fits(l.v_offset, l.c_pitch, chroma_rows, chroma_width, len),
    }
}
