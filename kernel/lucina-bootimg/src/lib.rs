//! Android boot image, header versions 0 to 2 (the format Xiaomi ABL loads on surya).
//!
//! Layout, per AOSP `mkbootimg` (`references/upstream.lock`, oracle used by `scripts/bootimg-oracle.sh`):
//! one header page, then kernel, ramdisk, second, recovery DTBO (v1+) and DTB (v2), each padded to the page
//! size. The page arithmetic in [`layout`] mirrors `verification/bootimg/layout.rs`, which Verus proves
//! page-aligned, ordered, non-overlapping and overflow-free.
//!
//! The parser never panics on untrusted input and does not allocate.

#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(feature = "std")]
pub mod build;
#[cfg(feature = "std")]
pub mod files;

pub const MAGIC: &[u8; 8] = b"ANDROID!";
pub const NAME_SIZE: usize = 16;
pub const ARGS_SIZE: usize = 512;
pub const EXTRA_ARGS_SIZE: usize = 1024;
pub const ID_SIZE: usize = 32;
pub const HEADER_V0_SIZE: usize = 1632;
pub const HEADER_V1_SIZE: usize = 1648;
pub const HEADER_V2_SIZE: usize = 1660;
/// Page sizes accepted by `mkbootimg` (2^11 to 2^14).
pub const PAGE_SIZES: [u32; 4] = [2048, 4096, 8192, 16384];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Shorter than the header, or a section runs past the end of the image.
    Truncated {
        needed: u64,
        len: u64,
    },
    BadMagic,
    /// Header versions 3 and 4 use a different layout and are not supported.
    UnsupportedVersion(u32),
    BadPageSize(u32),
    /// v1/v2 `header_size` does not match the version.
    BadHeaderSize {
        version: u32,
        header_size: u32,
    },
    /// The stored recovery DTBO offset disagrees with the computed layout.
    BadRecoveryDtboOffset {
        stored: u64,
        expected: u64,
    },
    /// A field that must be zero for this version, or a NUL-terminated string without its NUL.
    Malformed(&'static str),
}

/// Section sizes in bytes, as stored in the header.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Sizes {
    pub kernel: u32,
    pub ramdisk: u32,
    pub second: u32,
    pub recovery_dtbo: u32,
    pub dtb: u32,
}

/// Byte offsets of each section and the total image length (all multiples of the page size).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub kernel: u64,
    pub ramdisk: u64,
    pub second: u64,
    pub recovery_dtbo: u64,
    pub dtb: u64,
    pub end: u64,
}

pub fn valid_page_size(page_size: u32) -> bool {
    PAGE_SIZES.contains(&page_size)
}

// `pages` and `layout_pages` must stay token-for-token identical (ignoring `proof` blocks and comments) to the
// functions of the same name in verification/bootimg/layout.rs, where Verus proves them for every valid page
// size. The `proof_model_matches_code` test enforces this.

/// Pages needed to hold `len` bytes. `p` must be a valid page size.
#[allow(clippy::manual_div_ceil, clippy::let_and_return)]
fn pages(len: u32, p: u64) -> u64 {
    let n = (len as u64 + (p - 1)) / p;
    n
}

/// Section offsets. `p` must be a valid page size; then nothing overflows and `end < 2^38` (proved).
fn layout_pages(p: u64, k: u32, r: u32, s: u32, o: u32, d: u32) -> Layout {
    let kernel_page: u64 = 1;
    let ramdisk_page = kernel_page + pages(k, p);
    let second_page = ramdisk_page + pages(r, p);
    let recovery_dtbo_page = second_page + pages(s, p);
    let dtb_page = recovery_dtbo_page + pages(o, p);
    let end_page = dtb_page + pages(d, p);
    Layout {
        kernel: kernel_page * p,
        ramdisk: ramdisk_page * p,
        second: second_page * p,
        recovery_dtbo: recovery_dtbo_page * p,
        dtb: dtb_page * p,
        end: end_page * p,
    }
}

/// Section offsets for the given sizes. The header takes the first page.
pub fn layout(page_size: u32, sizes: &Sizes) -> Result<Layout, Error> {
    if !valid_page_size(page_size) {
        return Err(Error::BadPageSize(page_size));
    }
    let s = sizes;
    Ok(layout_pages(
        page_size as u64,
        s.kernel,
        s.ramdisk,
        s.second,
        s.recovery_dtbo,
        s.dtb,
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header<'a> {
    pub version: u32,
    pub sizes: Sizes,
    pub kernel_addr: u32,
    pub ramdisk_addr: u32,
    pub second_addr: u32,
    pub tags_addr: u32,
    pub page_size: u32,
    /// Raw `(os_version << 11) | os_patch_level`.
    pub os_version: u32,
    /// Board name without its NUL padding.
    pub name: &'a [u8],
    /// First part of the kernel command line (up to its NUL).
    pub cmdline: &'a [u8],
    pub id: &'a [u8; ID_SIZE],
    /// Continuation of the command line (up to its NUL); the full line is `cmdline` followed by this.
    pub extra_cmdline: &'a [u8],
    /// Only for v1+: absolute offset stored in the header (0 when there is no recovery DTBO).
    pub recovery_dtbo_offset: u64,
    /// Only for v2.
    pub dtb_addr: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootImage<'a> {
    pub header: Header<'a>,
    pub layout: Layout,
    pub kernel: &'a [u8],
    pub ramdisk: &'a [u8],
    pub second: &'a [u8],
    pub recovery_dtbo: &'a [u8],
    pub dtb: &'a [u8],
}

fn u32_at(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn u64_at(b: &[u8], off: usize) -> u64 {
    u32_at(b, off) as u64 | (u32_at(b, off + 4) as u64) << 32
}

/// The bytes before the first NUL, or the whole field if it is completely filled (C `char[N]` fields need
/// no terminator, and ABL reads them that way).
fn asciiz(field: &[u8]) -> &[u8] {
    let n = field.iter().position(|&c| c == 0).unwrap_or(field.len());
    &field[..n]
}

fn truncated(needed: u64, len: usize) -> Error {
    Error::Truncated {
        needed,
        len: len as u64,
    }
}

/// Parses and validates a boot image. Every section slice lies inside `bytes`.
pub fn parse(bytes: &[u8]) -> Result<BootImage<'_>, Error> {
    if bytes.len() < HEADER_V0_SIZE {
        return Err(truncated(HEADER_V0_SIZE as u64, bytes.len()));
    }
    if &bytes[..8] != MAGIC {
        return Err(Error::BadMagic);
    }
    let version = u32_at(bytes, 40);
    let header_len = match version {
        0 => HEADER_V0_SIZE,
        1 => HEADER_V1_SIZE,
        2 => HEADER_V2_SIZE,
        v => return Err(Error::UnsupportedVersion(v)),
    };
    if bytes.len() < header_len {
        return Err(truncated(header_len as u64, bytes.len()));
    }
    let page_size = u32_at(bytes, 36);
    if !valid_page_size(page_size) {
        return Err(Error::BadPageSize(page_size));
    }

    let mut sizes = Sizes {
        kernel: u32_at(bytes, 8),
        ramdisk: u32_at(bytes, 16),
        second: u32_at(bytes, 24),
        ..Sizes::default()
    };
    let mut recovery_dtbo_offset = 0;
    let mut dtb_addr = 0;
    if version >= 1 {
        sizes.recovery_dtbo = u32_at(bytes, 1632);
        recovery_dtbo_offset = u64_at(bytes, 1636);
        let header_size = u32_at(bytes, 1644);
        if header_size as usize != header_len {
            return Err(Error::BadHeaderSize {
                version,
                header_size,
            });
        }
    }
    if version >= 2 {
        sizes.dtb = u32_at(bytes, 1648);
        dtb_addr = u64_at(bytes, 1652);
    }

    let layout = layout(page_size, &sizes)?;
    if sizes.recovery_dtbo > 0 && recovery_dtbo_offset != layout.recovery_dtbo {
        return Err(Error::BadRecoveryDtboOffset {
            stored: recovery_dtbo_offset,
            expected: layout.recovery_dtbo,
        });
    }
    if sizes.recovery_dtbo == 0 && recovery_dtbo_offset != 0 {
        return Err(Error::Malformed(
            "recovery_dtbo_offset set without a recovery DTBO",
        ));
    }
    // Sections only need their bytes present: trailing padding may be cut, and an empty section needs nothing.
    let section = |off: u64, size: u32| -> Result<&[u8], Error> {
        if size == 0 {
            return Ok(&[]);
        }
        let end = off + size as u64;
        if end > bytes.len() as u64 {
            return Err(truncated(end, bytes.len()));
        }
        Ok(&bytes[off as usize..end as usize])
    };

    let header = Header {
        version,
        sizes,
        kernel_addr: u32_at(bytes, 12),
        ramdisk_addr: u32_at(bytes, 20),
        second_addr: u32_at(bytes, 28),
        tags_addr: u32_at(bytes, 32),
        page_size,
        os_version: u32_at(bytes, 44),
        name: asciiz(&bytes[48..64]),
        cmdline: asciiz(&bytes[64..576]),
        id: bytes[576..608]
            .try_into()
            .map_err(|_| Error::Malformed("id"))?,
        extra_cmdline: asciiz(&bytes[608..1632]),
        recovery_dtbo_offset,
        dtb_addr,
    };
    Ok(BootImage {
        header,
        layout,
        kernel: section(layout.kernel, sizes.kernel)?,
        ramdisk: section(layout.ramdisk, sizes.ramdisk)?,
        second: section(layout.second, sizes.second)?,
        recovery_dtbo: section(layout.recovery_dtbo, sizes.recovery_dtbo)?,
        dtb: section(layout.dtb, sizes.dtb)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Body of `fn name(` in `src`, with comments, `proof { .. }` blocks and whitespace removed.
    fn body(src: &str, name: &str) -> String {
        let start = src
            .find(&format!("fn {name}("))
            .unwrap_or_else(|| panic!("fn {name} not found"));
        let open = start + src[start..].find('{').unwrap();
        let text: String = src[open + 1..matching(src, open)]
            .lines()
            .map(|l| l.split("//").next().unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        let mut text = text;
        while let Some(i) = text.find("proof {") {
            let end = matching(&text, i + "proof ".len());
            text.replace_range(i..=end, "");
        }
        text.chars().filter(|c| !c.is_whitespace()).collect()
    }

    /// Index of the `}` matching the `{` at `open`.
    fn matching(s: &str, open: usize) -> usize {
        let mut depth = 0;
        for (i, c) in s[open..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return open + i;
                    }
                }
                _ => {}
            }
        }
        panic!("unbalanced braces");
    }

    #[test]
    fn proof_model_matches_code() {
        const MODEL: &str = include_str!("../../../verification/bootimg/layout.rs");
        const CODE: &str = include_str!("lib.rs");
        for name in ["pages", "layout_pages"] {
            let (model, code) = (body(MODEL, name), body(CODE, name));
            assert!(!code.is_empty());
            assert_eq!(model, code, "{name} in lib.rs differs from the Verus model");
        }
    }

    #[test]
    fn fields_without_nul_use_the_whole_field() {
        let img = crate::build::build(&crate::build::BuildSpec {
            kernel: &[1],
            ..crate::build::BuildSpec::surya_v0()
        })
        .unwrap();
        let mut full = img.clone();
        full[48..64].copy_from_slice(b"0123456789abcdef");
        full[64..576].fill(b'a');
        let p = parse(&full).unwrap();
        assert_eq!(p.header.name, b"0123456789abcdef");
        assert_eq!(p.header.cmdline.len(), 512);
    }

    #[test]
    fn image_cut_after_last_data_parses() {
        // Kernel of 1 byte: the image may end right after it; empty sections past the end are fine.
        let img = crate::build::build(&crate::build::BuildSpec {
            kernel: &[1],
            ..crate::build::BuildSpec::surya_v0()
        })
        .unwrap();
        let p = parse(&img[..4096 + 1]).unwrap();
        assert_eq!((p.kernel, p.ramdisk.len(), p.dtb.len()), (&[1u8][..], 0, 0));
        assert!(matches!(parse(&img[..4096]), Err(Error::Truncated { .. })));
    }

    #[test]
    fn layout_is_page_aligned_and_ordered() {
        let sizes = Sizes {
            kernel: 5000,
            ramdisk: 1,
            second: 0,
            recovery_dtbo: 4096,
            dtb: 4097,
        };
        let l = layout(4096, &sizes).unwrap();
        assert_eq!(
            l,
            Layout {
                kernel: 4096,
                ramdisk: 12288,
                second: 16384,
                recovery_dtbo: 16384,
                dtb: 20480,
                end: 28672
            }
        );
    }

    #[test]
    fn layout_max_sizes_do_not_overflow() {
        let m = u32::MAX;
        let l = layout(
            16384,
            &Sizes {
                kernel: m,
                ramdisk: m,
                second: m,
                recovery_dtbo: m,
                dtb: m,
            },
        )
        .unwrap();
        assert_eq!(l.end, 16384 * (1 + 5 * (m as u64).div_ceil(16384)));
    }

    #[test]
    fn rejects_bad_page_size() {
        assert_eq!(
            layout(4095, &Sizes::default()),
            Err(Error::BadPageSize(4095))
        );
        assert_eq!(layout(0, &Sizes::default()), Err(Error::BadPageSize(0)));
    }

    #[test]
    fn rejects_short_and_bad_magic() {
        assert!(matches!(parse(&[0; 100]), Err(Error::Truncated { .. })));
        assert_eq!(parse(&[0; 2048]), Err(Error::BadMagic));
    }
}
