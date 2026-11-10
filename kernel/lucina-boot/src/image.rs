//! The arm64 `Image` header (Linux `Documentation/arch/arm64/booting.rst`, "Call the kernel image").
//!
//! ```text
//! u32 code0          executable code (a branch past the header)
//! u32 code1
//! u64 text_offset    image load offset from a 2 MiB-aligned base, little endian
//! u64 image_size     effective image size including BSS, little endian (0: a pre-3.17 kernel)
//! u64 flags          bit 0 big endian, bits 1-2 page size, bit 3 "anywhere in physical memory"
//! u64 res2, res3, res4
//! u32 magic          0x644d5241 ("ARM\x64")
//! u32 res5
//! ```

/// Header size in bytes.
pub const HEADER_SIZE: usize = 64;
/// `magic` at offset 56.
pub const MAGIC: u32 = 0x644d_5241;
/// The 2 MiB alignment of the base `text_offset` is relative to.
pub const BASE_ALIGN: u64 = 2 << 20;
/// `text_offset` that pre-3.17 images (`image_size == 0`) imply.
pub const LEGACY_TEXT_OFFSET: u64 = 0x8_0000;

/// Bit 3 of `flags`: the image may be placed anywhere, not only close to the start of DRAM.
pub const FLAG_PHYS_ANYWHERE: u64 = 1 << 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Shorter than the 64-byte header.
    Truncated,
    /// No `ARM\x64` magic at offset 56.
    BadMagic,
    /// A big-endian image (flags bit 0); LucinaBoot only boots little-endian ones.
    BigEndian,
    /// Page size field is not 4 KiB or "unspecified": Asterinas and LucinaBoot use 4 KiB pages.
    BadPageSize,
    /// `text_offset` is not a multiple of 4 KiB, or `image_size` is smaller than the file.
    BadLayout,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Error::Truncated => "image shorter than the 64-byte arm64 header",
            Error::BadMagic => "no arm64 Image magic",
            Error::BigEndian => "big-endian image",
            Error::BadPageSize => "image wants a page size other than 4 KiB",
            Error::BadLayout => "text_offset not 4 KiB-aligned or image_size below the file size",
        })
    }
}

/// A parsed header plus the size of the file it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Image {
    pub text_offset: u64,
    /// Bytes the image occupies once loaded (BSS included): at least `file_size`.
    pub image_size: u64,
    pub flags: u64,
    pub file_size: u64,
}

fn le64(b: &[u8], off: usize) -> u64 {
    let mut v = [0u8; 8];
    v.copy_from_slice(&b[off..off + 8]);
    u64::from_le_bytes(v)
}

impl Image {
    pub fn parse(bytes: &[u8]) -> Result<Image, Error> {
        if bytes.len() < HEADER_SIZE {
            return Err(Error::Truncated);
        }
        let mut m = [0u8; 4];
        m.copy_from_slice(&bytes[56..60]);
        if u32::from_le_bytes(m) != MAGIC {
            return Err(Error::BadMagic);
        }
        let file_size = bytes.len() as u64;
        let (text_offset, image_size, flags) = match le64(bytes, 16) {
            // Before Linux 3.17 the fields were unset and the offset was always 0x80000.
            0 => (LEGACY_TEXT_OFFSET, file_size, 0),
            size => (le64(bytes, 8), size, le64(bytes, 24)),
        };
        if flags & 1 != 0 {
            return Err(Error::BigEndian);
        }
        if !matches!((flags >> 1) & 3, 0 | 1) {
            return Err(Error::BadPageSize);
        }
        if !text_offset.is_multiple_of(4096) || image_size < file_size {
            return Err(Error::BadLayout);
        }
        Ok(Image {
            text_offset,
            image_size,
            flags,
            file_size,
        })
    }

    /// The only legal address for this image in RAM starting at `dram_base` (the lowest 2 MiB-aligned base at or
    /// above it, plus `text_offset`), or `None` on overflow.
    pub fn load_address(&self, dram_base: u64) -> Option<u64> {
        let base = dram_base.checked_add(BASE_ALIGN - 1)? & !(BASE_ALIGN - 1);
        base.checked_add(self.text_offset)
    }
}

/// Writes `text_offset` and `image_size` into an image's header (used by the host tools on LucinaBoot itself,
/// whose linker cannot compute them without a relocation).
pub fn set_header(bytes: &mut [u8], text_offset: u64, image_size: u64) -> Result<(), Error> {
    if bytes.len() < HEADER_SIZE {
        return Err(Error::Truncated);
    }
    bytes[8..16].copy_from_slice(&text_offset.to_le_bytes());
    bytes[16..24].copy_from_slice(&image_size.to_le_bytes());
    Image::parse(bytes).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(text_offset: u64, image_size: u64, flags: u64) -> [u8; 128] {
        let mut b = [0u8; 128];
        b[8..16].copy_from_slice(&text_offset.to_le_bytes());
        b[16..24].copy_from_slice(&image_size.to_le_bytes());
        b[24..32].copy_from_slice(&flags.to_le_bytes());
        b[56..60].copy_from_slice(&MAGIC.to_le_bytes());
        b
    }

    #[test]
    fn parses_asterinas_style_header() {
        // Asterinas: text_offset 0x200000, flags 0b0010 (LE, 4 KiB, near the DRAM base).
        let b = header(0x20_0000, 0xa0_0000, 0b0010);
        let img = Image::parse(&b).unwrap();
        assert_eq!(img.text_offset, 0x20_0000);
        assert_eq!(img.image_size, 0xa0_0000);
        assert_eq!(img.load_address(0x4000_0000), Some(0x4020_0000));
        assert_eq!(img.load_address(0x8000_0000), Some(0x8020_0000));
        // A DRAM base that is not 2 MiB-aligned rounds up.
        assert_eq!(img.load_address(0x8000_1000), Some(0x8040_0000));
        assert_eq!(img.load_address(u64::MAX - 10), None);
    }

    #[test]
    fn legacy_and_rejections() {
        let legacy = header(0x1234, 0, 0xff);
        let img = Image::parse(&legacy).unwrap();
        assert_eq!(
            (img.text_offset, img.image_size, img.flags),
            (0x8_0000, 128, 0)
        );

        assert_eq!(Image::parse(&[0u8; 63]), Err(Error::Truncated));
        let mut bad = header(0, 4096, 0);
        bad[56] ^= 1;
        assert_eq!(Image::parse(&bad), Err(Error::BadMagic));
        assert_eq!(Image::parse(&header(0, 4096, 1)), Err(Error::BigEndian));
        assert_eq!(
            Image::parse(&header(0, 4096, 0b0100)),
            Err(Error::BadPageSize)
        );
        assert_eq!(Image::parse(&header(0x800, 4096, 0)), Err(Error::BadLayout));
        assert_eq!(Image::parse(&header(0, 64, 0)), Err(Error::BadLayout));
    }

    #[test]
    fn set_header_round_trips() {
        let mut b = header(0, 0, 0);
        b[24..32].copy_from_slice(&0b1010u64.to_le_bytes());
        set_header(&mut b, 0x8_0000, 0x3_0000).unwrap();
        let img = Image::parse(&b).unwrap();
        assert_eq!((img.text_offset, img.image_size), (0x8_0000, 0x3_0000));
        assert_eq!(img.flags & FLAG_PHYS_ANYWHERE, FLAG_PHYS_ANYWHERE);
    }
}
