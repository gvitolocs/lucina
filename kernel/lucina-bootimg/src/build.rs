//! Boot image builder, byte-compatible with AOSP `mkbootimg` for header versions 0 to 2.

use crate::{
    ARGS_SIZE, EXTRA_ARGS_SIZE, HEADER_V1_SIZE, HEADER_V2_SIZE, MAGIC, NAME_SIZE, Sizes, layout,
    valid_page_size,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildError {
    UnsupportedVersion(u32),
    BadPageSize(u32),
    /// A section is larger than the 32-bit size field.
    TooLarge(&'static str),
    /// The board name needs its NUL and must fit in 16 bytes.
    NameTooLong,
    /// The command line plus its NUL must fit in 512 + 1024 - 1 bytes.
    CmdlineTooLong,
    /// The command line contains a NUL byte.
    CmdlineNul,
    /// A recovery DTBO needs v1+, a DTB needs v2.
    SectionNotInVersion(&'static str),
    /// Header v2 requires a DTB (as `mkbootimg` does).
    MissingDtb,
    /// `base + offset` does not fit the address field.
    AddressOverflow(&'static str),
}

/// Inputs, named like `mkbootimg`'s arguments. Defaults match `mkbootimg`'s defaults.
#[derive(Debug, Clone)]
pub struct BuildSpec<'a> {
    pub header_version: u32,
    pub page_size: u32,
    pub base: u32,
    pub kernel_offset: u32,
    pub ramdisk_offset: u32,
    pub second_offset: u32,
    pub tags_offset: u32,
    pub dtb_offset: u64,
    /// Raw `(os_version << 11) | os_patch_level`.
    pub os_version: u32,
    pub name: &'a [u8],
    pub cmdline: &'a [u8],
    pub kernel: &'a [u8],
    pub ramdisk: &'a [u8],
    pub second: &'a [u8],
    pub recovery_dtbo: &'a [u8],
    pub dtb: &'a [u8],
}

impl Default for BuildSpec<'_> {
    fn default() -> Self {
        Self {
            header_version: 0,
            page_size: 2048,
            base: 0x1000_0000,
            kernel_offset: 0x0000_8000,
            ramdisk_offset: 0x0100_0000,
            second_offset: 0x00f0_0000,
            tags_offset: 0x0000_0100,
            dtb_offset: 0x01f0_0000,
            os_version: 0,
            name: b"",
            cmdline: b"",
            kernel: b"",
            ramdisk: b"",
            second: b"",
            recovery_dtbo: b"",
            dtb: b"",
        }
    }
}

impl BuildSpec<'_> {
    /// The surya mainline/U-Boot convention: header v0, page 4096, base 0 (`docs/research/boot-chain.md`).
    pub fn surya_v0() -> Self {
        Self {
            page_size: 4096,
            base: 0,
            ..Self::default()
        }
    }
}

fn size32(data: &[u8], what: &'static str) -> Result<u32, BuildError> {
    u32::try_from(data.len()).map_err(|_| BuildError::TooLarge(what))
}

fn addr(base: u32, offset: u32, what: &'static str) -> Result<u32, BuildError> {
    base.checked_add(offset)
        .ok_or(BuildError::AddressOverflow(what))
}

fn put_padded(out: &mut Vec<u8>, data: &[u8], width: usize) {
    out.extend_from_slice(data);
    out.resize(out.len() + (width - data.len()), 0);
}

fn pad_to(out: &mut Vec<u8>, page_size: usize) {
    out.resize(out.len().next_multiple_of(page_size), 0);
}

/// `mkbootimg`'s image id: SHA-1 over each section followed by its little-endian u32 size.
fn image_id(spec: &BuildSpec<'_>) -> [u8; 20] {
    let mut sha = sha1_smol::Sha1::new();
    let mut add = |data: &[u8]| {
        sha.update(data);
        sha.update(&(data.len() as u32).to_le_bytes());
    };
    add(spec.kernel);
    add(spec.ramdisk);
    add(spec.second);
    if spec.header_version > 0 {
        add(spec.recovery_dtbo);
    }
    if spec.header_version > 1 {
        add(spec.dtb);
    }
    sha.digest().bytes()
}

pub fn build(spec: &BuildSpec<'_>) -> Result<Vec<u8>, BuildError> {
    let v = spec.header_version;
    if v > 2 {
        return Err(BuildError::UnsupportedVersion(v));
    }
    if !valid_page_size(spec.page_size) {
        return Err(BuildError::BadPageSize(spec.page_size));
    }
    if v < 1 && !spec.recovery_dtbo.is_empty() {
        return Err(BuildError::SectionNotInVersion("recovery_dtbo"));
    }
    if v < 2 && !spec.dtb.is_empty() {
        return Err(BuildError::SectionNotInVersion("dtb"));
    }
    if v == 2 && spec.dtb.is_empty() {
        return Err(BuildError::MissingDtb);
    }
    if spec.name.len() >= NAME_SIZE {
        return Err(BuildError::NameTooLong);
    }
    if spec.cmdline.contains(&0) {
        return Err(BuildError::CmdlineNul);
    }
    if spec.cmdline.len() + 1 > ARGS_SIZE + EXTRA_ARGS_SIZE - 1 {
        return Err(BuildError::CmdlineTooLong);
    }

    let sizes = Sizes {
        kernel: size32(spec.kernel, "kernel")?,
        ramdisk: size32(spec.ramdisk, "ramdisk")?,
        second: size32(spec.second, "second")?,
        recovery_dtbo: size32(spec.recovery_dtbo, "recovery_dtbo")?,
        dtb: size32(spec.dtb, "dtb")?,
    };
    let layout =
        layout(spec.page_size, &sizes).map_err(|_| BuildError::BadPageSize(spec.page_size))?;
    let ramdisk_addr = if sizes.ramdisk > 0 {
        addr(spec.base, spec.ramdisk_offset, "ramdisk")?
    } else {
        0
    };
    let second_addr = if sizes.second > 0 {
        addr(spec.base, spec.second_offset, "second")?
    } else {
        0
    };
    let dtb_addr = (spec.base as u64)
        .checked_add(spec.dtb_offset)
        .ok_or(BuildError::AddressOverflow("dtb"))?;

    let mut out = Vec::with_capacity(layout.end as usize);
    out.extend_from_slice(MAGIC);
    for word in [
        sizes.kernel,
        addr(spec.base, spec.kernel_offset, "kernel")?,
        sizes.ramdisk,
        ramdisk_addr,
        sizes.second,
        second_addr,
        addr(spec.base, spec.tags_offset, "tags")?,
        spec.page_size,
        v,
        spec.os_version,
    ] {
        out.extend_from_slice(&word.to_le_bytes());
    }
    put_padded(&mut out, spec.name, NAME_SIZE);
    // mkbootimg: the first 511 bytes plus a NUL, the rest (with its NUL) in extra_cmdline.
    let split = spec.cmdline.len().min(ARGS_SIZE - 1);
    put_padded(&mut out, &spec.cmdline[..split], ARGS_SIZE);
    put_padded(&mut out, &image_id(spec), 32);
    put_padded(&mut out, &spec.cmdline[split..], EXTRA_ARGS_SIZE);
    if v >= 1 {
        let dtbo_offset = if sizes.recovery_dtbo > 0 {
            layout.recovery_dtbo
        } else {
            0
        };
        out.extend_from_slice(&sizes.recovery_dtbo.to_le_bytes());
        out.extend_from_slice(&dtbo_offset.to_le_bytes());
        let header_size = if v == 1 {
            HEADER_V1_SIZE
        } else {
            HEADER_V2_SIZE
        } as u32;
        out.extend_from_slice(&header_size.to_le_bytes());
    }
    if v >= 2 {
        out.extend_from_slice(&sizes.dtb.to_le_bytes());
        out.extend_from_slice(&dtb_addr.to_le_bytes());
    }

    let page = spec.page_size as usize;
    pad_to(&mut out, page);
    for section in [
        spec.kernel,
        spec.ramdisk,
        spec.second,
        spec.recovery_dtbo,
        spec.dtb,
    ] {
        out.extend_from_slice(section);
        pad_to(&mut out, page);
    }
    debug_assert_eq!(out.len() as u64, layout.end);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Error, parse};

    fn sample(v: u32) -> (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>) {
        let kernel: Vec<u8> = (0..5000u32).map(|i| i as u8).collect();
        let ramdisk = vec![7u8; 3];
        let dtbo = if v >= 1 { vec![1u8; 100] } else { vec![] };
        let dtb = if v >= 2 { vec![2u8; 4097] } else { vec![] };
        (kernel, ramdisk, dtbo, dtb)
    }

    #[test]
    fn round_trip_all_versions() {
        let long_cmdline = vec![b'x'; 1000];
        for v in 0..=2 {
            let (kernel, ramdisk, dtbo, dtb) = sample(v);
            let spec = BuildSpec {
                header_version: v,
                name: b"surya",
                cmdline: &long_cmdline,
                kernel: &kernel,
                ramdisk: &ramdisk,
                recovery_dtbo: &dtbo,
                dtb: &dtb,
                ..BuildSpec::surya_v0()
            };
            let img = build(&spec).unwrap();
            let p = parse(&img).unwrap();
            assert_eq!(p.header.version, v);
            assert_eq!(
                (p.kernel, p.ramdisk, p.recovery_dtbo, p.dtb),
                (&kernel[..], &ramdisk[..], &dtbo[..], &dtb[..])
            );
            assert_eq!(p.header.name, b"surya");
            assert_eq!(
                [p.header.cmdline, p.header.extra_cmdline].concat(),
                long_cmdline
            );
            assert_eq!(p.header.cmdline.len(), 511);
            assert_eq!(p.header.kernel_addr, 0x8000);
            assert_eq!(p.header.ramdisk_addr, 0x0100_0000);
            assert_eq!(
                p.header.second_addr, 0,
                "absent second gets address 0, like mkbootimg"
            );
            assert_eq!(img.len() as u64, p.layout.end);
        }
    }

    #[test]
    fn rejects_inconsistent_inputs() {
        let one = [1u8];
        assert_eq!(
            build(&BuildSpec {
                dtb: &one,
                ..BuildSpec::surya_v0()
            }),
            Err(BuildError::SectionNotInVersion("dtb"))
        );
        assert_eq!(
            build(&BuildSpec {
                page_size: 1000,
                ..BuildSpec::default()
            }),
            Err(BuildError::BadPageSize(1000))
        );
        assert_eq!(
            build(&BuildSpec {
                cmdline: b"a\0b",
                ..BuildSpec::default()
            }),
            Err(BuildError::CmdlineNul)
        );
        assert_eq!(
            build(&BuildSpec {
                cmdline: &[b'a'; 1535],
                ..BuildSpec::default()
            }),
            Err(BuildError::CmdlineTooLong)
        );
        assert!(
            build(&BuildSpec {
                cmdline: &[b'a'; 1534],
                ..BuildSpec::default()
            })
            .is_ok()
        );
        assert_eq!(
            build(&BuildSpec {
                name: b"0123456789abcdef",
                ..BuildSpec::default()
            }),
            Err(BuildError::NameTooLong)
        );
        assert_eq!(
            build(&BuildSpec {
                base: u32::MAX,
                ..BuildSpec::default()
            }),
            Err(BuildError::AddressOverflow("kernel"))
        );
    }

    #[test]
    fn parser_rejects_corruption() {
        let (kernel, ramdisk, dtbo, dtb) = sample(2);
        let spec = BuildSpec {
            header_version: 2,
            kernel: &kernel,
            ramdisk: &ramdisk,
            recovery_dtbo: &dtbo,
            dtb: &dtb,
            ..BuildSpec::surya_v0()
        };
        let img = build(&spec).unwrap();

        // Every truncation either fails cleanly or still yields in-bounds sections (never panics).
        for len in 0..img.len() {
            if let Ok(p) = parse(&img[..len]) {
                assert!(p.layout.dtb + p.dtb.len() as u64 <= len as u64);
            }
        }
        let mut bad = img.clone();
        bad[40] = 3;
        assert_eq!(parse(&bad), Err(Error::UnsupportedVersion(3)));
        let mut bad = img.clone();
        bad[1644] = 0; // header_size
        assert!(matches!(parse(&bad), Err(Error::BadHeaderSize { .. })));
        let mut bad = img.clone();
        bad[1636] ^= 1; // recovery_dtbo_offset
        assert!(matches!(
            parse(&bad),
            Err(Error::BadRecoveryDtboOffset { .. })
        ));
        // In v2 a bigger kernel_size first trips the recovery DTBO offset check; v0 has no such field.
        let mut bad = img.clone();
        bad[8..12].copy_from_slice(&u32::MAX.to_le_bytes()); // kernel_size
        assert!(matches!(
            parse(&bad),
            Err(Error::BadRecoveryDtboOffset { .. })
        ));
        let mut v0 = build(&BuildSpec {
            kernel: &kernel,
            ..BuildSpec::surya_v0()
        })
        .unwrap();
        v0[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(parse(&v0), Err(Error::Truncated { .. })));
        let mut bad = img;
        bad[1640] = 1; // recovery_dtbo_offset high bits while a DTBO is present
        assert!(matches!(
            parse(&bad),
            Err(Error::BadRecoveryDtboOffset { .. })
        ));
    }
}
