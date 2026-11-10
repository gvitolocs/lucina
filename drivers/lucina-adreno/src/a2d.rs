// SPDX-License-Identifier: GPL-2.0-only
//! The 2D engine: solid fills, copies and scaling with no shader, as Mesa's `fd6_blitter.cc` (MIT) drives it. The encoding of one
//! blit is here; the kernel chooses what to draw and when.
//!
//! Addresses of surfaces must be 64-byte aligned and pitches multiples of 64 (`RB_A2D_DEST_BUFFER_BASE` `align="64"`), so a surface
//! is its whole buffer and a rectangle is a position in it, up to 16383 in each direction.

use alloc::vec::Vec;

use crate::{Result, err, pm4};

// The registers, dwords (`a6xx.xml`).
const RB_A2D_BLT_CNTL: u32 = 0x8c00;
const RB_A2D_DEST_BUFFER_INFO: u32 = 0x8c17;
const RB_A2D_CLEAR_COLOR_DW0: u32 = 0x8c2c;
const GRAS_A2D_BLT_CNTL: u32 = 0x8400;
const GRAS_A2D_SRC_XMIN: u32 = 0x8401;
const SP_A2D_OUTPUT_INFO: u32 = 0xacc0;
const TPL1_A2D_SRC_TEXTURE_INFO: u32 = 0xb4c0;
pub const RB_CCU_CNTL: u32 = 0x8e07;

/// `FMT6_8_8_8_8_UNORM`.
const FORMAT_8888: u32 = 0x30;
/// `WXYZ`: bytes blue, green, red, alpha, which is what the framebuffer has as XRGB8888.
const SWAP_BGRA: u32 = 1;
const MASK_ALL: u32 = 0xf;
/// `BLIT_OP_SCALE`.
const OP_SCALE: u32 = 3;
/// `RM6_BLIT2DSCALE`.
const MODE_BLIT2DSCALE: u32 = 12;
/// `RB_CCU_CNTL` for a blit that does not use GMEM: colour cache at `info->a6xx.ccu_color_offset_bypass`, 0x10000.
pub const CCU_BYPASS: u32 = 0x10 << 23;
// `vgt_event_type`.
const EVENT_CCU_INVALIDATE_DEPTH: u32 = 0x18;
const EVENT_CCU_INVALIDATE_COLOR: u32 = 0x19;
const EVENT_CCU_FLUSH_DEPTH_TS: u32 = 0x1c;
const EVENT_CCU_FLUSH_COLOR_TS: u32 = 0x1d;
const EVENT_CACHE_INVALIDATE: u32 = 0x31;
/// `CP_EVENT_WRITE_0_TIMESTAMP`.
const EVENT_TIMESTAMP: u32 = 1 << 30;

const MAX_COORD: u32 = 0x4000;

/// A buffer in the format of the framebuffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Surface {
    pub address: u64,
    pub pitch: u32,
    pub width: u32,
    pub height: u32,
}

/// A rectangle, `x + w` and `y + h` being exclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Surface {
    fn check(&self) -> Result<()> {
        if !self.address.is_multiple_of(64) {
            return err(
                "a2d: a surface's address must be 64-byte aligned",
                self.address as u32,
            );
        }
        if self.pitch == 0 || !self.pitch.is_multiple_of(64) || self.pitch / 64 > 0xffff {
            return err(
                "a2d: a surface's pitch must be a multiple of 64",
                self.pitch,
            );
        }
        if self.pitch < self.width * 4
            || self.width >= MAX_COORD
            || self.height >= MAX_COORD
            || self.width == 0
            || self.height == 0
        {
            return err("a2d: bad surface size", self.width);
        }
        Ok(())
    }

    pub(crate) fn holds(&self, rect: &Rect) -> Result<()> {
        if rect.w == 0
            || rect.h == 0
            || rect.x + rect.w > self.width
            || rect.y + rect.h > self.height
        {
            return err("a2d: a rectangle outside its surface", rect.x + rect.w);
        }
        Ok(())
    }
}

fn reg(words: &mut Vec<u32>, first: u32, values: &[u32]) {
    words.push(pm4::pkt4(first, values.len() as u32));
    words.extend_from_slice(values);
}

fn event(words: &mut Vec<u32>, event: u32) {
    words.push(pm4::pkt7(pm4::CP_EVENT_WRITE, 1));
    words.push(event);
}

fn event_ts(words: &mut Vec<u32>, event: u32, scratch: u64) {
    words.push(pm4::pkt7(pm4::CP_EVENT_WRITE, 4));
    words.push(event | EVENT_TIMESTAMP);
    words.push(scratch as u32);
    words.push((scratch >> 32) as u32);
    words.push(0);
}

/// The caches' turn around a blit: written pixels leave the colour cache (`scratch` gets a timestamp, 8 bytes of memory), and what
/// the next blit reads is not stale. `fd6_emit_flushes`.
pub fn flush(words: &mut Vec<u32>, scratch: u64) {
    event_ts(words, EVENT_CCU_FLUSH_COLOR_TS, scratch);
    event_ts(words, EVENT_CCU_FLUSH_DEPTH_TS, scratch);
    event(words, EVENT_CCU_INVALIDATE_COLOR);
    event(words, EVENT_CCU_INVALIDATE_DEPTH);
    event(words, EVENT_CACHE_INVALIDATE);
    words.push(pm4::pkt7(pm4::CP_WAIT_FOR_IDLE, 0));
}

/// `emit_setup`, for a run of blits.
pub fn setup(words: &mut Vec<u32>) {
    words.push(pm4::pkt7(pm4::CP_SET_MARKER, 1));
    words.push(MODE_BLIT2DSCALE);
    reg(words, RB_CCU_CNTL, &[CCU_BYPASS]);
}

fn state(words: &mut Vec<u32>, solid: bool, dst: &Surface, rect: &Rect) {
    // `emit_blit_setup`: raw format, with the channels as given.
    let cntl = (u32::from(solid) << 7) | (FORMAT_8888 << 8) | (MASK_ALL << 20);
    reg(words, RB_A2D_BLT_CNTL, &[cntl, 0]);
    reg(words, GRAS_A2D_BLT_CNTL, &[cntl]);
    reg(
        words,
        SP_A2D_OUTPUT_INFO,
        &[(FORMAT_8888 << 3) | (MASK_ALL << 12)],
    );
    reg(
        words,
        RB_A2D_DEST_BUFFER_INFO,
        &[
            FORMAT_8888 | (SWAP_BGRA << 10),
            dst.address as u32,
            (dst.address >> 32) as u32,
            dst.pitch >> 6,
        ],
    );
    let _ = rect;
}

fn run(words: &mut Vec<u32>) {
    words.push(pm4::pkt7(pm4::CP_BLIT, 1));
    words.push(OP_SCALE);
}

const fn xy(x: u32, y: u32) -> u32 {
    x | (y << 16)
}

/// A solid fill of `rect` in `dst`. `colour` is 0xAARRGGBB.
pub fn fill(words: &mut Vec<u32>, dst: &Surface, rect: &Rect, colour: u32) -> Result<()> {
    dst.check()?;
    dst.holds(rect)?;
    state(words, true, dst, rect);
    let [b, g, r, a] = colour.to_le_bytes();
    reg(
        words,
        RB_A2D_CLEAR_COLOR_DW0,
        &[u32::from(r), u32::from(g), u32::from(b), u32::from(a)],
    );
    reg(
        words,
        GRAS_A2D_SRC_XMIN,
        &[
            0,
            0,
            0,
            0,
            xy(rect.x, rect.y),
            xy(rect.x + rect.w - 1, rect.y + rect.h - 1),
        ],
    );
    run(words);
    Ok(())
}

/// A copy of `from` in `src` to `to` in `dst`, scaled if their sizes differ, with a bilinear filter if `filter`. The GPU reads and writes
/// in its own order, so overlapping rectangles on one surface need a copy through a third.
pub fn blit(
    words: &mut Vec<u32>,
    src: &Surface,
    from: &Rect,
    dst: &Surface,
    to: &Rect,
    filter: bool,
) -> Result<()> {
    src.check()?;
    dst.check()?;
    src.holds(from)?;
    dst.holds(to)?;
    state(words, false, dst, to);
    // `UNK20`, `UNK22`: Mesa always sets them here.
    let info = FORMAT_8888 | (SWAP_BGRA << 10) | (u32::from(filter) << 16) | (1 << 20) | (1 << 22);
    reg(
        words,
        TPL1_A2D_SRC_TEXTURE_INFO,
        &[
            info,
            src.width | (src.height << 15),
            src.address as u32,
            (src.address >> 32) as u32,
            // `PITCH`, bits 9.. in units of 64 bytes.
            (src.pitch >> 6) << 9,
        ],
    );
    reg(
        words,
        GRAS_A2D_SRC_XMIN,
        &[
            from.x << 8,
            (from.x + from.w - 1) << 8,
            from.y << 8,
            (from.y + from.h - 1) << 8,
            xy(to.x, to.y),
            xy(to.x + to.w - 1, to.y + to.h - 1),
        ],
    );
    run(words);
    Ok(())
}

/// `R2D_UNORM8`, for [`blit_yuv`].
const IFMT_UNORM8: u32 = 0x10;

/// A probe, not a path anything uses: a picture in YUV, planes as [`crate::yuv_rules::fits`] accepted them, copied by A2D with its
/// source in `FMT6_R8_G8B8_2PLANE_420_UNORM` or `_3PLANE_`. Nobody has documented whether A2D converts colour or hands the planes'
/// bytes on as they are; `lucina-gpu yuv` reads back which. The 3D path ([`crate::quad::YUV_FRAGMENT_SHADER`]) does the conversion
/// itself either way.
pub fn blit_yuv(
    words: &mut Vec<u32>,
    address: u64,
    layout: &crate::yuv_rules::Layout,
    from: &Rect,
    dst: &Surface,
    to: &Rect,
    filter: bool,
) -> Result<()> {
    use crate::yuv_rules::Planar;
    dst.check()?;
    dst.holds(to)?;
    if from.w == 0
        || from.h == 0
        || from.x + from.w > layout.width
        || from.y + from.h > layout.height
    {
        return err("a2d: a rectangle outside its picture", from.x + from.w);
    }
    let format = match layout.planar {
        Planar::Two => 0x8e,
        Planar::Three => 0x90,
    };
    let cntl = (FORMAT_8888 << 8) | (MASK_ALL << 20) | (IFMT_UNORM8 << 24);
    reg(words, RB_A2D_BLT_CNTL, &[cntl, 0]);
    reg(words, GRAS_A2D_BLT_CNTL, &[cntl]);
    reg(words, SP_A2D_OUTPUT_INFO, &[1 | (FORMAT_8888 << 3) | (MASK_ALL << 12)]);
    reg(
        words,
        RB_A2D_DEST_BUFFER_INFO,
        &[
            FORMAT_8888 | (SWAP_BGRA << 10),
            dst.address as u32,
            (dst.address >> 32) as u32,
            dst.pitch >> 6,
        ],
    );
    let (y, c, v) = (
        address + layout.y_offset,
        address + layout.c_offset,
        address + layout.v_offset,
    );
    let info = format | (u32::from(filter) << 16) | (1 << 20) | (1 << 22);
    reg(
        words,
        TPL1_A2D_SRC_TEXTURE_INFO,
        &[
            info,
            layout.width | (layout.height << 15),
            y as u32,
            (y >> 32) as u32,
            (layout.y_pitch >> 6) << 9,
            // `SP_PS_2D_SRC_PLANE1`, `_PLANE_PITCH`, `_PLANE2` (`a6xx.xml`: "planes for NV12, etc. (TODO: not tested)").
            c as u32,
            (c >> 32) as u32,
            layout.c_pitch >> 6,
            if layout.planar == Planar::Three { v as u32 } else { 0 },
            if layout.planar == Planar::Three { (v >> 32) as u32 } else { 0 },
        ],
    );
    reg(
        words,
        GRAS_A2D_SRC_XMIN,
        &[
            from.x << 8,
            (from.x + from.w - 1) << 8,
            from.y << 8,
            (from.y + from.h - 1) << 8,
            xy(to.x, to.y),
            xy(to.x + to.w - 1, to.y + to.h - 1),
        ],
    );
    run(words);
    Ok(())
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::vec;

    use super::*;

    const FB: Surface = Surface {
        address: 0x9c00_0000,
        pitch: 7680,
        width: 1920,
        height: 1080,
    };

    /// The registers a command stream sets, in order, read back through its own headers.
    fn regs_in(words: &[u32]) -> std::vec::Vec<(u32, u32)> {
        let mut found = std::vec::Vec::new();
        let mut at = 0;
        while at < words.len() {
            let header = words[at];
            match header >> 28 {
                4 => {
                    let count = header & 0x7f;
                    let first = (header >> 8) & 0x3ffff;
                    for i in 0..count {
                        found.push((first + i, words[at + 1 + i as usize]));
                    }
                    at += 1 + count as usize;
                }
                7 => at += 1 + (header & 0x3fff) as usize,
                other => panic!("bad packet type {} at {}", other, at),
            }
        }
        found
    }

    #[test]
    fn a_fill_sets_what_mesa_sets() {
        let mut words = vec![];
        fill(
            &mut words,
            &FB,
            &Rect {
                x: 10,
                y: 20,
                w: 100,
                h: 50,
            },
            0x4411_2233,
        )
        .unwrap();
        let regs = regs_in(&words);
        let get = |r: u32| regs.iter().find(|(a, _)| *a == r).map(|(_, v)| *v).unwrap();
        // Solid colour, FMT6_8_8_8_8_UNORM, all channels, raw UNORM8.
        assert_eq!(get(RB_A2D_BLT_CNTL), 0x00f0_3080);
        assert_eq!(get(RB_A2D_BLT_CNTL + 1), 0);
        assert_eq!(get(GRAS_A2D_BLT_CNTL), 0x00f0_3080);
        assert_eq!(get(SP_A2D_OUTPUT_INFO), 0xf180);
        assert_eq!(get(RB_A2D_DEST_BUFFER_INFO), 0x430);
        assert_eq!(get(RB_A2D_DEST_BUFFER_INFO + 1), 0x9c00_0000);
        assert_eq!(get(RB_A2D_DEST_BUFFER_INFO + 3), 120);
        assert_eq!(get(RB_A2D_CLEAR_COLOR_DW0), 0x11);
        assert_eq!(get(RB_A2D_CLEAR_COLOR_DW0 + 1), 0x22);
        assert_eq!(get(RB_A2D_CLEAR_COLOR_DW0 + 2), 0x33);
        assert_eq!(get(RB_A2D_CLEAR_COLOR_DW0 + 3), 0x44);
        assert_eq!(get(GRAS_A2D_SRC_XMIN + 4), 10 | (20 << 16));
        assert_eq!(get(GRAS_A2D_SRC_XMIN + 5), 109 | (69 << 16));
        assert_eq!(words[words.len() - 1], OP_SCALE);
        assert_eq!(words[words.len() - 2], pm4::pkt7(pm4::CP_BLIT, 1));
    }

    #[test]
    fn a_blit_says_where_the_source_is() {
        let src = Surface {
            address: 0x1_0000_0040,
            pitch: 1024,
            width: 256,
            height: 128,
        };
        let mut words = vec![];
        blit(
            &mut words,
            &src,
            &Rect {
                x: 1,
                y: 2,
                w: 3,
                h: 4,
            },
            &FB,
            &Rect {
                x: 5,
                y: 6,
                w: 7,
                h: 8,
            },
            true,
        )
        .unwrap();
        let regs = regs_in(&words);
        let get = |r: u32| regs.iter().find(|(a, _)| *a == r).map(|(_, v)| *v).unwrap();
        assert_eq!(
            get(TPL1_A2D_SRC_TEXTURE_INFO),
            0x30 | (1 << 10) | (1 << 16) | (1 << 20) | (1 << 22)
        );
        assert_eq!(get(TPL1_A2D_SRC_TEXTURE_INFO + 1), 256 | (128 << 15));
        assert_eq!(get(TPL1_A2D_SRC_TEXTURE_INFO + 2), 0x40);
        assert_eq!(get(TPL1_A2D_SRC_TEXTURE_INFO + 3), 1);
        assert_eq!(get(TPL1_A2D_SRC_TEXTURE_INFO + 4), 16 << 9);
        assert_eq!(get(GRAS_A2D_SRC_XMIN), 1 << 8);
        assert_eq!(get(GRAS_A2D_SRC_XMIN + 1), 3 << 8);
        assert_eq!(get(GRAS_A2D_SRC_XMIN + 2), 2 << 8);
        assert_eq!(get(GRAS_A2D_SRC_XMIN + 3), 5 << 8);
    }

    #[test]
    fn what_the_hardware_cannot_address_is_refused() {
        let mut words = vec![];
        let odd = Surface {
            address: 0x20,
            ..FB
        };
        assert!(
            fill(
                &mut words,
                &odd,
                &Rect {
                    x: 0,
                    y: 0,
                    w: 1,
                    h: 1
                },
                0
            )
            .is_err()
        );
        let tight = Surface { pitch: 100, ..FB };
        assert!(
            fill(
                &mut words,
                &tight,
                &Rect {
                    x: 0,
                    y: 0,
                    w: 1,
                    h: 1
                },
                0
            )
            .is_err()
        );
        assert!(
            fill(
                &mut words,
                &FB,
                &Rect {
                    x: 1919,
                    y: 0,
                    w: 2,
                    h: 1
                },
                0
            )
            .is_err()
        );
        assert!(
            fill(
                &mut words,
                &FB,
                &Rect {
                    x: 0,
                    y: 0,
                    w: 0,
                    h: 1
                },
                0
            )
            .is_err()
        );
    }

    #[test]
    fn the_cache_turn_has_each_event_mesa_uses() {
        let mut words = vec![];
        flush(&mut words, 0x1000);
        assert!(
            words
                .windows(2)
                .any(|w| w[1] == (EVENT_CCU_FLUSH_COLOR_TS | EVENT_TIMESTAMP))
        );
        assert!(words.windows(2).any(|w| w[1] == EVENT_CCU_INVALIDATE_COLOR));
        assert_eq!(*words.last().unwrap(), pm4::pkt7(pm4::CP_WAIT_FOR_IDLE, 0));
    }
}
