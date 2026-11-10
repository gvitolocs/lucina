// SPDX-License-Identifier: GPL-2.0-only
//! What userspace may ask of the GPU through `/dev/lucina-gpu`: lines of text, never command streams, so that a program cannot make
//! the GPU touch memory other than the screen and the kernel's offscreen surface. One write is one batch; it returns when the
//! pixels are in memory.
//!
//! A surface is `screen`, `off` or `b<id>`, a buffer of this open file that the program paints itself and the GPU only reads:
//! `buffer <w> <h>` makes one and a read of the device says its id and where to `mmap` it, `free b<id>` ends it, and
//! `clean b<id> <y> <h>` hands rows the CPU has changed to the GPU, within a batch and before what reads them. Rows not cleaned may
//! reach the GPU stale: the memory is cached for the CPU, which is why painting in it is fast.
//!
//! ```text
//! fill <surface> <x> <y> <w> <h> <aarrggbb>
//! blit <surface> <x> <y> <w> <h> <surface> <x> <y> <w> <h> [filter]
//! move <x> <y> <w> <h> <to x> <to y>
//! quad <surface> <x> <y> <w> <h> <surface> <x> <y> <w> <h> <tint aarrggbb> [blend] [premul] [filter]
//! yuv b<id> <nv12|i420> <w> <h> <y offset> <y line> <c offset> <c line> <v offset> <x> <y> <w> <h> <surface> <x> <y> <w> <h>
//!     <12 x IEEE 754 single, hex> [filter] [a2d]
//! vblank <bottom>
//! ```
//!
//! `vblank` makes the kernel hold the batch, before it takes the GPU and for at most a frame, until the panel's scan is below screen
//! row `bottom` or in the blanking before row 0: the batch then draws behind the scan, which a picture drawn top to bottom stays ahead
//! of. A kernel that offers it says `vblank yes` in its status.
//!
//! `yuv` draws a decoded picture that a buffer holds as 4:2:0 planes, at byte offsets and lines from the buffer's start
//! ([`crate::yuv_rules`] says where they may lie), through [`crate::quad::YUV_FRAGMENT_SHADER`]: each colour channel is
//! `m[0] * Y + m[1] * Cb + m[2] * Cr + m[3]`, red, green and blue in turn, everything from 0 to 1. The program picks the matrix, so
//! BT.601, BT.709, full or video range are its business. `a2d` tries the 2D engine instead, which is a probe: see
//! [`a2d::blit_yuv`].
//!
//! `move` works on the screen and may overlap: it goes through `off`, whose same rectangle it
//! overwrites. `quad` draws through the 3D pipeline: the source times the tint, blended over the target with the result's alpha if
//! `blend`, or as colour already multiplied by alpha if `premul`, which implies `blend` (a tint then multiplies colour too: for
//! opacity `a`, give `aaaaaaaa`). Everything else goes through A2D, which is cheaper.

use alloc::vec::Vec;

use crate::{
    Result,
    a2d::{self, Rect, Surface},
    err, quad,
    yuv_rules::{self, Layout, Planar},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Screen,
    Off,
    Buffer(u16),
}

/// How many buffers there can be, on the whole machine.
pub const MAX_BUFFERS: u32 = 256;
/// How many texture states there are: the two surfaces and the buffers, each with and without a filter.
pub const TEXTURE_STATES: u32 = (2 + MAX_BUFFERS) * 2;
/// How many `yuv` commands a batch may hold: each needs its own texture state, at [`Env::yuv_states`].
pub const YUV_PER_BATCH: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Fill {
        target: Target,
        rect: Rect,
        colour: u32,
    },
    Blit {
        src: Target,
        from: Rect,
        dst: Target,
        to: Rect,
        filter: bool,
    },
    Move {
        from: Rect,
        x: u32,
        y: u32,
    },
    /// Rows of a buffer that the CPU changed. The kernel acts on it; it makes no GPU commands.
    Clean {
        buffer: u16,
        y: u32,
        h: u32,
    },
    Quad {
        src: Target,
        from: Rect,
        dst: Target,
        to: Rect,
        tint: u32,
        blend: bool,
        premultiplied: bool,
        filter: bool,
    },
    /// Wait for the scan: the kernel acts on it; it makes no GPU commands.
    Vblank {
        bottom: u32,
    },
    Yuv {
        buffer: u16,
        layout: Layout,
        from: Rect,
        dst: Target,
        to: Rect,
        matrix: [u32; 12],
        filter: bool,
        a2d: bool,
    },
}

fn target(word: &str) -> Option<Target> {
    match word {
        "screen" => Some(Target::Screen),
        "off" => Some(Target::Off),
        _ => {
            let id: u16 = word.strip_prefix('b')?.parse().ok()?;
            (u32::from(id) < MAX_BUFFERS).then_some(Target::Buffer(id))
        }
    }
}

/// `b<id>`.
pub fn buffer_id(word: &str) -> Option<u16> {
    match target(word)? {
        Target::Buffer(id) => Some(id),
        _ => None,
    }
}

fn rect<'a>(words: &mut impl Iterator<Item = &'a str>) -> Option<Rect> {
    let mut next = || words.next()?.parse::<u32>().ok();
    Some(Rect {
        x: next()?,
        y: next()?,
        w: next()?,
        h: next()?,
    })
}

/// One line, or `None` if it is not a command. Sizes are checked by [`encode`].
pub fn parse(line: &str) -> Option<Command> {
    let mut words = line.split_ascii_whitespace();
    let command = match words.next()? {
        "fill" => {
            let target = target(words.next()?)?;
            let rect = rect(&mut words)?;
            let colour = u32::from_str_radix(words.next()?, 16).ok()?;
            Command::Fill {
                target,
                rect,
                colour,
            }
        }
        "blit" => {
            let src = target(words.next()?)?;
            let from = rect(&mut words)?;
            let dst = target(words.next()?)?;
            let to = rect(&mut words)?;
            let filter = match words.next() {
                None => false,
                Some("filter") => true,
                Some(_) => return None,
            };
            Command::Blit {
                src,
                from,
                dst,
                to,
                filter,
            }
        }
        "move" => {
            let from = rect(&mut words)?;
            let x = words.next()?.parse().ok()?;
            let y = words.next()?.parse().ok()?;
            Command::Move { from, x, y }
        }
        "clean" => {
            let buffer = buffer_id(words.next()?)?;
            let y = words.next()?.parse().ok()?;
            let h = words.next()?.parse().ok()?;
            Command::Clean { buffer, y, h }
        }
        "quad" => {
            let src = target(words.next()?)?;
            let from = rect(&mut words)?;
            let dst = target(words.next()?)?;
            let to = rect(&mut words)?;
            let tint = u32::from_str_radix(words.next()?, 16).ok()?;
            let (mut blend, mut premultiplied, mut filter) = (false, false, false);
            for word in words.by_ref() {
                match word {
                    "blend" => blend = true,
                    "premul" => {
                        blend = true;
                        premultiplied = true;
                    }
                    "filter" => filter = true,
                    _ => return None,
                }
            }
            Command::Quad {
                src,
                from,
                dst,
                to,
                tint,
                blend,
                premultiplied,
                filter,
            }
        }
        "vblank" => Command::Vblank {
            bottom: words.next()?.parse().ok()?,
        },
        "yuv" => {
            let buffer = buffer_id(words.next()?)?;
            let planar = match words.next()? {
                "nv12" => Planar::Two,
                "i420" => Planar::Three,
                _ => return None,
            };
            let mut number = || words.next()?.parse::<u64>().ok();
            let (width, height) = (number()?, number()?);
            let (y_offset, y_pitch, c_offset, c_pitch, v_offset) =
                (number()?, number()?, number()?, number()?, number()?);
            let layout = Layout {
                planar,
                width: u32::try_from(width).ok()?,
                height: u32::try_from(height).ok()?,
                y_offset,
                y_pitch: u32::try_from(y_pitch).ok()?,
                c_offset,
                c_pitch: u32::try_from(c_pitch).ok()?,
                v_offset,
            };
            let from = rect(&mut words)?;
            let dst = target(words.next()?)?;
            let to = rect(&mut words)?;
            let mut matrix = [0u32; 12];
            for value in matrix.iter_mut() {
                *value = u32::from_str_radix(words.next()?, 16).ok()?;
            }
            let (mut filter, mut a2d) = (false, false);
            for word in words.by_ref() {
                match word {
                    "filter" => filter = true,
                    "a2d" => a2d = true,
                    _ => return None,
                }
            }
            Command::Yuv {
                buffer,
                layout,
                from,
                dst,
                to,
                matrix,
                filter,
                a2d,
            }
        }
        _ => return None,
    };
    if words.next().is_some() {
        return None;
    }
    Some(command)
}

/// Every line of a batch, or `None` if any is not a command. At most `limit` of them.
pub fn parse_batch(text: &str, limit: usize) -> Option<Vec<Command>> {
    let mut commands = Vec::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        if commands.len() == limit {
            return None;
        }
        commands.push(parse(line)?);
    }
    (!commands.is_empty()).then_some(commands)
}

/// What a batch may draw with: the two surfaces, and the kernel's memory for the 3D pipeline.
#[derive(Clone, Copy, Debug)]
pub struct Env {
    pub screen: Surface,
    pub off: Surface,
    /// 8 bytes of memory for the caches' timestamps.
    pub scratch: u64,
    /// [`quad::shader_memory`].
    pub shaders: u64,
    /// [`quad::TEXTURE_STATE_BYTES`] apart, in the order of [`Env::texture_slot`], holding [`quad::texture_state`].
    pub texture_states: u64,
    /// [`quad::VERTEX_BYTES`] for each quad of a batch.
    pub vertices: u64,
    /// [`quad::TEXTURE_STATE_BYTES`] for each of [`YUV_PER_BATCH`], written from what [`encode`] puts in `yuv_states`.
    pub yuv_states: u64,
}

impl Env {
    /// Which texture state serves a source and a filter.
    pub const fn texture_slot(src: Target, filter: bool) -> u32 {
        let source = match src {
            Target::Screen => 0,
            Target::Off => 1,
            Target::Buffer(id) => 2 + id as u32,
        };
        source * 2 + filter as u32
    }
}

/// The command stream of a batch, the vertices that go at [`Env::vertices`] and the texture states that go at [`Env::yuv_states`].
/// The caller adds the last flush and a fence.
///
/// `buffers` says what a buffer of the program that sent this batch is, and `None` for any other number: a program reaches no
/// other program's memory. A buffer is only ever a source.
pub fn encode(
    commands: &[Command],
    env: &Env,
    buffers: &dyn Fn(u16) -> Option<Surface>,
    words: &mut Vec<u32>,
    vertices: &mut Vec<u32>,
    yuv_states: &mut Vec<u32>,
) -> Result<()> {
    let (screen, off, scratch) = (&env.screen, &env.off, env.scratch);
    let source = |target: Target| -> Result<Surface> {
        match target {
            Target::Screen => Ok(*screen),
            Target::Off => Ok(*off),
            Target::Buffer(id) => match buffers(id) {
                Some(surface) => Ok(surface),
                None => err("no such buffer in this program", u32::from(id)),
            },
        }
    };
    let destination = |target: Target| -> Result<Surface> {
        if let Target::Buffer(id) = target {
            return err("a buffer is only read by the GPU", u32::from(id));
        }
        source(target)
    };
    a2d::setup(words);
    // The pass of the 3D pipeline set up by the last command, if it was a quad.
    let mut current: Option<quad::Pass> = None;
    let (mut current_texture, mut current_tint) = (None, None);
    for command in commands {
        if matches!(command, Command::Clean { .. } | Command::Vblank { .. }) {
            continue;
        }
        let three_d = matches!(command, Command::Quad { .. })
            || matches!(command, Command::Yuv { a2d: false, .. });
        if !three_d && current.take().is_some() {
            a2d::flush(words, scratch);
            a2d::setup(words);
        }
        match *command {
            Command::Clean { .. } | Command::Vblank { .. } => {}
            Command::Fill {
                target,
                rect,
                colour,
            } => a2d::fill(words, &destination(target)?, &rect, colour)?,
            Command::Blit {
                src,
                from,
                dst,
                to,
                filter,
            } => {
                if src == dst {
                    return err(
                        "a blit within a surface: the GPU reads and writes in its own order",
                        from.x,
                    );
                }
                a2d::blit(words, &source(src)?, &from, &destination(dst)?, &to, filter)?;
                a2d::flush(words, scratch);
            }
            Command::Move { from, x, y } => {
                a2d::blit(words, screen, &from, off, &from, false)?;
                a2d::flush(words, scratch);
                a2d::blit(
                    words,
                    off,
                    &from,
                    screen,
                    &Rect {
                        x,
                        y,
                        w: from.w,
                        h: from.h,
                    },
                    false,
                )?;
                a2d::flush(words, scratch);
            }
            Command::Quad {
                src,
                from,
                dst,
                to,
                tint,
                blend,
                premultiplied,
                filter,
            } => {
                if src == dst {
                    return err(
                        "a quad within a surface: the GPU reads and writes in its own order",
                        from.x,
                    );
                }
                let (from_surface, target) = (source(src)?, destination(dst)?);
                from_surface.holds(&from)?;
                target.holds(&to)?;
                let pass = quad::Pass {
                    dst: target,
                    blend,
                    premultiplied,
                    shaders: env.shaders,
                    yuv: false,
                };
                if current != Some(pass) {
                    // What was drawn may be what is read next.
                    a2d::flush(words, scratch);
                    if current.is_none() {
                        quad::baseline(words);
                    }
                    quad::pass(words, &pass)?;
                    current = Some(pass);
                    current_texture = None;
                    current_tint = None;
                }
                let texture = env.texture_states
                    + u64::from(Env::texture_slot(src, filter) * quad::TEXTURE_STATE_BYTES);
                if current_texture != Some(texture) {
                    quad::texture(words, texture);
                    current_texture = Some(texture);
                }
                if current_tint != Some(tint) {
                    quad::tint(words, tint);
                    current_tint = Some(tint);
                }
                let at =
                    env.vertices + (vertices.len() as u64 / 36) * u64::from(quad::VERTEX_BYTES);
                vertices.extend_from_slice(&quad::vertices(&from_surface, &from, &target, &to));
                quad::draw(words, at);
            }
            Command::Yuv {
                buffer,
                layout,
                from,
                dst,
                to,
                matrix,
                filter,
                a2d,
            } => {
                let Some(memory) = buffers(buffer) else {
                    return err("no such buffer in this program", u32::from(buffer));
                };
                let len = u64::from(memory.pitch) * u64::from(memory.height);
                if !yuv_rules::fits(&layout, len) {
                    return err("yuv: planes outside the buffer, or not 64-byte aligned", u32::from(buffer));
                }
                let inside = |at: u32, size: u32, limit: u32| {
                    size != 0 && at.checked_add(size).is_some_and(|end| end <= limit)
                };
                if !inside(from.x, from.w, layout.width) || !inside(from.y, from.h, layout.height) {
                    return err("yuv: a rectangle outside its picture", from.x);
                }
                let target = destination(dst)?;
                target.holds(&to)?;
                if a2d {
                    a2d::blit_yuv(words, memory.address, &layout, &from, &target, &to, filter)?;
                    a2d::flush(words, scratch);
                    continue;
                }
                let index = yuv_states.len() / (quad::TEXTURE_STATE_BYTES as usize / 4);
                if index >= YUV_PER_BATCH {
                    return err("yuv: too many pictures in one batch", index as u32);
                }
                yuv_states.extend_from_slice(&quad::yuv_texture_state(memory.address, &layout, filter));
                let pass = quad::Pass {
                    dst: target,
                    blend: false,
                    premultiplied: false,
                    shaders: env.shaders,
                    yuv: true,
                };
                if current != Some(pass) {
                    a2d::flush(words, scratch);
                    if current.is_none() {
                        quad::baseline(words);
                    }
                    quad::pass(words, &pass)?;
                    current = Some(pass);
                }
                quad::texture(words, env.yuv_states + index as u64 * u64::from(quad::TEXTURE_STATE_BYTES));
                quad::matrix(words, &matrix);
                // What the next quad finds loaded is not its own.
                current_texture = None;
                current_tint = None;
                // The vertices only need the picture's size for its texture coordinates.
                let picture = Surface {
                    address: 0,
                    pitch: 0,
                    width: layout.width,
                    height: layout.height,
                };
                let at =
                    env.vertices + (vertices.len() as u64 / 36) * u64::from(quad::VERTEX_BYTES);
                vertices.extend_from_slice(&quad::vertices(&picture, &from, &target, &to));
                quad::draw(words, at);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    const SCREEN: Surface = Surface {
        address: 0x9bc0_0000,
        pitch: 7680,
        width: 1920,
        height: 1080,
    };
    const OFF: Surface = Surface {
        address: 0x1_6700_0000,
        pitch: 7680,
        width: 1920,
        height: 1080,
    };

    const ENV: Env = Env {
        screen: SCREEN,
        off: OFF,
        scratch: 0x1000,
        shaders: 0x2000,
        texture_states: 0x3000,
        vertices: 0x4000,
        yuv_states: 0x5000,
    };

    #[test]
    fn quads_parse_and_their_vertices_come_out_in_order() {
        let line = "quad off 0 0 10 10 screen 5 5 20 20 80ffffff blend filter";
        let command = parse(line).unwrap();
        assert_eq!(
            command,
            Command::Quad {
                src: Target::Off,
                from: Rect {
                    x: 0,
                    y: 0,
                    w: 10,
                    h: 10
                },
                dst: Target::Screen,
                to: Rect {
                    x: 5,
                    y: 5,
                    w: 20,
                    h: 20
                },
                tint: 0x80ff_ffff,
                blend: true,
                premultiplied: false,
                filter: true
            }
        );
        assert_eq!(parse("quad off 0 0 10 10 screen 5 5 20 20 ff shiny"), None);
        // `premul` implies blending, and a change of it is a new pass.
        let premul = parse("quad off 0 0 10 10 screen 5 5 20 20 ffffffff premul").unwrap();
        assert!(matches!(
            premul,
            Command::Quad {
                blend: true,
                premultiplied: true,
                filter: false,
                ..
            }
        ));
        let (mut two, mut none) = (Vec::new(), Vec::new());
        encode(&[command, premul], &ENV, &|_| None, &mut two, &mut none, &mut Vec::new()).unwrap();
        assert_eq!(
            two.windows(2)
                .filter(|w| w[0] == crate::pm4::pkt4(0x8825, 2))
                .count(),
            2
        );
        let (mut words, mut vertices) = (Vec::new(), Vec::new());
        let fill = Command::Fill {
            target: Target::Off,
            rect: Rect {
                x: 0,
                y: 0,
                w: 1,
                h: 1,
            },
            colour: 0,
        };
        encode(
            &[command, command, fill, command],
            &ENV,
            &|_| None,
            &mut words,
            &mut vertices,
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(vertices.len(), 3 * 36);
        // The second quad of a pass does not repeat its state, and draws from the next vertices.
        let bases: std::vec::Vec<_> = words
            .windows(2)
            .filter(|w| w[0] == crate::pm4::pkt4(0xa010, 2))
            .map(|w| w[1])
            .collect();
        assert_eq!(
            bases,
            [
                0x4000,
                0x4000 + quad::VERTEX_BYTES,
                0x4000 + 2 * quad::VERTEX_BYTES
            ]
        );
        let passes = words
            .windows(2)
            .filter(|w| w[0] == crate::pm4::pkt4(0x8825, 2))
            .count();
        assert_eq!(passes, 2);
    }

    #[test]
    fn a_quad_outside_its_surfaces_or_within_one_is_refused() {
        let r = Rect {
            x: 0,
            y: 0,
            w: 1,
            h: 1,
        };
        let quad = |src, from, dst, to| Command::Quad {
            src,
            from,
            dst,
            to,
            tint: 0,
            blend: false,
            premultiplied: false,
            filter: false,
        };
        let (mut words, mut vertices) = (Vec::new(), Vec::new());
        assert!(
            encode(
                &[quad(Target::Off, r, Target::Off, r)],
                &ENV,
                &|_| None,
                &mut words,
                &mut vertices,
                &mut Vec::new()
            )
            .is_err()
        );
        assert!(
            encode(
                &[quad(Target::Off, Rect { x: 1920, ..r }, Target::Screen, r)],
                &ENV,
                &|_| None,
                &mut words,
                &mut vertices,
                &mut Vec::new()
            )
            .is_err()
        );
        assert!(
            encode(
                &[quad(Target::Off, r, Target::Screen, Rect { y: 1080, ..r })],
                &ENV,
                &|_| None,
                &mut words,
                &mut vertices,
                &mut Vec::new()
            )
            .is_err()
        );
    }

    #[test]
    fn a_program_reaches_only_its_own_buffers_and_only_reads_them() {
        let tile = Surface {
            address: 0x1_0000_0000,
            pitch: 1024,
            width: 256,
            height: 256,
        };
        let mine = |id: u16| (id == 3).then_some(tile);
        let r = Rect {
            x: 0,
            y: 0,
            w: 256,
            h: 256,
        };
        let quad = |src, dst| Command::Quad {
            src,
            from: r,
            dst,
            to: r,
            tint: !0,
            blend: true,
            premultiplied: false,
            filter: false,
        };
        let (mut words, mut vertices) = (Vec::new(), Vec::new());
        encode(
            &[quad(Target::Buffer(3), Target::Screen)],
            &ENV,
            &mine,
            &mut words,
            &mut vertices,
            &mut Vec::new(),
        )
        .unwrap();
        assert!(
            words.contains(&pm4_texture_low(3, false)),
            "the pass names buffer 3's texture state"
        );
        assert_eq!(f32::from_bits(vertices[0]), -1.0);
        assert!(
            encode(
                &[quad(Target::Buffer(4), Target::Screen)],
                &ENV,
                &mine,
                &mut words,
                &mut vertices,
                &mut Vec::new()
            )
            .is_err()
        );
        assert!(
            encode(
                &[quad(Target::Off, Target::Buffer(3))],
                &ENV,
                &mine,
                &mut words,
                &mut vertices,
                &mut Vec::new()
            )
            .is_err()
        );
        let fill = Command::Fill {
            target: Target::Buffer(3),
            rect: r,
            colour: 0,
        };
        assert!(encode(&[fill], &ENV, &mine, &mut words, &mut vertices, &mut Vec::new()).is_err());
        let out = Command::Quad {
            src: Target::Buffer(3),
            from: Rect { w: 257, ..r },
            dst: Target::Screen,
            to: r,
            tint: 0,
            blend: false,
            premultiplied: false,
            filter: false,
        };
        assert!(encode(&[out], &ENV, &mine, &mut words, &mut vertices, &mut Vec::new()).is_err());
    }

    fn pm4_texture_low(id: u16, filter: bool) -> u32 {
        ENV.texture_states as u32
            + Env::texture_slot(Target::Buffer(id), filter) * quad::TEXTURE_STATE_BYTES
    }

    #[test]
    fn clean_lines_parse_and_make_no_gpu_commands() {
        assert_eq!(
            parse("clean b7 10 20"),
            Some(Command::Clean {
                buffer: 7,
                y: 10,
                h: 20
            })
        );
        assert_eq!(parse("clean off 10 20"), None);
        assert_eq!(parse("clean b256 0 1"), None);
        assert_eq!(parse("fill b256 0 0 1 1 0"), None);
        let (mut with, mut without) = (Vec::new(), Vec::new());
        let fill = Command::Fill {
            target: Target::Off,
            rect: Rect {
                x: 0,
                y: 0,
                w: 1,
                h: 1,
            },
            colour: 0,
        };
        encode(
            &[
                Command::Clean {
                    buffer: 0,
                    y: 0,
                    h: 1,
                },
                fill,
            ],
            &ENV,
            &|_| None,
            &mut with,
            &mut Vec::new(),
            &mut Vec::new(),
        )
        .unwrap();
        encode(&[fill], &ENV, &|_| None, &mut without, &mut Vec::new(), &mut Vec::new()).unwrap();
        assert_eq!(with, without);
    }

    #[test]
    fn a_page_of_tiles_fits_the_ring() {
        let tile = Surface {
            address: 0x1_0000_0000,
            pitch: 1024,
            width: 256,
            height: 256,
        };
        let r = Rect {
            x: 0,
            y: 0,
            w: 256,
            h: 256,
        };
        // As many as a batch takes (the kernel's limit), each from another buffer.
        let page: std::vec::Vec<_> = (0..128u16)
            .map(|id| Command::Quad {
                src: Target::Buffer(id),
                from: r,
                dst: Target::Screen,
                to: r,
                tint: !0,
                blend: true,
                premultiplied: false,
                filter: true,
            })
            .collect();
        let (mut words, mut vertices) = (Vec::new(), Vec::new());
        encode(&page, &ENV, &|_| Some(tile), &mut words, &mut vertices, &mut Vec::new()).unwrap();
        assert!(
            words.len() < crate::cp::RING_SIZE / 4 - 128,
            "{} words",
            words.len()
        );
        // One setup of the pipeline, one change of source per tile.
        assert_eq!(
            words
                .windows(2)
                .filter(|w| w[0] == crate::pm4::pkt4(0x8825, 2))
                .count(),
            1
        );
        assert_eq!(
            words
                .windows(2)
                .filter(|w| w[0] == crate::pm4::pkt4(0xa9e4, 2))
                .count(),
            128
        );
    }

    #[test]
    fn lines_parse() {
        assert_eq!(
            parse("fill screen 1 2 3 4 ff102030"),
            Some(Command::Fill {
                target: Target::Screen,
                rect: Rect {
                    x: 1,
                    y: 2,
                    w: 3,
                    h: 4
                },
                colour: 0xff10_2030
            })
        );
        assert_eq!(
            parse("blit off 1 2 3 4 screen 5 6 7 8 filter"),
            Some(Command::Blit {
                src: Target::Off,
                from: Rect {
                    x: 1,
                    y: 2,
                    w: 3,
                    h: 4
                },
                dst: Target::Screen,
                to: Rect {
                    x: 5,
                    y: 6,
                    w: 7,
                    h: 8
                },
                filter: true
            })
        );
        assert_eq!(
            parse("move 0 20 1920 1060 0 0"),
            Some(Command::Move {
                from: Rect {
                    x: 0,
                    y: 20,
                    w: 1920,
                    h: 1060
                },
                x: 0,
                y: 0
            })
        );
    }

    #[test]
    fn anything_else_is_refused() {
        for line in [
            "",
            "fill",
            "fill screen 1 2 3",
            "fill disk 1 2 3 4 0",
            "fill screen 1 2 3 4 zz",
            "fill screen 1 2 3 4 0 extra",
            "move 1 2 3 4 5",
            "blit screen 1 2 3 4 off 1 2 3 4 smooth",
            "fill screen -1 2 3 4 0",
        ] {
            assert_eq!(parse(line), None, "{line}");
        }
        assert!(parse_batch("fill screen 1 2 3 4 0\nnonsense", 8).is_none());
        assert!(parse_batch("\n\n", 8).is_none());
        assert!(parse_batch("fill screen 1 2 3 4 0\nfill screen 1 2 3 4 0", 1).is_none());
    }

    #[test]
    fn rectangles_outside_the_surfaces_never_become_commands() {
        let bad = [
            Command::Fill {
                target: Target::Screen,
                rect: Rect {
                    x: 1919,
                    y: 0,
                    w: 2,
                    h: 1,
                },
                colour: 0,
            },
            Command::Move {
                from: Rect {
                    x: 0,
                    y: 0,
                    w: 10,
                    h: 10,
                },
                x: 1915,
                y: 0,
            },
            Command::Blit {
                src: Target::Screen,
                from: Rect {
                    x: 0,
                    y: 1080,
                    w: 1,
                    h: 1,
                },
                dst: Target::Off,
                to: Rect {
                    x: 0,
                    y: 0,
                    w: 1,
                    h: 1,
                },
                filter: false,
            },
        ];
        for command in bad {
            let mut words = Vec::new();
            assert!(
                encode(&[command], &ENV, &|_| None, &mut words, &mut Vec::new(), &mut Vec::new()).is_err(),
                "{command:?}"
            );
        }
    }

    #[test]
    fn a_blit_within_one_surface_is_refused() {
        let mut words = Vec::new();
        let r = Rect {
            x: 0,
            y: 0,
            w: 1,
            h: 1,
        };
        assert!(
            encode(
                &[Command::Blit {
                    src: Target::Off,
                    from: r,
                    dst: Target::Off,
                    to: r,
                    filter: false
                }],
                &ENV,
                &|_| None,
                &mut words,
                &mut Vec::new(),
                &mut Vec::new()
            )
            .is_err()
        );
    }

    #[test]
    fn a_move_goes_through_the_other_surface() {
        let mut words = Vec::new();
        encode(
            &[Command::Move {
                from: Rect {
                    x: 0,
                    y: 20,
                    w: 100,
                    h: 50,
                },
                x: 0,
                y: 0,
            }],
            &ENV,
            &|_| None,
            &mut words,
            &mut Vec::new(),
            &mut Vec::new(),
        )
        .unwrap();
        // Two blits, both destinations named: the offscreen surface, then the screen.
        let lows: std::vec::Vec<_> = words
            .windows(2)
            .filter(|w| w[0] == crate::pm4::pkt4(0x8c17, 4))
            .map(|_| ())
            .collect();
        assert_eq!(lows.len(), 2);
        assert!(words.contains(&(OFF.address as u32)));
        assert!(words.contains(&(SCREEN.address as u32)));
    }

    fn picture() -> Layout {
        Layout {
            planar: Planar::Three,
            width: 1280,
            height: 534,
            y_offset: 0,
            y_pitch: 1280,
            c_offset: 1280 * 534,
            c_pitch: 640,
            v_offset: 1280 * 534 + 640 * 267,
        }
    }

    const IDENTITY: &str = "3f800000 0 0 0 0 3f800000 0 0 0 0 3f800000 0";

    #[test]
    fn a_yuv_line_parses() {
        let line = std::format!("yuv b3 i420 1280 534 0 1280 683520 640 854400 0 0 1280 534 screen 0 273 1920 534 {IDENTITY} filter");
        let Some(Command::Yuv { buffer, layout, from, dst, to, matrix, filter, a2d }) = parse(&line) else {
            panic!("not parsed");
        };
        assert_eq!((buffer, layout, dst, filter, a2d), (3, picture(), Target::Screen, true, false));
        assert_eq!(from, Rect { x: 0, y: 0, w: 1280, h: 534 });
        assert_eq!(to, Rect { x: 0, y: 273, w: 1920, h: 534 });
        assert_eq!(matrix[0], 0x3f80_0000);
        assert!(parse(&line.replace("i420", "yuy2")).is_none());
        assert!(parse(&line.replace(" filter", " 0")).is_none(), "one value too many");
    }

    /// The buffer the picture is in: 1280 bytes a line, enough lines for the three planes.
    fn holder(id: u16) -> Option<Surface> {
        (id == 3).then_some(Surface { address: 0x2_0000_0000, pitch: 1280, width: 320, height: 801 })
    }

    fn yuv(layout: Layout, to: Rect) -> Command {
        Command::Yuv {
            buffer: 3,
            layout,
            from: Rect { x: 0, y: 0, w: layout.width, h: layout.height },
            dst: Target::Screen,
            to,
            matrix: [0x3f80_0000; 12],
            filter: true,
            a2d: false,
        }
    }

    #[test]
    fn a_yuv_picture_is_drawn_with_its_own_texture_state() {
        let to = Rect { x: 0, y: 273, w: 1920, h: 534 };
        let (mut words, mut vertices, mut states) = (Vec::new(), Vec::new(), Vec::new());
        encode(&[yuv(picture(), to), yuv(picture(), to)], &ENV, &holder, &mut words, &mut vertices, &mut states).unwrap();
        assert_eq!(states.len(), 2 * 20);
        assert_eq!(vertices.len(), 2 * 36);
        assert_eq!(states[4], 0);
        assert_eq!(states[5] & 0x1ffff, 2);
        // The second picture's state is the second slot.
        let second = ENV.yuv_states + 80;
        assert!(words.windows(2).any(|w| w[0] == second as u32 && w[1] == 0));
    }

    #[test]
    fn a_yuv_picture_never_reads_outside_its_buffer() {
        let to = Rect { x: 0, y: 0, w: 100, h: 100 };
        let (mut words, mut vertices, mut states) = (Vec::new(), Vec::new(), Vec::new());
        let tall = Layout { height: 540, ..picture() };
        assert!(encode(&[yuv(tall, to)], &ENV, &holder, &mut words, &mut vertices, &mut states).is_err());
        let odd = Layout { c_offset: 1280 * 534 + 32, ..picture() };
        assert!(encode(&[yuv(odd, to)], &ENV, &holder, &mut words, &mut vertices, &mut states).is_err());
        let mut elsewhere = yuv(picture(), to);
        if let Command::Yuv { buffer, .. } = &mut elsewhere {
            *buffer = 4;
        }
        assert!(encode(&[elsewhere], &ENV, &holder, &mut words, &mut vertices, &mut states).is_err());
        let mut wide = yuv(picture(), to);
        if let Command::Yuv { from, .. } = &mut wide {
            from.x = u32::MAX;
        }
        assert!(encode(&[wide], &ENV, &holder, &mut words, &mut vertices, &mut states).is_err());
        let five: Vec<Command> = (0..5).map(|_| yuv(picture(), to)).collect();
        assert!(encode(&five, &ENV, &holder, &mut words, &mut vertices, &mut states).is_err());
    }

    #[test]
    fn vblank_waits_and_draws_nothing() {
        assert_eq!(parse("vblank 940"), Some(Command::Vblank { bottom: 940 }));
        assert!(parse("vblank").is_none());
        assert!(parse("vblank 1 2").is_none());
        let (mut words, mut vertices) = (Vec::new(), Vec::new());
        encode(&[Command::Vblank { bottom: 1080 }], &ENV, &|_| None, &mut words, &mut vertices, &mut Vec::new()).unwrap();
        let mut setup_only = Vec::new();
        a2d::setup(&mut setup_only);
        assert_eq!(words, setup_only);
    }
}
