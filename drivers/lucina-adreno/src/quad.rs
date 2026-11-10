// SPDX-License-Identifier: GPL-2.0-only
//! A textured quad on the 3D pipeline, the one thing A2D cannot do: alpha blending and tint. The state follows Turnip as Scarlet's
//! `sgfx-codegen-adreno-a6xx` emits it, and the two shaders are from its A618 pack, compiled by Mesa's IR3 at commit
//! `3f1b217baffffa00cb8f53e158713a33e1bd4632` (`vs_stride24_pos4_uv2`, `fs_texture_rgba`; GPL-2.0-only, see that pack's NOTICE).
//!
//! No floating point: this runs in the kernel, which does not save FP registers. Values reach the GPU as IEEE 754 bits, from
//! [`ratio`].

use alloc::vec::Vec;

use crate::{
    Result,
    a2d::{Rect, Surface},
    err, pm4,
};

/// `(rpt3)mul.f`, `mad.f32` x3, `end`: the position through the matrix in c0..c3. 128-byte alignment is the hardware's.
pub const VERTEX_SHADER: [u32; 10] = [
    0x0000_1000,
    0x4070_0b06,
    0x2006_1004,
    0x6380_8b06,
    0x2006_1008,
    0x6381_0b06,
    0x2006_100c,
    0x6381_8b0a,
    0,
    0x0300_0000,
];
/// `bary.f` with sampler pre-dispatch, `(rpt3)mul.f` by c4, `end`: the texture times the tint.
pub const FRAGMENT_SHADER: [u32; 6] = [
    0x0000_2000,
    0x4730_80fc,
    0x1010_0002,
    0x5078_0b06,
    0,
    0x0300_0000,
];
/// Where the fragment shader goes in the shader memory.
pub const FRAGMENT_OFFSET: u32 = 128;
const SHADER_BYTES: u32 = 128;

/// The fragment shader for a picture in YUV: the texture's (Y, Cb, Cr), which is what a 2- or 3-plane 4:2:0 texture gives in x, y, z, times
/// the 3x4 matrix in c4..c6, with alpha from c7.x. Written for this pipeline's registers (input r0.z.., output r1.z..) and assembled by
/// Mesa 25.2.6's IR3 assembler for the A618, which also re-assembled [`FRAGMENT_SHADER`] to the same words:
///
/// ```text
/// bary.f (ei)r63.x, 0, r0.x
/// (sy)mad.f32 r1.z, c4.x, r0.z, c4.w
/// mad.f32 r1.w, c5.x, r0.z, c5.w
/// mad.f32 r2.x, c6.x, r0.z, c6.w
/// mad.f32 r1.z, c4.y, r0.w, r1.z
/// mad.f32 r1.w, c5.y, r0.w, r1.w
/// mad.f32 r2.x, c6.y, r0.w, r2.x
/// mad.f32 r1.z, c4.z, r1.x, r1.z
/// mad.f32 r1.w, c5.z, r1.x, r1.w
/// mad.f32 r2.x, c6.z, r1.x, r2.x
/// mov.f32f32 r2.y, c7.x
/// nop
/// end
/// ```
///
/// A mad's third source waits one slot for the result before it (Mesa's `ir3_delayslots`); here it has two.
pub const YUV_FRAGMENT_SHADER: [u32; 26] = [
    0x0000_2000,
    0x4730_80fc,
    0x1013_1010,
    0x7381_0006,
    0x1017_1014,
    0x6381_0007,
    0x101b_1018,
    0x6381_0008,
    0x0006_1011,
    0x6381_8006,
    0x0007_1015,
    0x6381_8007,
    0x0008_1019,
    0x6381_8008,
    0x0006_1012,
    0x6382_0006,
    0x0007_1016,
    0x6382_0007,
    0x0008_101a,
    0x6382_0008,
    0x0000_001c,
    0x2024_4009,
    0,
    0,
    0,
    0x0300_0000,
];
/// Where [`YUV_FRAGMENT_SHADER`] goes in the shader memory.
pub const YUV_FRAGMENT_OFFSET: u32 = 256;

/// Bytes of one texture state: a 16-word descriptor, then a 4-word sampler.
pub const TEXTURE_STATE_BYTES: u32 = 80;
const SAMPLER_OFFSET: u64 = 64;
pub const SAMPLER_NEAREST: [u32; 4] = [0x920, 0x40, 0, 0];
pub const SAMPLER_LINEAR: [u32; 4] = [0x92a, 0x40, 0x20, 0];

/// Bytes of the vertices of one quad: 6 vertices of 6 words.
pub const VERTEX_BYTES: u32 = 6 * 24;

// Packets.
const CP_LOAD_STATE6_GEOM: u8 = 0x32;
const CP_LOAD_STATE6_FRAG: u8 = 0x34;
const CP_DRAW_INDX_OFFSET: u8 = 0x38;
const CP_SKIP_IB2_ENABLE_GLOBAL: u8 = 0x1d;
const CP_SKIP_IB2_ENABLE_LOCAL: u8 = 0x23;
const CP_SET_VISIBILITY_OVERRIDE: u8 = 0x64;
const CP_REG_WRITE: u8 = 0x6d;

/// IEEE 754 single of `numerator / denominator`, rounded down in magnitude.
pub const fn ratio(numerator: i64, denominator: i64) -> u32 {
    if numerator == 0 {
        return 0;
    }
    let sign = ((numerator < 0) != (denominator < 0)) as u32;
    let mut n = numerator.unsigned_abs() as u128;
    let d = denominator.unsigned_abs() as u128;
    let mut exponent = 0i32;
    // Scale so that n / d is in [2^23, 2^24).
    while n < d << 23 {
        n <<= 1;
        exponent -= 1;
    }
    while n >= d << 24 {
        n >>= 1;
        exponent += 1;
    }
    let mantissa = (n / d) as u32 & 0x7f_ffff;
    (sign << 31) | (((exponent + 23 + 127) as u32) << 23) | mantissa
}

const ONE: u32 = 0x3f80_0000;

/// `guardband_axis` for a viewport covering `size` pixels from 0: the clip adjustment as 3.6 floating point.
fn guardband(size: u32) -> u32 {
    const MAX: u32 = 0x1ff;
    // (32767 - size / 2) / (size / 2), the smaller side.
    let bits = ratio(2 * 32767 - i64::from(size), i64::from(size));
    let exponent = ((bits >> 23) & 0xff) as i32 - 127;
    if !(0..=7).contains(&exponent) {
        return MAX;
    }
    ((exponent as u32) << 6) | ((bits >> 17) & 0x3f)
}

/// The descriptor `A6XX_TEX_MEMOBJ` for a linear surface in the framebuffer's format: BGRA, identity swizzle, one level.
pub fn texture_state(surface: &Surface, filter: bool) -> [u32; 20] {
    let mut state = [0u32; 20];
    state[0] = 0x4c00_6880;
    state[1] = surface.width | (surface.height << 15);
    state[2] = (surface.pitch << 7) | (1 << 29);
    state[3] = (surface.pitch * surface.height).div_ceil(4096);
    state[4] = surface.address as u32;
    // `DEPTH` 1 above the address.
    state[5] = (surface.address >> 32) as u32 | (1 << 17);
    state[16..].copy_from_slice(if filter {
        &SAMPLER_LINEAR
    } else {
        &SAMPLER_NEAREST
    });
    state
}

/// The six vertices of `to` in `dst`, sampling `from` in `src`. Clip space; `+Y` is the top.
pub fn vertices(src: &Surface, from: &Rect, dst: &Surface, to: &Rect) -> [u32; 36] {
    let x = |px: u32| {
        ratio(
            2 * i64::from(px) - i64::from(dst.width),
            i64::from(dst.width),
        )
    };
    let y = |py: u32| {
        ratio(
            i64::from(dst.height) - 2 * i64::from(py),
            i64::from(dst.height),
        )
    };
    let u = |px: u32| ratio(i64::from(px), i64::from(src.width));
    let v = |py: u32| ratio(i64::from(py), i64::from(src.height));
    let (l, r, t, b) = (x(to.x), x(to.x + to.w), y(to.y), y(to.y + to.h));
    let (ul, ur, vt, vb) = (u(from.x), u(from.x + from.w), v(from.y), v(from.y + from.h));
    let corner = |px: u32, py: u32, tu: u32, tv: u32| [px, py, 0, ONE, tu, tv];
    let tl = corner(l, t, ul, vt);
    let tr = corner(r, t, ur, vt);
    let bl = corner(l, b, ul, vb);
    let br = corner(r, b, ur, vb);
    let mut out = [0u32; 36];
    for (i, vertex) in [tl, bl, tr, tr, bl, br].iter().enumerate() {
        out[i * 6..i * 6 + 6].copy_from_slice(vertex);
    }
    out
}

/// c0..c3 the identity matrix, c4 the tint, 0xAARRGGBB.
fn uniforms(tint: u32) -> [u32; 20] {
    let [b, g, r, a] = tint.to_le_bytes();
    let channel = |c: u8| ratio(i64::from(c), 255);
    [
        ONE,
        0,
        0,
        0,
        0,
        ONE,
        0,
        0,
        0,
        0,
        ONE,
        0,
        0,
        0,
        0,
        ONE,
        channel(r),
        channel(g),
        channel(b),
        channel(a),
    ]
}

fn reg(words: &mut Vec<u32>, first: u32, values: &[u32]) {
    words.push(pm4::pkt4(first, values.len() as u32));
    words.extend_from_slice(values);
}

fn pkt(words: &mut Vec<u32>, opcode: u8, values: &[u32]) {
    words.push(pm4::pkt7(opcode, values.len() as u32));
    words.extend_from_slice(values);
}

fn addr(words: &mut Vec<u32>, first: u32, address: u64) {
    reg(words, first, &[address as u32, (address >> 32) as u32]);
}

/// `restore_3d_baseline`: from the upstream sysmem restore, what a stream this small can reach.
pub fn baseline(words: &mut Vec<u32>) {
    reg(words, 0xbb08, &[0xff]); // SP_UPDATE_CNTL
    words.push(pm4::pkt7(pm4::CP_WAIT_FOR_IDLE, 0));
    reg(words, 0x8e04, &[0x0410_0000]); // RB_DBG_ECO_CNTL
    reg(words, 0x80a0, &[2]); // GRAS_SC_CNTL
    reg(words, 0x8100, &[0]); // GRAS_LRZ_CNTL
    reg(words, 0x8898, &[0]); // RB_LRZ_CNTL
    reg(words, 0xa007, &[0]); // VFD_RENDER_MODE
    reg(words, 0xa009, &[3]); // VFD_MODE_CNTL
    reg(words, 0x9804, &[0x1f]); // PC_MODE_CNTL
    reg(words, 0x9b07, &[0]); // PC_STEREO_RENDERING_CNTL
    reg(words, 0xab00, &[5]); // SP_MODE_CNTL
    reg(words, 0xb309, &[0xa2]); // TPL1_MODE_CNTL
    reg(words, 0x8811, &[0x10]); // RB_MODE_CNTL
    reg(words, 0x80a1, &[0x00c0_0000]); // GRAS_SC_BIN_CNTL
    reg(words, 0x8800, &[0x00c0_0000]); // RB_CNTL
    reg(words, 0x88d3, &[0]); // RB_BIN_CONTROL2
    reg(words, 0x8890, &[0]); // RB_WINDOW_OFFSET
    reg(words, 0x88d4, &[0]); // RB_WINDOW_OFFSET2
    reg(words, 0xb4d1, &[0]); // SP_WINDOW_OFFSET
    reg(words, 0xb307, &[0]); // TPL1_WINDOW_OFFSET
    reg(words, 0x80af, &[0]); // GRAS_SC_SCREEN_SCISSOR_CNTL
    // Sysmem has one geometry pass, so stream-out stays on or nothing reaches the rasterizer.
    reg(words, 0x9306, &[0]); // VPC_SO_OVERRIDE
    pkt(words, pm4::CP_SET_MARKER, &[1]); // RM6_DIRECT_RENDER
    pkt(words, CP_SKIP_IB2_ENABLE_GLOBAL, &[0]);
    pkt(words, CP_SKIP_IB2_ENABLE_LOCAL, &[1]);
    pkt(words, CP_SET_VISIBILITY_OVERRIDE, &[1]);
    reg(words, crate::a2d::RB_CCU_CNTL, &[crate::a2d::CCU_BYPASS]);
}

/// What stays the same for a run of quads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pass {
    pub dst: Surface,
    pub blend: bool,
    /// With `blend`: the source's colour is already multiplied by its alpha, as WebRender's and Vello's pixels are.
    pub premultiplied: bool,
    /// Address of the shader memory: [`VERTEX_SHADER`], then [`FRAGMENT_SHADER`] at [`FRAGMENT_OFFSET`].
    pub shaders: u64,
    /// The source is a picture in YUV: [`YUV_FRAGMENT_SHADER`], with [`matrix`] instead of [`tint`].
    pub yuv: bool,
}

const FORMAT_8888: u32 = 0x30;

/// The whole state of a [`Pass`], in `Emitter::draw`'s order. After this [`draw`] needs no more until the pass changes.
pub fn pass(words: &mut Vec<u32>, pass: &Pass) -> Result<()> {
    let dst = &pass.dst;
    if !dst.address.is_multiple_of(64) || !dst.pitch.is_multiple_of(64) || dst.width == 0 || dst.height == 0 {
        return err("quad: bad target", dst.pitch);
    }
    let (blend, xy_br) = (pass.blend, (dst.width - 1) | ((dst.height - 1) << 16));
    // emit_program_config
    reg(words, 0xb800, &[0x103, 0, 0, 0]); // SP_VS_CONST_CONFIG
    reg(words, 0xbb10, &[0x102]); // SP_PS_CONST_CONFIG
    reg(words, 0xa823, &[0x100]); // SP_VS_CONFIG
    reg(words, 0xa83b, &[0]); // SP_HS_CONFIG
    reg(words, 0xa863, &[0]); // SP_DS_CONFIG
    reg(words, 0xa894, &[0]); // SP_GS_CONFIG
    reg(words, 0xab04, &[0x2_0300]); // SP_PS_CONFIG
    reg(words, 0xab20, &[0]); // SP_GFX_USIZE
    // The CP watches RB_RENDER_CNTL for a hang workaround: not a plain write.
    pkt(words, CP_REG_WRITE, &[2, 0x8801, 0x10]);
    // RB_MRT: straight, or source over: rgb src * a + dst * (1 - a), or src + dst * (1 - a) when premultiplied, alpha src + dst * (1 - a).
    reg(
        words,
        0x8820,
        &[
            if blend { 0x7e3 } else { 0x7e0 },
            match (blend, pass.premultiplied) {
                (false, _) => 0x0001_0001,
                (true, false) => 0x0701_0706,
                (true, true) => 0x0701_0701,
            },
        ],
    );
    reg(words, 0x8822, &[FORMAT_8888 | (1 << 13)]); // RB_MRT_BUF_INFO: BGRA
    reg(
        words,
        0x8823,
        &[dst.pitch >> 6, (dst.pitch * dst.height) >> 6],
    ); // RB_MRT_PITCH
    addr(words, 0x8825, dst.address); // RB_MRT_BASE
    reg(words, 0x8827, &[0]); // RB_MRT_BASE_GMEM
    reg(words, 0x8903, &[0; 3]); // RB_COLOR_FLAG_BUFFER_ADDR
    reg(words, 0x8102, &[FORMAT_8888]); // GRAS_LRZ_MRT_BUFFER_INFO_0
    reg(words, 0x880f, &[0]); // RB_SRGB_CNTL
    reg(words, 0xa98a, &[0]); // SP_SRGB_CNTL
    reg(words, 0x8004, &[0]); // GRAS_CL_ARRAY_SIZE
    reg(
        words,
        0x8865,
        &[if blend { 0xffff_0001 } else { 0xffff_0000 }],
    ); // RB_BLEND_CNTL
    reg(words, 0xa989, &[u32::from(blend)]); // SP_BLEND_CNTL
    reg(words, 0x880e, &[0]); // RB_DITHER_CNTL
    reg(words, 0xa98d, &[1]); // SP_PS_MRT_CNTL
    reg(words, 0x880c, &[1]); // RB_PS_MRT_CNTL
    reg(words, 0xa996, &[FORMAT_8888]); // SP_PS_MRT_REG
    // emit_depth_state, no depth
    reg(words, 0x8864, &[0]); // RB_ALPHA_TEST_CNTL
    reg(words, 0x8880, &[0]); // RB_STENCIL_CNTL
    reg(words, 0x8115, &[0]); // GRAS_SU_STENCIL_CNTL
    reg(words, 0x8887, &[0]); // RB_STENCIL_REF_CNTL
    reg(words, 0x8888, &[0, 0]); // RB_STENCIL_MASK
    reg(words, 0x8870, &[0]); // RB_DEPTH_PLANE_CNTL
    reg(words, 0x8094, &[0]); // GRAS_SU_DEPTH_PLANE_CNTL
    reg(words, 0x8878, &[0, ONE]); // RB_DEPTH_BOUND_MIN
    reg(words, 0x8881, &[0]); // RB_STENCIL_BUFFER_INFO
    reg(words, 0x8871, &[0]); // RB_DEPTH_CNTL
    reg(words, 0x8114, &[0]); // GRAS_SU_DEPTH_CNTL
    reg(words, 0x8872, &[0; 6]); // RB_DEPTH_BUFFER_INFO
    reg(words, 0x8098, &[0]); // GRAS_SU_DEPTH_BUFFER_INFO
    // emit_single_sample_state
    reg(words, 0xb300, &[0, 4]); // TPL1_RAS_MSAA_CNTL
    reg(words, 0x80a2, &[0, 4, 0]); // GRAS_SC_RAS_MSAA_CNTL
    reg(words, 0x8802, &[0, 4, 0]); // RB_RAS_MSAA_CNTL
    reg(words, 0xb304, &[0]); // TPL1_MSAA_SAMPLE_POS_CNTL
    reg(words, 0x88d5, &[0]); // RB_RESOLVE_GMEM_BUFFER_INFO
    // The viewport is the whole target; clip space maps to its pixels. Z: half scale, half offset.
    let half_w = ratio(i64::from(dst.width), 2);
    let half_h = ratio(i64::from(dst.height), 2);
    let half = ratio(1, 2);
    // GRAS_CL_VIEWPORT_XOFFSET..
    reg(
        words,
        0x8010,
        &[half_w, half_w, half_h, half_h | (1 << 31), half, half],
    );
    reg(
        words,
        0x8006,
        &[guardband(dst.width) | (guardband(dst.height) << 10)],
    ); // GRAS_CL_GUARDBAND_CLIP_ADJ
    reg(words, 0x80b0, &[0, xy_br]); // GRAS_SC_SCREEN_SCISSOR_TL
    reg(words, 0x80d0, &[0, xy_br]); // GRAS_SC_VIEWPORT_SCISSOR_TL
    reg(words, 0x80f0, &[0, xy_br]); // GRAS_SC_WINDOW_SCISSOR_TL
    reg(words, 0x8000, &[0x80]); // GRAS_CL_CNTL
    reg(words, 0x8090, &[0x2010]); // GRAS_SU_CNTL, no culling
    reg(words, 0x8091, &[0x0010_0010]); // GRAS_SU_POINT_MINMAX
    reg(words, 0x8092, &[0x10]); // GRAS_SU_POINT_SIZE
    reg(words, 0x8095, &[0, 0, 0]); // GRAS_SU_POLY_OFFSET_SCALE
    reg(words, 0x9b00, &[0]); // PC_CNTL
    reg(words, 0x9108, &[3]); // VPC_RAST_CNTL
    reg(words, 0x9981, &[3]); // PC_DGEN_RAST_CNTL
    // Vertices: vec4 position, vec2 texture coordinate, 24 bytes.
    reg(words, 0xa000, &[2 | (2 << 8)]); // VFD_CNTL_0
    reg(
        words,
        0xa001,
        &[
            0xfcfc_fcfc,
            0x0000_fcfc,
            0xfcfc_fcfc,
            0x0000_00fc,
            0x0000_fcfc,
            0,
        ],
    ); // VFD_CNTL_1..6
    reg(words, 0xa090, &[0xc820_0000, 1, 0xc670_0200, 1]); // VFD_FETCH_INSTR
    reg(words, 0xa0d0, &[15, 67]); // VFD_DEST_CNTL
    // emit_vs
    reg(words, 0xa800, &[0x0010_0200]); // SP_VS_CNTL_0
    reg(words, 0xa802, &[2]); // SP_VS_OUTPUT_CNTL
    reg(words, 0xa803, &[0x0f0a_0304]); // SP_VS_OUTPUT_REG
    reg(words, 0xa813, &[512]); // SP_VS_VPC_DEST_REG
    reg(words, 0xa824, &[1]); // SP_VS_INSTR_SIZE
    reg(words, 0x9212, &[0xffff_fffc, u32::MAX, u32::MAX, u32::MAX]); // VPC_VARYING_LM_TRANSFER_CNTL_DISABLE
    reg(words, 0x9301, &[0x00ff_0206]); // VPC_VS_CNTL
    reg(words, 0x9101, &[0x00ff_ff00]); // VPC_VS_CLIP_CULL_CNTL
    reg(words, 0x9311, &[0x00ff_ff00]); // VPC_VS_CLIP_CULL_CNTL_V2
    reg(words, 0x8001, &[0]); // GRAS_CL_VS_CLIP_CULL_DISTANCE
    reg(words, 0x9304, &[0xff01_ff02]); // VPC_PS_CNTL
    reg(words, 0x9b01, &[6]); // PC_VS_CNTL
    reg(words, 0x9104, &[0x0000_ffff]); // VPC_VS_SIV_CNTL
    reg(words, 0x9314, &[0x0000_ffff]); // VPC_VS_SIV_CNTL_V2
    reg(words, 0x809b, &[0]); // GRAS_SU_VS_SIV_CNTL
    reg(words, 0x9806, &[0]); // PC_PS_CNTL
    // emit_fs
    reg(words, 0xa980, &[0x8150_0180]); // SP_PS_CNTL_0
    reg(words, 0xa99e, &[1, 0x23c2_0000]); // SP_PS_INITIAL_TEX_LOAD_CNTL, _CMD
    reg(words, 0xa9a3, &[0]); // SP_PS_INITIAL_TEX_INDEX_CMD
    reg(words, 0xa9a7, &[1]); // SP_PS_TSIZE
    reg(
        words,
        0xb983,
        &[0xfcfc_fcfc, 0xfcfc_fc00, 0xfcfc_fcfc, 0xfcfc],
    ); // SP_REG_PROG_ID_0
    reg(words, 0xa98c, &[0xfcfc_fc00]); // SP_PS_OUTPUT_CNTL
    reg(words, 0xa98e, &[6]); // SP_PS_OUTPUT_REG
    reg(words, 0xa98b, &[15]); // SP_PS_OUTPUT_MASK
    reg(words, 0x880b, &[0]); // RB_PS_OUTPUT_CNTL
    reg(words, 0x880d, &[15]); // RB_PS_OUTPUT_MASK
    reg(words, 0xab05, &[1]); // SP_PS_INSTR_SIZE
    reg(words, 0xb980, &[3]); // SP_PS_WAVE_CNTL
    reg(words, 0xb982, &[7]); // SP_LB_PARAM_LIMIT
    reg(words, 0x8005, &[1]); // GRAS_CL_INTERP_CNTL
    reg(words, 0x8809, &[0x401]); // RB_INTERP_CNTL
    reg(words, 0x880a, &[0]); // RB_PS_INPUT_CNTL
    reg(words, 0x9200, &[0; 8]); // VPC_VARYING_INTERP_MODE
    reg(words, 0x9208, &[0; 8]); // VPC_VARYING_REPLACE_MODE
    reg(words, 0x8810, &[0]); // RB_PS_SAMPLEFREQ_CNTL
    reg(words, 0x8101, &[0]); // GRAS_LRZ_PS_INPUT_CNTL
    reg(words, 0x8109, &[0]); // GRAS_LRZ_PS_SAMPLEFREQ_CNTL
    // The programs: their layout as one burst, then the preload the GPU also needs.
    let vs = pass.shaders;
    let fs = pass.shaders
        + u64::from(if pass.yuv {
            YUV_FRAGMENT_OFFSET
        } else {
            FRAGMENT_OFFSET
        });
    let layout = |words: &mut Vec<u32>, first: u32, address: u64| {
        reg(
            words,
            first,
            &[0, address as u32, (address >> 32) as u32, 0, 0, 0, 0],
        );
    };
    layout(words, 0xa81b, vs); // SP_VS_PROGRAM_COUNTER_OFFSET
    reg(words, 0xa825, &[0]); // SP_VS_PVT_MEM_STACK_OFFSET
    pkt(
        words,
        CP_LOAD_STATE6_GEOM,
        &[
            (2 << 16) | (8 << 18) | (1 << 22),
            vs as u32,
            (vs >> 32) as u32,
        ],
    );
    layout(words, 0xa982, fs); // SP_PS_PROGRAM_COUNTER_OFFSET
    reg(words, 0xa9a9, &[0]); // SP_PS_PVT_MEM_STACK_OFFSET
    pkt(
        words,
        CP_LOAD_STATE6_FRAG,
        &[
            (2 << 16) | (12 << 18) | (1 << 22),
            fs as u32,
            (fs >> 32) as u32,
        ],
    );
    Ok(())
}

/// The source of the next quads: address of a [`texture_state`]. Descriptor and sampler, state type 1 and 0 of block SB6_FS_TEX.
/// This is all a change of tile costs.
pub fn texture(words: &mut Vec<u32>, texture: u64) {
    let sampler = texture + SAMPLER_OFFSET;
    addr(words, 0xa9e0, sampler); // SP_PS_SAMPLER_BASE
    addr(words, 0xa9e4, texture); // SP_PS_TEXMEMOBJ_BASE
    pkt(
        words,
        CP_LOAD_STATE6_FRAG,
        &[
            (2 << 16) | (4 << 18) | (1 << 22),
            sampler as u32,
            (sampler >> 32) as u32,
        ],
    );
    pkt(
        words,
        CP_LOAD_STATE6_FRAG,
        &[
            (1 << 14) | (2 << 16) | (4 << 18) | (1 << 22),
            texture as u32,
            (texture >> 32) as u32,
        ],
    );
}

/// The next quads' tint, 0xAARRGGBB, and the identity matrix.
pub fn tint(words: &mut Vec<u32>, tint: u32) {
    let values = uniforms(tint);
    let load = |words: &mut Vec<u32>, opcode: u8, block: u32| {
        words.push(pm4::pkt7(opcode, 3 + values.len() as u32));
        words.extend_from_slice(&[(1 << 14) | (block << 18) | (5 << 22), 0, 0]);
        words.extend_from_slice(&values);
    };
    load(words, CP_LOAD_STATE6_GEOM, 8);
    load(words, CP_LOAD_STATE6_FRAG, 12);
}

/// One quad, its vertices at `vertices`.
pub fn draw(words: &mut Vec<u32>, vertices: u64) {
    reg(words, 0xa00e, &[0, 0]); // VFD_INDEX_OFFSET
    addr(words, 0xa010, vertices); // VFD_VERTEX_BUFFER_BASE
    reg(words, 0xa012, &[VERTEX_BYTES, 24]); // VFD_VERTEX_BUFFER_SIZE, _STRIDE
    // TRIANGLES | AUTO_INDEX, one instance, six vertices.
    pkt(words, CP_DRAW_INDX_OFFSET, &[0x84, 1, 6]);
}

/// The next quads' colour matrix, for a [`Pass`] with `yuv`: `rows` are three rows of four IEEE 754 singles, each colour channel being
/// `row[0] * Y + row[1] * Cb + row[2] * Cr + row[3]`, everything from 0 to 1. The vertex shader keeps the identity matrix.
pub fn matrix(words: &mut Vec<u32>, rows: &[u32; 12]) {
    tint(words, !0);
    let mut values = [0u32; 16];
    values[..12].copy_from_slice(rows);
    values[12] = ONE;
    words.push(pm4::pkt7(CP_LOAD_STATE6_FRAG, 3 + values.len() as u32));
    // From c4, four of them.
    words.extend_from_slice(&[4 | (1 << 14) | (12 << 18) | (4 << 22), 0, 0]);
    words.extend_from_slice(&values);
}

/// The texture state of a picture in YUV at `address` (the start of the buffer it is in), laid out as `layout` says, which
/// [`crate::yuv_rules::fits`] has accepted. Chroma is sited as MPEG has it for 4:2:0: with the first luma column, between two rows.
pub fn yuv_texture_state(address: u64, layout: &crate::yuv_rules::Layout, filter: bool) -> [u32; 20] {
    use crate::yuv_rules::Planar;
    // `FMT6_R8_G8B8_2PLANE_420_UNORM` and `FMT6_R8_G8_B8_3PLANE_420_UNORM`: Mesa maps NV12 and I420 to these with swap `WZYX` and
    // takes (Y, Cb, Cr) from x, y, z (`fd6_format_table.c`, `fdl6_format_swiz`).
    let format = match layout.planar {
        Planar::Two => 0x8e,
        Planar::Three => 0x90,
    };
    let (y, c, v) = (
        address + layout.y_offset,
        address + layout.c_offset,
        address + layout.v_offset,
    );
    let mut state = [0u32; 20];
    // Swizzle x, y, z, one; `CHROMA_MIDPOINT_Y`.
    state[0] = (format << 22) | (1 << 7) | (2 << 10) | (5 << 13) | (1 << 18);
    state[1] = layout.width | (layout.height << 15);
    state[2] = (layout.y_pitch << 7) | (1 << 29);
    state[3] = (layout.y_pitch * layout.height).div_ceil(4096);
    state[4] = y as u32;
    state[5] = (y >> 32) as u32 | (1 << 17);
    state[6] = layout.c_pitch << 8;
    state[7] = c as u32;
    state[8] = (c >> 32) as u32;
    if layout.planar == Planar::Three {
        state[9] = v as u32;
        state[10] = (v >> 32) as u32;
    }
    state[16..].copy_from_slice(if filter {
        &SAMPLER_LINEAR
    } else {
        &SAMPLER_NEAREST
    });
    state
}

/// The shaders padded to their alignment, for the shader memory.
pub fn shader_memory() -> [u32; (3 * SHADER_BYTES / 4) as usize] {
    let mut memory = [0u32; (3 * SHADER_BYTES / 4) as usize];
    memory[..VERTEX_SHADER.len()].copy_from_slice(&VERTEX_SHADER);
    let at = (FRAGMENT_OFFSET / 4) as usize;
    memory[at..at + FRAGMENT_SHADER.len()].copy_from_slice(&FRAGMENT_SHADER);
    let at = (YUV_FRAGMENT_OFFSET / 4) as usize;
    memory[at..at + YUV_FRAGMENT_SHADER.len()].copy_from_slice(&YUV_FRAGMENT_SHADER);
    memory
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::vec;

    use super::*;

    #[test]
    fn ratio_is_what_the_fpu_says() {
        for (n, d) in [
            (1, 1),
            (1, 2),
            (3, 2),
            (-1, 1),
            (-3, 4),
            (1, 255),
            (254, 255),
            (960, 1),
            (2, 1920),
            (1919, 1920),
            (65534 - 1920, 1920),
            (7, 1080),
            (1079, 1080),
        ] {
            let exact = n as f64 / d as f64;
            let bits = ratio(n, d);
            let got = f32::from_bits(bits) as f64;
            // Rounded down in magnitude: within one unit in the last place.
            assert!(
                (got - exact).abs() <= exact.abs() * 2f64.powi(-23),
                "{n}/{d}: {got} vs {exact}"
            );
            assert!(got.abs() <= exact.abs(), "{n}/{d}: {got} vs {exact}");
        }
        assert_eq!(ratio(1, 1), 1.0f32.to_bits());
        assert_eq!(ratio(1, 2), 0.5f32.to_bits());
        assert_eq!(ratio(-3, 2), (-1.5f32).to_bits());
        assert_eq!(ratio(0, 7), 0);
    }

    /// The test values of Scarlet's `guardband_matches_freedreno_3_6_encoding`, for a viewport from 0.
    #[test]
    fn guardband_matches_the_float_algorithm() {
        for size in [640u32, 1080, 1920, 2560, 16384] {
            let scale = size as f32 * 0.5;
            let adjustment = (32_767.0 - scale) / scale;
            let bits = adjustment.to_bits();
            let exponent = ((bits >> 23) & 0xff) as i32 - 127;
            let expected = ((exponent as u32) << 6) | ((bits >> 17) & 0x3f);
            assert_eq!(guardband(size), expected, "{size}");
        }
    }

    const SCREEN: Surface = Surface {
        address: 0x9bc0_0000,
        pitch: 7680,
        width: 1920,
        height: 1080,
    };

    #[test]
    fn a_quad_over_the_whole_target_reaches_its_corners() {
        let all = Rect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
        };
        let v = vertices(&SCREEN, &all, &SCREEN, &all);
        // Top left: clip (-1, +1), texture (0, 0).
        assert_eq!(
            &v[..6],
            &[(-1.0f32).to_bits(), 1.0f32.to_bits(), 0, ONE, 0, 0]
        );
        // Bottom right, the last vertex: clip (+1, -1), texture (1, 1).
        assert_eq!(
            &v[30..],
            &[1.0f32.to_bits(), (-1.0f32).to_bits(), 0, ONE, ONE, ONE]
        );
    }

    #[test]
    fn the_descriptor_has_scarlets_layout() {
        let surface = Surface {
            address: 0x1_2345_6000,
            pitch: 1024,
            width: 256,
            height: 128,
        };
        let state = texture_state(&surface, true);
        assert_eq!(state[1], 256 | (128 << 15));
        assert_eq!(state[2], (1024 << 7) | (1 << 29));
        assert_eq!(state[3], 32);
        assert_eq!(state[4], 0x2345_6000);
        assert_eq!(state[5], 1 | (1 << 17));
        assert_eq!(&state[16..], &SAMPLER_LINEAR);
        assert_eq!(state.len() as u32 * 4, TEXTURE_STATE_BYTES);
    }

    #[test]
    fn a_pass_says_where_the_pixels_go_and_how_they_blend() {
        let mut words = vec![];
        let p = Pass {
            dst: SCREEN,
            blend: true,
            premultiplied: false,
            shaders: 0x2000,
            yuv: false,
        };
        pass(&mut words, &p).unwrap();
        assert!(
            words
                .windows(3)
                .any(|w| w[0] == pm4::pkt4(0x8825, 2) && w[1] == 0x9bc0_0000 && w[2] == 0)
        );
        assert!(
            words
                .windows(3)
                .any(|w| w[0] == pm4::pkt4(0x8820, 2) && w[1] == 0x7e3 && w[2] == 0x0701_0706)
        );
        let mut straight = vec![];
        pass(&mut straight, &Pass { blend: false, ..p }).unwrap();
        assert!(
            straight
                .windows(3)
                .any(|w| w[0] == pm4::pkt4(0x8820, 2) && w[1] == 0x7e0)
        );
        let mut premultiplied = vec![];
        pass(
            &mut premultiplied,
            &Pass {
                premultiplied: true,
                ..p
            },
        )
        .unwrap();
        assert!(
            premultiplied
                .windows(3)
                .any(|w| w[0] == pm4::pkt4(0x8820, 2) && w[1] == 0x7e3 && w[2] == 0x0701_0701)
        );
        // Every packet is framed: walking the headers ends exactly at the end.
        let mut at = 0;
        while at < words.len() {
            let header = words[at];
            at += 1 + match header >> 28 {
                4 => (header & 0x7f) as usize,
                7 => (header & 0x3fff) as usize,
                other => panic!("packet type {other}"),
            };
        }
        assert_eq!(at, words.len());
    }

    #[test]
    fn the_shader_words_are_the_packs() {
        let memory = shader_memory();
        assert_eq!(memory[0], 0x0000_1000);
        assert_eq!(memory[32], 0x0000_2000);
        // `end` of each, the same word.
        assert_eq!(memory[9], 0x0300_0000);
        assert_eq!(memory[32 + 5], 0x0300_0000);
        assert_eq!(memory[64], 0x0000_2000);
        assert_eq!(memory[64 + 25], 0x0300_0000);
    }

    #[test]
    fn a_yuv_picture_names_its_planes() {
        use crate::yuv_rules::{Layout, Planar};
        let nv12 = Layout {
            planar: Planar::Two,
            width: 1920,
            height: 1080,
            y_offset: 0,
            y_pitch: 1920,
            c_offset: 1920 * 1080,
            c_pitch: 1920,
            v_offset: 0,
        };
        let state = yuv_texture_state(0x1_0000_0000, &nv12, true);
        assert_eq!((state[0] >> 22) & 0xff, 0x8e);
        assert_eq!(state[0] >> 30, 0, "swap WZYX");
        assert_eq!(state[1], 1920 | (1080 << 15));
        assert_eq!(state[2], (1920 << 7) | (1 << 29));
        assert_eq!((state[4], state[5]), (0, 1 | (1 << 17)));
        assert_eq!(state[6], 1920 << 8);
        assert_eq!((state[7], state[8]), (1920 * 1080, 1));
        assert_eq!((state[9], state[10]), (0, 0));
        let i420 = Layout {
            planar: Planar::Three,
            c_pitch: 960,
            v_offset: 1920 * 1080 + 960 * 540,
            ..nv12
        };
        let state = yuv_texture_state(0x8000_0000, &i420, false);
        assert_eq!((state[0] >> 22) & 0xff, 0x90);
        assert_eq!(state[9], 0x8000_0000 + 1920 * 1080 + 960 * 540);
        assert_eq!(&state[16..], &SAMPLER_NEAREST);
    }

    #[test]
    fn a_matrix_reaches_c4_to_c7_of_the_fragment_shader() {
        let mut words = vec![];
        let rows = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
        matrix(&mut words, &rows);
        let at = words
            .iter()
            .position(|w| *w == 4 | (1 << 14) | (12 << 18) | (4 << 22))
            .unwrap();
        assert_eq!(words[at - 1], pm4::pkt7(CP_LOAD_STATE6_FRAG, 19));
        assert_eq!(&words[at + 3..at + 15], &rows);
        assert_eq!(&words[at + 15..at + 19], &[ONE, 0, 0, 0]);
    }
}
