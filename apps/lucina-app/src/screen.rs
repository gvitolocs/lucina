//! The screen as apps and the shell use it: the framebuffer, and the GPU where the kernel offers it (`lucina.gpu=on`).
//!
//! All drawing still happens in a [`Frame`] on the CPU. With a GPU the frame's pixels live in a buffer the GPU can read, and large
//! changed rectangles reach the screen as a blit instead of the CPU writing slow write-combined memory. Small ones stay with the CPU,
//! which is quicker for them: a GPU batch has a fixed cost. With no GPU, or once it has failed, this is the plain framebuffer.
//!
//! `LUCINA_GPU_PRESENT=0` keeps a program on the CPU, to compare.

use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::fb::{Fb, Surface};
use crate::gfx::{Frame, Rect};
use crate::gpu::{self, BATCH_COMMANDS, Batch, Gpu};

/// From this many pixels the GPU is the quicker way. `lucina-gpu present` on the laptop at 430 MHz, CPU against GPU: 1920x1080 1.9 / 1.6 ms,
/// 960x540 1.1 / 0.53 ms, 640x360 435 / 294 us, 1920x200 313 / 311 us, 1920x100 146 / 164 us, 64x1080 325 / 506 us, 64x32 2.4 / 30 us.
const GPU_FROM_PIXELS: i64 = 200_000;

/// How long a program started before the GPU is ready keeps looking for it. The kernel brings it up beside the boot.
const LOOK_FOR: Duration = Duration::from_secs(60);
const LOOK_EVERY: Duration = Duration::from_secs(1);

/// Whether the GPU is the quicker way to show this rectangle.
pub fn worth_blitting(r: &Rect) -> bool {
    i64::from(r.w.max(0)) * i64::from(r.h.max(0)) >= GPU_FROM_PIXELS
}

/// Shows `rects`: the ones worth it through `blit` as one lot, the others through `copy`. If `blit` fails, `copy` does its rectangles
/// and the GPU is not asked again (`*alive` goes false). Returns how many went through the GPU.
fn present_each(
    rects: &[Rect],
    alive: &mut bool,
    mut blit: impl FnMut(&[Rect]) -> io::Result<()>,
    mut copy: impl FnMut(Rect),
) -> usize {
    let (big, small): (Vec<Rect>, Vec<Rect>) = rects
        .iter()
        .copied()
        .partition(|r| *alive && worth_blitting(r));
    for r in small {
        copy(r);
    }
    if big.is_empty() {
        return 0;
    }
    match blit(&big) {
        Ok(()) => big.len(),
        Err(e) => {
            eprintln!("lucina-app: the GPU failed, the CPU draws from now on: {e}");
            *alive = false;
            for r in big {
                copy(r);
            }
            0
        }
    }
}

/// The commands that hand `rects` of buffer `id` to the GPU and copy them to the same place on the screen, split into batches the
/// kernel takes. A rectangle's rows are cleaned in the batch that reads them.
/// With `vblank`, the first batch waits until the panel's scan is below the lowest changed row (or in the blanking), so that the
/// frame lands behind the scan instead of tearing across it.
fn batches(buffer: &gpu::Buffer, rects: &[Rect], vblank: bool) -> Vec<Batch> {
    let bottom = rects.iter().map(|r| (r.y + r.h).max(0) as u32).max().unwrap_or(0);
    rects
        .chunks(BATCH_COMMANDS / 2 - 1)
        .enumerate()
        .map(|(i, part)| {
            let mut batch = Batch::new();
            if vblank && i == 0 {
                batch.vblank(bottom);
            }
            for r in part {
                batch.clean(buffer, r.y as u32, r.h as u32);
                batch.blit(buffer, *r, *r, false);
            }
            batch
        })
        .collect()
}

pub struct Screen {
    pub fb: Fb,
    gpu: Option<Gpu>,
    /// Still looking for a GPU, until then. `None` once found, or if this program is not to use one.
    looking_until: Option<Instant>,
    last_look: Option<Instant>,
    /// How many rectangles of the last [`Surface::present_all`] the GPU showed, for traces.
    pub last_gpu_rects: usize,
    /// Presents so far, for the present report.
    frames: u64,
    /// Wanted: large changes wait for the panel's vertical blank (`set_vsync`). `vblank` is whether the kernel offers it, asked
    /// once (`None` until then).
    vsync: bool,
    vblank: Option<bool>,
}

impl Screen {
    pub fn new(fb: Fb) -> Screen {
        let wanted = std::env::var_os("LUCINA_GPU_PRESENT").is_none_or(|v| v != "0");
        // The GPU writes the start of the screen's memory in XRGB: anything else stays with the CPU.
        let able = fb.layout.is_xrgb8888() && fb.origin() == 0;
        Screen {
            fb,
            gpu: None,
            looking_until: (wanted && able).then(|| Instant::now() + LOOK_FOR),
            last_look: None,
            last_gpu_rects: 0,
            frames: 0,
            vsync: false,
            vblank: None,
        }
    }

    /// Let the GPU's copies wait for the panel's vertical blank, so that animation lands whole: the shell's home does this. Off by
    /// default: a program that is not animating gains nothing from waiting. Only where the kernel has the wait (`Gpu::has_vblank`).
    pub fn set_vsync(&mut self, on: bool) {
        self.vsync = on;
    }

    /// Whether frames wait for the vertical blank now.
    pub fn vsync(&mut self) -> bool {
        if !self.vsync {
            return false;
        }
        if self.vblank.is_none()
            && let Some(g) = self.gpu.as_mut()
        {
            self.vblank = Some(g.has_vblank());
        }
        self.vblank == Some(true)
    }

    /// Call before drawing a frame: when the GPU is there, the frame's pixels move into a buffer. Costs a clock read when not looking.
    pub fn prepare(&mut self, frame: &mut Frame) {
        let Some(until) = self.looking_until else {
            return;
        };
        let now = Instant::now();
        if now >= until {
            self.looking_until = None;
            return;
        }
        if self.last_look.is_some_and(|t| now - t < LOOK_EVERY) {
            return;
        }
        self.last_look = Some(now);
        if !Path::new(gpu::DEVICE).exists() {
            return;
        }
        // The device is there, so this is settled either way.
        self.looking_until = None;
        let Some(mut gpu) = Gpu::open() else {
            return;
        };
        match gpu.buffer(frame.w, frame.h) {
            Ok(buffer) => match frame.move_to_buffer(buffer) {
                Ok(()) => {
                    eprintln!("lucina-app: large changes reach the screen through the GPU");
                    self.gpu = Some(gpu);
                }
                Err(_) => eprintln!("lucina-app: a GPU buffer does not fit this frame"),
            },
            Err(e) => eprintln!("lucina-app: no GPU buffer for the frame: {e}"),
        }
    }

    /// Whether the GPU shows large changes now.
    pub fn uses_gpu(&self) -> bool {
        self.gpu.is_some()
    }

    pub fn present(&mut self, frame: &Frame, area: Rect) {
        self.present_all(frame, &[area]);
    }

    pub fn present_image(&mut self, x: i32, y: i32, w: usize, px: &[u32]) {
        self.fb.present_image(x, y, w, px);
    }
}

impl Surface for Screen {
    fn present(&mut self, frame: &Frame, area: Rect) {
        Screen::present(self, frame, area);
    }

    fn present_image(&mut self, x: i32, y: i32, w: usize, px: &[u32]) {
        self.fb.present_image(x, y, w, px);
    }

    fn present_all(&mut self, frame: &Frame, rects: &[Rect]) {
        let bounds = frame.bounds();
        let rects: Vec<Rect> = rects
            .iter()
            .map(|r| r.intersect(&bounds))
            .filter(|r| !r.is_empty())
            .collect();
        let vblank = self.vsync();
        let Screen { fb, gpu, .. } = self;
        let mut alive = gpu.is_some() && frame.buffer().is_some();
        let shown = present_each(
            &rects,
            &mut alive,
            |big| {
                let (Some(gpu), Some(buffer)) = (gpu.as_mut(), frame.buffer()) else {
                    return Err(io::Error::other("no GPU"));
                };
                batches(buffer, big, vblank).iter().try_for_each(|b| gpu.run(b))
            },
            |r| fb.present(frame, r),
        );
        if !alive {
            // The frame stays in the buffer's memory, which the CPU can still read.
            *gpu = None;
        }
        self.last_gpu_rects = shown;
        // The present report (docs/research/juno-desk.md, stage D): which rectangles are on the screen now, sent after the copy
        // has ended so that a reader sees whole frames. One cached check when nobody listens.
        if !rects.is_empty() {
            self.frames += 1;
            let report: Vec<[i32; 4]> = rects.iter().map(|r| [r.x, r.y, r.w, r.h]).collect();
            lucina_events::present(self.frames, &report, shown as u32);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHOLE: Rect = Rect {
        x: 0,
        y: 0,
        w: 1920,
        h: 1080,
    };

    #[test]
    fn the_gpu_takes_large_rectangles_and_the_cpu_small_ones() {
        assert!(worth_blitting(&WHOLE));
        assert!(worth_blitting(&Rect::new(0, 0, 640, 360)));
        // A band is not: its rows' hand-over and the batch outweigh what the blit saves.
        assert!(!worth_blitting(&Rect::new(0, 0, 1920, 100)));
        // A key press: one tile of the frame.
        assert!(!worth_blitting(&Rect::new(64, 32, 64, 32)));
        assert!(!worth_blitting(&Rect::new(0, 0, 200, 40)));
        // Tall and thin: handing over whole rows costs more than the copy.
        assert!(!worth_blitting(&Rect::new(0, 0, 64, 1080)));
        assert!(!worth_blitting(&Rect::new(0, 0, 0, 0)));
    }

    #[test]
    fn each_rectangle_is_shown_once_by_the_quicker_way() {
        let small = Rect::new(64, 32, 64, 32);
        let (mut blits, mut copies) = (Vec::new(), Vec::new());
        let mut alive = true;
        let shown = present_each(
            &[small, WHOLE, small],
            &mut alive,
            |big| {
                blits.extend_from_slice(big);
                Ok(())
            },
            |r| copies.push(r),
        );
        assert_eq!(shown, 1);
        assert_eq!(blits, [WHOLE]);
        assert_eq!(copies, [small, small]);
        assert!(alive);
    }

    #[test]
    fn a_failing_gpu_loses_no_rectangle_and_is_not_asked_again() {
        let small = Rect::new(64, 32, 64, 32);
        let (mut asked, mut copies) = (0, Vec::new());
        let mut alive = true;
        let shown = present_each(
            &[WHOLE, small],
            &mut alive,
            |_| {
                asked += 1;
                Err(io::Error::other("the fence never came"))
            },
            |r| copies.push(r),
        );
        assert_eq!(shown, 0);
        assert!(!alive);
        assert_eq!(asked, 1);
        assert_eq!(copies, [small, WHOLE]);
        copies.clear();
        let shown = present_each(
            &[WHOLE],
            &mut alive,
            |_| {
                asked += 1;
                Ok(())
            },
            |r| copies.push(r),
        );
        assert_eq!((shown, asked), (0, 1));
        assert_eq!(copies, [WHOLE]);
    }

    #[test]
    fn no_gpu_means_the_cpu_copies_everything() {
        let mut copies = Vec::new();
        let mut alive = false;
        let shown = present_each(
            &[WHOLE],
            &mut alive,
            |_| panic!("no GPU to ask"),
            |r| copies.push(r),
        );
        assert_eq!(shown, 0);
        assert_eq!(copies, [WHOLE]);
    }
}
