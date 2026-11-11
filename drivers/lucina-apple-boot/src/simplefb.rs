// SPDX-License-Identifier: GPL-2.0-only
// Derived from the devicetree binding Documentation/devicetree/bindings/display/simple-framebuffer.yaml (SPDX GPL-2.0, i.e.
// version 2 only; maintainer Hans de Goede) of AsahiLinux/linux (branch asahi, rev 6ba0637d1ed9), and from the behaviour of
// m1n1 src/kboot.c (rev cf37927d6bf4; m1n1 is GPL-2.0-or-later from v2.0.0, MIT before; no code copied). Re-expressed in Rust
// for LucinaOS; see the crate README.
// This is a Rust port of the behaviour of the simple-framebuffer binding and m1n1 kboot.c; the original is the source of truth.

//! The `simple-framebuffer` node under `/chosen` (devicetree binding `display/simple-framebuffer.yaml`) as m1n1 fills it in.
//!
//! m1n1 (`src/kboot.c:180-254`, `dt_set_fb`, rev cf37927d6bf4) finds the node `/chosen/framebuffer` of the device tree it
//! ships (Asahi `t8132-jxxx.dtsi:24-30`: `framebuffer0: framebuffer@0`, compatible `"apple,simple-framebuffer",
//! "simple-framebuffer"`, `reg = <0 0 0 0>`, `status = "disabled"`), then sets:
//!
//! - `reg` = (`video.base`, `video.stride * height`), where `height` is the display height without the notch and the base
//!   is unchanged (`kboot.c:194-197`, `get_notchless_fb` at :69-95: it only shortens the height), and renames the node
//!   `framebuffer@<base>` (:200, :205);
//! - `width` = `video.width`, `height` = the notchless height, `stride` = `video.stride` in bytes (:208-215);
//! - `format` from `video.depth & 0xff`: 32 is `x8r8g8b8`, 30 is `x2r10g10b10`, 16 is `r5g6b5` (:217-235). Any other depth
//!   returns without setting the format and without deleting `status` (:229-231);
//! - and deletes `status`, last of all (:237), so a node that is still `"disabled"` was never set up.
//!
//! When there is no framebuffer (`video.base` zero, :182-185) nothing is touched either, and the node stays `"disabled"` with
//! `reg = <0 0 0 0>`.

/// The compatible that the Apple device trees list first (`t8132-jxxx.dtsi:25`).
pub const COMPATIBLE_APPLE: &str = "apple,simple-framebuffer";
/// The generic compatible every `simple-framebuffer` node lists.
pub const COMPATIBLE_GENERIC: &str = "simple-framebuffer";

/// Which of the two compatibles a node lists. [`Rank::Apple`] is tried first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rank {
    /// Lists `apple,simple-framebuffer`.
    Apple,
    /// Lists `simple-framebuffer` but not the Apple one.
    Generic,
}

/// The rank of a node with these `compatible` strings, or `None` when it is no simple framebuffer.
pub fn rank<'a>(compatibles: impl Iterator<Item = &'a str>) -> Option<Rank> {
    let mut generic = false;
    for c in compatibles {
        if c == COMPATIBLE_APPLE {
            return Some(Rank::Apple);
        }
        generic |= c == COMPATIBLE_GENERIC;
    }
    generic.then_some(Rank::Generic)
}

/// `status` is absent, `"okay"` or `"ok"` (devicetree specification, 2.3.4; the kernel's own rule for nodes it uses).
/// m1n1 removes the property from the node it set up (`kboot.c:237`), and leaves `"disabled"` on one it did not.
pub fn status_usable(status: Option<&str>) -> bool {
    status.is_none_or(|s| s == "okay" || s == "ok")
}

/// The formats a boot loader can name for this node that the kernel can draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// 32 bits, B G R and a padding byte in memory (little-endian `x8r8g8b8`). The fourth byte is padding, not alpha
    /// (kernel patch 0962).
    X8r8g8b8,
    /// As `X8r8g8b8` with an alpha byte nobody reads: drawn the same way.
    A8r8g8b8,
    /// 32 bits per pixel, 10 bits per channel, little-endian: bits 29..20 red, 19..10 green, 9..0 blue, 31..30 padding.
    /// m1n1 writes it for depth 30 (`kboot.c:223-225`).
    X2r10g10b10,
    /// 16 bits, 5-6-5.
    R5g6b5,
}

impl Format {
    /// The `format` string of the binding. m1n1 can write `x8r8g8b8`, `x2r10g10b10` and `r5g6b5` (`kboot.c:219-228`);
    /// `a8r8g8b8` is accepted too, as before.
    pub fn parse(s: &str) -> Option<Format> {
        match s {
            "x8r8g8b8" => Some(Format::X8r8g8b8),
            "a8r8g8b8" => Some(Format::A8r8g8b8),
            "x2r10g10b10" => Some(Format::X2r10g10b10),
            "r5g6b5" => Some(Format::R5g6b5),
            _ => None,
        }
    }

    /// Bytes per pixel in memory.
    pub fn bytes_per_pixel(self) -> usize {
        match self {
            Format::X8r8g8b8 | Format::A8r8g8b8 | Format::X2r10g10b10 => 4,
            Format::R5g6b5 => 2,
        }
    }

    /// The `bpp` that the kernel's `BootloaderFramebufferArg` carries. It is the bits per pixel in memory (32, 16), except
    /// that 10-bit channels, which the framebuffer component must draw differently from 8-bit ones, are 30: m1n1's `depth`
    /// for that format (`kboot.c:223`).
    pub fn kernel_bpp(self) -> usize {
        match self {
            Format::X8r8g8b8 | Format::A8r8g8b8 => 32,
            Format::X2r10g10b10 => 30,
            Format::R5g6b5 => 16,
        }
    }
}

/// A framebuffer the kernel can draw, in the units of `BootloaderFramebufferArg`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Framebuffer {
    pub address: usize,
    pub width: usize,
    pub height: usize,
    /// See [`Format::kernel_bpp`].
    pub bpp: usize,
    /// Bytes from one line to the next.
    pub line_size: usize,
    pub format: Format,
}

/// Checks a node's properties and builds the framebuffer, or `None` if the node is not a usable framebuffer: no or empty
/// `reg`, a zero address, zero width or height, an unknown format, a stride shorter than a line, or a `reg` size shorter than
/// `stride * height`. All the checks overflow-safe. `reg_size` is `None` when the `reg` property has no size cell.
pub fn parse(
    address: usize,
    reg_size: Option<usize>,
    width: usize,
    height: usize,
    stride: usize,
    format: &str,
) -> Option<Framebuffer> {
    let format = Format::parse(format)?;
    if address == 0 || width == 0 || height == 0 {
        return None;
    }
    if stride < width.checked_mul(format.bytes_per_pixel())?
        || reg_size? < stride.checked_mul(height)?
    {
        return None;
    }
    Some(Framebuffer {
        address,
        width,
        height,
        bpp: format.kernel_bpp(),
        line_size: stride,
        format,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apple_compatible_ranks_first_whatever_its_place() {
        assert_eq!(
            rank(["apple,simple-framebuffer", "simple-framebuffer"].into_iter()),
            Some(Rank::Apple)
        );
        assert_eq!(
            rank(["simple-framebuffer", "apple,simple-framebuffer"].into_iter()),
            Some(Rank::Apple)
        );
        assert_eq!(
            rank(["apple,simple-framebuffer"].into_iter()),
            Some(Rank::Apple)
        );
        assert_eq!(
            rank(["simple-framebuffer"].into_iter()),
            Some(Rank::Generic)
        );
        assert!(Rank::Apple < Rank::Generic);
    }

    #[test]
    fn other_nodes_are_not_framebuffers() {
        assert_eq!(rank(core::iter::empty()), None);
        assert_eq!(rank(["arm,pl111"].into_iter()), None);
        assert_eq!(rank(["simple-framebuffer-x"].into_iter()), None);
        assert_eq!(rank(["apple,simple-framebuffers"].into_iter()), None);
    }

    #[test]
    fn status_rule() {
        assert!(status_usable(None));
        assert!(status_usable(Some("okay")));
        assert!(status_usable(Some("ok")));
        // What m1n1 leaves on a node it did not set up (t8132-jxxx.dtsi:29).
        assert!(!status_usable(Some("disabled")));
        assert!(!status_usable(Some("fail")));
        assert!(!status_usable(Some("")));
    }

    #[test]
    fn the_formats_m1n1_writes() {
        // kboot.c:219-228: depth 32, 30 and 16.
        assert_eq!(Format::parse("x8r8g8b8"), Some(Format::X8r8g8b8));
        assert_eq!(Format::parse("x2r10g10b10"), Some(Format::X2r10g10b10));
        assert_eq!(Format::parse("r5g6b5"), Some(Format::R5g6b5));
        assert_eq!(Format::parse("a8r8g8b8"), Some(Format::A8r8g8b8));
        // Formats of the binding that the kernel cannot draw.
        for f in [
            "r8g8b8",
            "a2r10g10b10",
            "x8b8g8r8",
            "a8b8g8r8",
            "x1r5g5b5",
            "a8",
            "",
            "X8R8G8B8",
        ] {
            assert_eq!(Format::parse(f), None, "{f}");
        }
    }

    #[test]
    fn bytes_and_kernel_bpp() {
        assert_eq!(
            (
                Format::X8r8g8b8.bytes_per_pixel(),
                Format::X8r8g8b8.kernel_bpp()
            ),
            (4, 32)
        );
        assert_eq!(
            (
                Format::A8r8g8b8.bytes_per_pixel(),
                Format::A8r8g8b8.kernel_bpp()
            ),
            (4, 32)
        );
        assert_eq!(
            (
                Format::X2r10g10b10.bytes_per_pixel(),
                Format::X2r10g10b10.kernel_bpp()
            ),
            (4, 30)
        );
        assert_eq!(
            (
                Format::R5g6b5.bytes_per_pixel(),
                Format::R5g6b5.kernel_bpp()
            ),
            (2, 16)
        );
    }

    /// A MacBook Air 13-inch panel as iBoot reports it: 2560 x 1664, 32 bits, stride 10240; the notch cuts the height (the
    /// numbers are an example of the geometry rules, not a measurement).
    #[test]
    fn a_plain_apple_node() {
        let fb = parse(
            0x9_c000_0000,
            Some(10240 * 1620),
            2560,
            1620,
            10240,
            "x8r8g8b8",
        )
        .unwrap();
        assert_eq!(
            fb,
            Framebuffer {
                address: 0x9_c000_0000,
                width: 2560,
                height: 1620,
                bpp: 32,
                line_size: 10240,
                format: Format::X8r8g8b8
            }
        );
    }

    #[test]
    fn the_thirty_bit_format_keeps_its_stride() {
        let fb = parse(0x1000, Some(5120 * 100), 1280, 100, 5120, "x2r10g10b10").unwrap();
        assert_eq!(
            (fb.bpp, fb.line_size, fb.format),
            (30, 5120, Format::X2r10g10b10)
        );
        // A stride padded beyond the width, as the display engine aligns lines.
        let fb = parse(0x1000, Some(5376 * 100), 1280, 100, 5376, "x8r8g8b8").unwrap();
        assert_eq!((fb.bpp, fb.line_size), (32, 5376));
    }

    #[test]
    fn sixteen_bits() {
        let fb = parse(0x1000, Some(2048 * 10), 1024, 10, 2048, "r5g6b5").unwrap();
        assert_eq!((fb.bpp, fb.line_size), (16, 2048));
        // 16 bits need only 2 bytes per pixel.
        assert!(parse(0x1000, Some(2000 * 10), 1024, 10, 2000, "r5g6b5").is_none());
    }

    #[test]
    fn what_the_loader_left_alone_is_refused() {
        // reg = <0 0 0 0>, no width: the node m1n1 never set up (t8132-jxxx.dtsi:26, kboot.c:182-185).
        assert!(parse(0, Some(0), 0, 0, 0, "x8r8g8b8").is_none());
        assert!(parse(0, Some(1 << 20), 100, 100, 400, "x8r8g8b8").is_none());
        assert!(parse(0x1000, Some(1 << 20), 0, 100, 400, "x8r8g8b8").is_none());
        assert!(parse(0x1000, Some(1 << 20), 100, 0, 400, "x8r8g8b8").is_none());
        // No size cell in reg.
        assert!(parse(0x1000, None, 100, 100, 400, "x8r8g8b8").is_none());
        // Unknown format.
        assert!(parse(0x1000, Some(1 << 20), 100, 100, 400, "x8b8g8r8").is_none());
    }

    #[test]
    fn geometry_rules() {
        // Stride shorter than a line of 4-byte pixels.
        assert!(parse(0x1000, Some(1 << 20), 100, 100, 399, "x8r8g8b8").is_none());
        assert!(parse(0x1000, Some(1 << 20), 100, 100, 400, "x8r8g8b8").is_some());
        // reg shorter than stride * height by one byte.
        assert!(parse(0x1000, Some(400 * 100 - 1), 100, 100, 400, "x8r8g8b8").is_none());
        assert!(parse(0x1000, Some(400 * 100), 100, 100, 400, "x8r8g8b8").is_some());
        // Overflows are refusals, not wraps.
        assert!(
            parse(
                0x1000,
                Some(usize::MAX),
                usize::MAX / 2,
                2,
                usize::MAX,
                "x8r8g8b8"
            )
            .is_none()
        );
        assert!(parse(0x1000, Some(usize::MAX), 100, usize::MAX, 400, "x8r8g8b8").is_none());
    }
}
