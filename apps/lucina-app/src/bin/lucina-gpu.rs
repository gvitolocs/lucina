//! `lucina-gpu`: tries the GPU from a shell.
//!
//! - `lucina-gpu status`
//! - `lucina-gpu <a line of the device's language>`
//! - `lucina-gpu demo <seconds>`: a pattern on the screen, drawn only by the GPU, with the console off while it lasts
//! - `lucina-gpu tiles <seconds>`: a page of tiles the program paints, moved, scaled and blended by the GPU with no copy; checks its
//!   pixels, prints what each part costs and holds the last picture
//! - `lucina-gpu bench`: the same fills and scrolls by the CPU, as `lucina-shell` does them, and by the GPU
//! - `lucina-gpu present`: what showing a changed rectangle of a frame costs each way, against the rule `lucina_app::screen` uses, and
//!   the whole path checked on the screen's memory
//! - `lucina-gpu yuv`: pictures in NV12 and I420 drawn through `yuv`, checked on the screen's memory with the identity matrix (which
//!   plane lands in which channel) and with BT.601, then what a video frame costs to copy in and to draw. `lucina-gpu yuv a2d` asks
//!   the 2D engine the same, last, since an engine that hangs ends the GPU until a restart
//! - `lucina-gpu power <cycles> [max_sleep_ms]`: suspend and resume under load. Each cycle sleeps 0..=max ms (default three
//!   idle periods), then draws NV12 and I420 pictures and a fill in one batch; every quadrant must equal what the first draw
//!   showed, bit for bit, and the fill its colour. Prints suspends, resumes and what a batch costs with and without a resume
//! - `lucina-gpu power-race <seconds> [threads]`: threads with their own device files draw and check small fills at random
//!   times around the idle period, so that batches race the idle timer

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

use std::time::{Duration, Instant};

use lucina_app::{
    fb::{Fb, GraphicsMode, Surface},
    gfx::{Frame, Rect, Rgb},
    gpu::{Batch, Buffer, Gpu, YuvLayout, yuv_matrix},
    screen::{Screen, worth_blitting},
};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(mut gpu) = Gpu::open() else {
        eprintln!(
            "lucina-gpu: no {}: this kernel needs lucina.gpu=on",
            lucina_app::gpu::DEVICE
        );
        std::process::exit(1);
    };
    let result = match args.first().map(String::as_str) {
        Some("status") | None => gpu.status().map(|text| print!("{text}")),
        Some("demo") => demo(
            &mut gpu,
            args.get(1).and_then(|s| s.parse().ok()).unwrap_or(30),
        ),
        Some("bench") => bench(&mut gpu),
        Some("present") => present(&mut gpu),
        Some("yuv") => yuv(&mut gpu, args.get(1).is_some_and(|a| a == "a2d")),
        Some("tiles") => tiles(
            &mut gpu,
            args.get(1).and_then(|s| s.parse().ok()).unwrap_or(10),
        ),
        Some("power") => power(
            &mut gpu,
            args.get(1).and_then(|s| s.parse().ok()).unwrap_or(200),
            args.get(2).and_then(|s| s.parse().ok()),
        ),
        Some("power-race") => power_race(
            &mut gpu,
            args.get(1).and_then(|s| s.parse().ok()).unwrap_or(30),
            args.get(2).and_then(|s| s.parse().ok()).unwrap_or(3),
        ),
        Some(_) => gpu.submit(&(args.join(" ") + "\n")),
    };
    if let Err(err) = result {
        eprintln!("lucina-gpu: {err}");
        std::process::exit(1);
    }
}

fn demo(gpu: &mut Gpu, seconds: u64) -> std::io::Result<()> {
    let _graphics = GraphicsMode::enter()?;
    let fb = Fb::open("/dev/fb0")?;
    let (w, h) = (fb.width as i32, fb.height as i32);
    let started = Instant::now();
    gpu.fill(Rect { x: 0, y: 0, w, h }, 0x101820)?;
    for i in 0..6 {
        let colour = [0xc03030, 0x30c030, 0x3030c0, 0xc0c030, 0x30c0c0, 0xc030c0][i as usize];
        gpu.fill(
            Rect {
                x: 60 + i * 300,
                y: 60,
                w: 280,
                h: 200,
            },
            colour,
        )?;
    }
    // A band of the row above, copied down with overlap, then a filtered enlargement through the kernel's offscreen surface.
    gpu.move_rect(
        Rect {
            x: 60,
            y: 60,
            w: 1780,
            h: 200,
        },
        90,
        150,
    )?;
    gpu.submit("blit screen 60 60 900 200 off 0 0 900 200\nblit off 0 0 900 200 screen 60 420 1800 600 filter\n")?;
    let drawn = started.elapsed();
    // A picture with alpha, only in the offscreen surface: a colour wheel of sorts, fading out from the middle.
    const SIDE: usize = 256;
    let mut picture = vec![0u32; SIDE * SIDE];
    for (i, px) in picture.iter_mut().enumerate() {
        let (x, y) = ((i % SIDE) as i32, (i / SIDE) as i32);
        let (dx, dy) = (x - 128, y - 128);
        let fade = 255 - ((dx * dx + dy * dy) as f32).sqrt().min(127.0) as i32 * 2;
        *px = ((fade.max(0) as u32) << 24)
            | ((x as u32) << 16)
            | ((y as u32) << 8)
            | (255 - x as u32);
    }
    gpu.upload(0, 600, SIDE, &picture)?;
    let from = Rect {
        x: 0,
        y: 600,
        w: SIDE as i32,
        h: SIDE as i32,
    };
    let started = Instant::now();
    for i in 0..4 {
        gpu.quad(
            from,
            Rect {
                x: 120 + i * 440,
                y: 430,
                w: 400,
                h: 400,
            },
            0xffff_ffff,
            true,
            true,
        )?;
    }
    // Tinted and stretched, without blending, then half transparent across everything.
    gpu.quad(
        from,
        Rect {
            x: 120,
            y: 860,
            w: 800,
            h: 180,
        },
        0xff40_c0ff,
        false,
        true,
    )?;
    gpu.quad(
        from,
        Rect {
            x: 1000,
            y: 860,
            w: 800,
            h: 180,
        },
        0x80ff_ffff,
        true,
        false,
    )?;
    println!(
        "rectangles in {drawn:?}, quads in {:?}, holding for {seconds} s",
        started.elapsed()
    );
    std::thread::sleep(Duration::from_secs(seconds));
    Ok(())
}

/// Average over `rounds`, of something that costs the CPU `run`'s whole time.
fn time(rounds: u32, mut run: impl FnMut(u32) -> std::io::Result<()>) -> std::io::Result<Duration> {
    let started = Instant::now();
    for round in 0..rounds {
        run(round)?;
    }
    Ok(started.elapsed() / rounds)
}

fn bench(gpu: &mut Gpu) -> std::io::Result<()> {
    let _graphics = GraphicsMode::enter()?;
    let mut fb = Fb::open("/dev/fb0")?;
    let (w, h) = (fb.width as i32, fb.height as i32);
    let all = Rect { x: 0, y: 0, w, h };
    let mut frame = Frame::new(w as u32, h as u32);
    const ROUNDS: u32 = 30;
    const SCROLL: i32 = 24;

    let cpu_fill = time(ROUNDS, |round| {
        frame.fill_rect(all, Rgb::hex((round << 16) | 0x2030));
        fb.present(&frame, all);
        Ok(())
    })?;
    let cpu_scroll = time(ROUNDS, |_| {
        let width = w as usize;
        frame.px.copy_within(SCROLL as usize * width.., 0);
        fb.present(&frame, all);
        Ok(())
    })?;
    let band = Rect {
        x: 0,
        y: 0,
        w,
        h: 100,
    };
    let cpu_band = time(ROUNDS, |round| {
        frame.fill_rect(band, Rgb::hex((round << 16) | 0x4010));
        fb.present(&frame, band);
        Ok(())
    })?;

    let cpu_blend = time(ROUNDS.min(6), |round| {
        let a = 0x40 + round as u8;
        for y in 0..h {
            for x in 0..w {
                frame.blend(x, y, Rgb::hex(0x30_6090), a);
            }
        }
        fb.present(&frame, all);
        Ok(())
    })?;

    let gpu_fill = time(ROUNDS, |round| gpu.fill(all, (round << 16) | 0x2030))?;
    let gpu_scroll = time(ROUNDS, |_| {
        gpu.move_rect(
            Rect {
                x: 0,
                y: SCROLL,
                w,
                h: h - SCROLL,
            },
            0,
            0,
        )
    })?;
    let gpu_band = time(ROUNDS, |round| gpu.fill(band, (round << 16) | 0x4010))?;

    gpu.submit(&format!("fill off 0 0 {w} {h} 40306090\n"))?;
    let gpu_blend = time(ROUNDS, |_| gpu.quad(all, all, 0xffff_ffff, true, false))?;
    let small = Rect {
        x: 0,
        y: 0,
        w: 32,
        h: 32,
    };
    let gpu_quad = time(ROUNDS, |_| gpu.quad(small, small, 0xffff_ffff, true, false))?;

    println!("{w}x{h}, average of {ROUNDS} rounds, until the pixels are in the screen's memory");
    println!("full fill:         CPU {cpu_fill:>10.1?}   GPU {gpu_fill:>10.1?}");
    println!("scroll {SCROLL} rows:    CPU {cpu_scroll:>10.1?}   GPU {gpu_scroll:>10.1?}");
    println!("fill 100 rows:     CPU {cpu_band:>10.1?}   GPU {gpu_band:>10.1?}");
    println!("blend whole screen: CPU {cpu_blend:>10.1?}   GPU {gpu_blend:>10.1?}");
    println!("one 32x32 quad, alone:             GPU {gpu_quad:>10.1?}");
    Ok(())
}

/// A pattern in which a wrong row, column or stale copy shows.
fn pattern(frame: &mut Frame, r: Rect, seed: u32) {
    let width = frame.w as usize;
    for y in r.y..r.bottom() {
        let line = y as usize * width;
        for x in r.x..r.right() {
            frame.px[line + x as usize] =
                (x as u32).wrapping_mul(31) ^ (y as u32).wrapping_mul(1009) ^ seed.wrapping_mul(0x01_0101);
        }
    }
}

/// Compares samples of `r` on the screen with the frame.
fn same_on_screen(fb: &Fb, frame: &Frame, r: Rect) -> std::io::Result<()> {
    let (columns, rows) = (((r.w - 1) / 16).max(1), ((r.h - 1) / 16).max(1));
    for y in (r.y..r.bottom()).step_by(rows as usize) {
        for x in (r.x..r.right()).step_by(columns as usize) {
            let shown = fb.read_pixel(x as u32, y as u32) & 0xff_ffff;
            let meant = frame.px[y as usize * frame.w as usize + x as usize] & 0xff_ffff;
            if shown != meant {
                return Err(std::io::Error::other(format!(
                    "({x}, {y}) of {r:?} shows {shown:#x}, not {meant:#x}"
                )));
            }
        }
    }
    Ok(())
}

fn present(gpu: &mut Gpu) -> std::io::Result<()> {
    let _graphics = GraphicsMode::enter()?;
    let fb_out = Fb::open("/dev/fb0")?;
    let (w, h) = (fb_out.width as i32, fb_out.height as i32);
    let mut fb = fb_out;
    let mut frame = Frame::new(w as u32, h as u32);
    let buffer = gpu.buffer(w as u32, h as u32)?;
    if frame.move_to_buffer(buffer).is_err() {
        return Err(std::io::Error::other("a buffer does not fit the frame"));
    }
    const ROUNDS: u32 = 30;
    println!("{w}x{h}, average of {ROUNDS} rounds, painting not counted");
    println!("{:>12} {:>10} {:>10}  rule says", "rectangle", "CPU", "GPU");
    for (rw, rh) in [
        (w, h),
        (w, h / 2),
        (w, 200),
        (w, 100),
        (w, 32),
        (960, 540),
        (640, 360),
        (320, 180),
        (128, 64),
        (64, 32),
        (64, h),
    ] {
        let r = Rect::new(0, 0, rw, rh);
        pattern(&mut frame, r, rw as u32);
        let cpu = time(ROUNDS, |_| {
            fb.present(&frame, r);
            Ok(())
        })?;
        let buffer = frame.buffer().expect("moved above");
        let mut batch = Batch::new();
        batch.clean(buffer, 0, rh as u32);
        batch.blit(buffer, r, r, false);
        let by_gpu = time(ROUNDS, |_| gpu.run(&batch))?;
        same_on_screen(&fb, &frame, r)?;
        println!(
            "{:>12} {cpu:>10.1?} {by_gpu:>10.1?}  {}",
            format!("{rw}x{rh}"),
            if worth_blitting(&r) { "GPU" } else { "CPU" }
        );
    }

    // The way apps and the shell go: a frame of its own, a large change and a small one in one call, each checked.
    let mut screen = Screen::new(fb);
    let mut frame = Frame::new(w as u32, h as u32);
    screen.prepare(&mut frame);
    if !screen.uses_gpu() {
        return Err(std::io::Error::other("Screen found no GPU"));
    }
    let (big, small) = (Rect::new(0, 100, w, 400), Rect::new(64, 32, 64, 32));
    for seed in 0..3 {
        pattern(&mut frame, big, seed + 1);
        pattern(&mut frame, small, seed + 11);
        screen.present_all(&frame, &[big, small]);
        if screen.last_gpu_rects != 1 {
            return Err(std::io::Error::other(format!(
                "the GPU showed {} rectangles, not 1",
                screen.last_gpu_rects
            )));
        }
        same_on_screen(&screen.fb, &frame, big)?;
        same_on_screen(&screen.fb, &frame, small)?;
    }
    println!("the frame reached the screen both ways, three times: correct");
    Ok(())
}

const TILE: u32 = 256;

/// What a browser's painter might do: a pattern with a seam at each tile's edge, so that a wrong position shows.
fn paint(tile: &mut Buffer, column: u32, row: u32, phase: u32) {
    for y in 0..TILE {
        let line = tile.row(y);
        for (x, px) in line.iter_mut().enumerate() {
            let x = x as u32;
            let edge = x == 0 || y == 0;
            *px = if edge {
                0xff20_2020
            } else {
                0xff00_0000
                    | ((column * 40 + x / 2) & 0xff) << 16
                    | ((row * 60 + y / 2) & 0xff) << 8
                    | ((phase + x + y) / 4 & 0xff)
            };
        }
    }
}

/// Where tile (`column`, `row`) goes for a page scrolled up by `scroll`, at `scale` per cent.
fn place(column: u32, row: u32, scroll: i32, scale: i32) -> Rect {
    let side = TILE as i32 * scale / 100;
    Rect {
        x: column as i32 * side,
        y: row as i32 * side - scroll,
        w: side,
        h: side,
    }
}

/// Only what is on the screen: a rectangle off its edge is refused, as the source of a browser's own clipping would have to be.
fn clip(place: Rect, width: i32, height: i32) -> Option<(Rect, Rect)> {
    let to = place.intersect(&Rect {
        x: 0,
        y: 0,
        w: width,
        h: height,
    });
    if to.is_empty() {
        return None;
    }
    let side = TILE as i32;
    let from = Rect {
        x: (to.x - place.x) * side / place.w,
        y: (to.y - place.y) * side / place.h,
        w: (to.w * side / place.w).max(1),
        h: (to.h * side / place.h).max(1),
    };
    Some((from, to))
}

fn average(rounds: u32, started: Instant) -> Duration {
    started.elapsed() / rounds
}

fn tiles(gpu: &mut Gpu, seconds: u64) -> std::io::Result<()> {
    let _graphics = GraphicsMode::enter()?;
    let fb = Fb::open("/dev/fb0")?;
    let (w, h) = (fb.width as i32, fb.height as i32);
    let (columns, rows) = ((w as u32).div_ceil(TILE), (h as u32).div_ceil(TILE) + 1);
    let mut page = Vec::new();
    let started = Instant::now();
    for _ in 0..columns * rows {
        page.push(gpu.buffer(TILE, TILE)?);
    }
    println!(
        "{} tiles of {TILE}x{TILE}, made and mapped in {:?}",
        page.len(),
        started.elapsed()
    );

    let started = Instant::now();
    for (i, tile) in page.iter_mut().enumerate() {
        paint(tile, i as u32 % columns, i as u32 / columns, 0);
    }
    println!(
        "painted by the CPU, cached memory: {:?} per tile",
        average(page.len() as u32, started)
    );
    let mut batch = Batch::new();
    let started = Instant::now();
    for tile in &page {
        batch.clean(tile, 0, TILE);
    }
    gpu.run(&batch)?;
    println!(
        "handed over, all rows: {:?} per tile",
        average(page.len() as u32, started)
    );

    let frame = |gpu: &mut Gpu,
                 page: &[Buffer],
                 scroll: i32,
                 scale: i32,
                 tint: u32,
                 blend: bool,
                 filter: bool| {
        let mut batch = Batch::new();
        for (i, tile) in page.iter().enumerate() {
            let place = place(i as u32 % columns, i as u32 / columns, scroll, scale);
            if let Some((from, to)) = clip(place, w, h) {
                if blend || scale != 100 {
                    batch.quad(tile, from, to, tint, blend, filter);
                } else {
                    batch.blit(tile, from, to, false);
                }
            }
        }
        gpu.run(&batch)
    };

    // The pixels are checked where it is cheap: a tile corner at scale 1 is its seam colour.
    const ROUNDS: u32 = 60;
    let started = Instant::now();
    for round in 0..ROUNDS {
        frame(
            gpu,
            &page,
            (round as i32 * 4) % TILE as i32,
            100,
            !0,
            false,
            false,
        )?;
    }
    println!(
        "scroll, whole page, A2D blits:     {:?} per frame",
        average(ROUNDS, started)
    );
    let started = Instant::now();
    for round in 0..ROUNDS {
        frame(
            gpu,
            &page,
            (round as i32 * 4) % TILE as i32,
            100,
            !0,
            true,
            false,
        )?;
    }
    println!(
        "scroll, whole page, blended quads: {:?} per frame",
        average(ROUNDS, started)
    );
    let started = Instant::now();
    for round in 0..ROUNDS {
        frame(gpu, &page, 0, 50 + round as i32, !0, false, true)?;
    }
    println!(
        "zoom 50-110%, filtered quads:      {:?} per frame",
        average(ROUNDS, started)
    );

    // One row of tiles changes in each frame, as when a page animates, against a frame copied whole by the CPU.
    let started = Instant::now();
    for round in 0..ROUNDS {
        let mut batch = Batch::new();
        for column in 0..columns {
            let tile = &mut page[column as usize];
            paint(tile, column, 0, round * 8);
            batch.clean(tile, 0, TILE);
        }
        gpu.run(&batch)?;
        frame(gpu, &page, 0, 100, !0, false, false)?;
    }
    println!(
        "repaint {columns} tiles and show the page: {:?} per frame",
        average(ROUNDS, started)
    );
    let mut whole = Frame::new(w as u32, h as u32);
    let mut fb = fb;
    let all = Rect { x: 0, y: 0, w, h };
    let started = Instant::now();
    for round in 0..ROUNDS {
        for column in 0..columns {
            paint(&mut page[column as usize], column, 0, round * 8);
        }
        for (i, tile) in page.iter_mut().enumerate() {
            let place = place(i as u32 % columns, i as u32 / columns, 0, 100);
            let Some((_, to)) = clip(place, w, h) else {
                continue;
            };
            for y in 0..to.h {
                let row = (y as u32 + (to.y - place.y) as u32).min(TILE - 1);
                let line = &tile.row(row)[..to.w as usize];
                let at = (to.y + y) as usize * w as usize + to.x as usize;
                whole.px[at..at + line.len()].copy_from_slice(line);
            }
        }
        fb.present(&whole, all);
    }
    println!(
        "the same by the CPU, copied:          {:?} per frame",
        average(ROUNDS, started)
    );

    // What is left on the screen: a page at 100%, a half-transparent tile over it, and one scaled up, with their pixels checked.
    frame(gpu, &page, 0, 100, !0, false, false)?;
    let (red, seam) = (0x80ff_0000u32, 0xff20_2020u32);
    let mut overlay = gpu.buffer(TILE, TILE)?;
    overlay.pixels().fill(red);
    let mut batch = Batch::new();
    batch.clean(&overlay, 0, TILE);
    batch.quad(
        &overlay,
        overlay.bounds(),
        Rect {
            x: 300,
            y: 300,
            w: 512,
            h: 384,
        },
        !0,
        true,
        true,
    );
    batch.quad(
        &page[0],
        page[0].bounds(),
        Rect {
            x: 1000,
            y: 400,
            w: 640,
            h: 512,
        },
        0xc0ff_ffff,
        true,
        true,
    );
    gpu.run(&batch)?;
    let screen = fb.read_pixel(0, 0);
    let inside = fb.read_pixel(500, 500);
    println!(
        "corner of tile 0 on the screen {screen:#x} (painted {seam:#x}); under half red {inside:#x}"
    );
    if screen & 0xff_ffff != seam & 0xff_ffff {
        return Err(std::io::Error::other("tile 0 is not where it was put"));
    }
    println!("holding for {seconds} s");
    std::thread::sleep(Duration::from_secs(seconds));
    Ok(())
}

/// Four quadrants, each its own (Y, Cb, Cr): BT.601's 75% red, green and blue, and a grey.
const QUADRANTS: [(u8, u8, u8); 4] = [(65, 100, 212), (112, 72, 58), (35, 212, 114), (180, 128, 128)];

/// A `w` by `h` picture of [`QUADRANTS`], left to right then top to bottom, into `buffer` as `layout` places it.
fn quadrants(buffer: &mut Buffer, layout: &YuvLayout) {
    let (w, h) = (layout.width as usize, layout.height as usize);
    let at = |x: usize, y: usize| QUADRANTS[(x * 2 / w) + 2 * (y * 2 / h)];
    let bytes = buffer.bytes_mut();
    for y in 0..h {
        for x in 0..w {
            bytes[layout.y_offset + y * layout.y_pitch + x] = at(x, y).0;
        }
    }
    for y in 0..layout.chroma_rows() {
        for x in 0..(w + 1) / 2 {
            let (_, u, v) = at(x * 2, y * 2);
            if layout.nv12 {
                bytes[layout.c_offset + y * layout.c_pitch + 2 * x] = u;
                bytes[layout.c_offset + y * layout.c_pitch + 2 * x + 1] = v;
            } else {
                bytes[layout.c_offset + y * layout.c_pitch + x] = u;
                bytes[layout.v_offset + y * layout.c_pitch + x] = v;
            }
        }
    }
}

/// What `matrix` makes of one sample, as the shader does it, rounded as the screen's 8 bits are.
fn through(matrix: &[f32; 12], (y, u, v): (u8, u8, u8)) -> [u8; 3] {
    let s = [y as f32 / 255.0, u as f32 / 255.0, v as f32 / 255.0];
    let mut out = [0u8; 3];
    for (i, c) in out.iter_mut().enumerate() {
        let r = &matrix[i * 4..i * 4 + 4];
        *c = ((r[0] * s[0] + r[1] * s[1] + r[2] * s[2] + r[3]) * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    out
}

/// The middle of each quadrant of `to` on the screen, as red, green, blue.
fn shown_quadrants(fb: &Fb, to: Rect) -> [[u8; 3]; 4] {
    let mut out = [[0u8; 3]; 4];
    for (i, px) in out.iter_mut().enumerate() {
        let x = to.x + to.w / 4 + (i as i32 % 2) * to.w / 2;
        let y = to.y + to.h / 4 + (i as i32 / 2) * to.h / 2;
        let p = fb.read_pixel(x as u32, y as u32);
        *px = [(p >> 16) as u8, (p >> 8) as u8, p as u8];
    }
    out
}

fn close(a: [u8; 3], b: [u8; 3], by: u8) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= by)
}

fn yuv(gpu: &mut Gpu, a2d: bool) -> std::io::Result<()> {
    let _graphics = GraphicsMode::enter()?;
    let fb = Fb::open("/dev/fb0")?;
    let identity = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0];
    let bt601 = yuv_matrix(false, false);
    gpu.fill(Rect::new(0, 0, fb.width as i32, fb.height as i32), 0x000000)?;
    let mut failed = 0;
    for (n, nv12) in [true, false].into_iter().enumerate() {
        let name = if nv12 { "NV12" } else { "I420" };
        let layout = YuvLayout::packed(64, 64, nv12);
        let mut buffer = gpu.bytes_buffer(layout.bytes())?;
        quadrants(&mut buffer, &layout);
        let rows = layout.bytes().div_ceil(buffer.pitch as usize) as u32;
        let from = Rect::new(0, 0, 64, 64);
        if a2d {
            let to = Rect::new(100 + 300 * n as i32, 600, 128, 128);
            let mut batch = Batch::new();
            batch.clean(&buffer, 0, rows).yuv_by_a2d(&buffer, &layout, from, to);
            gpu.run(&batch)?;
            let shown = shown_quadrants(&fb, to);
            for (i, px) in shown.iter().enumerate() {
                let (y, u, v) = QUADRANTS[i];
                let what = if close(*px, [y, u, v], 2) {
                    "the planes as they are"
                } else if close(*px, through(&bt601, QUADRANTS[i]), 3) {
                    "converted as BT.601"
                } else if close(*px, through(&yuv_matrix(true, false), QUADRANTS[i]), 3) {
                    "converted as BT.709"
                } else {
                    "neither"
                };
                println!("a2d {name} quadrant {i}: (Y, Cb, Cr) ({y}, {u}, {v}) shows {px:?}: {what}");
            }
            gpu.free(buffer)?;
            continue;
        }
        for (k, (label, matrix, by)) in [("identity", &identity, 1u8), ("BT.601", &bt601, 2)].into_iter().enumerate() {
            for filter in [false, true] {
                let to = Rect::new(100 + 300 * n as i32 + 150 * filter as i32, 100 + 200 * k as i32, 128, 128);
                let mut batch = Batch::new();
                batch.clean(&buffer, 0, rows).yuv(&buffer, &layout, from, to, matrix, filter);
                gpu.run(&batch)?;
                let shown = shown_quadrants(&fb, to);
                for (i, px) in shown.iter().enumerate() {
                    let meant = through(matrix, QUADRANTS[i]);
                    let ok = close(*px, meant, by);
                    failed += usize::from(!ok);
                    println!(
                        "{name} {label}{} quadrant {i}: shows {px:?}, meant {meant:?}{}",
                        if filter { " filter" } else { "" },
                        if ok { "" } else { "  WRONG" }
                    );
                }
            }
        }
        gpu.free(buffer)?;
    }
    if a2d {
        return Ok(());
    }
    // What a frame of video costs: copying planes from a decoder's memory in and cleaning them, then drawing scaled to the screen.
    const ROUNDS: u32 = 30;
    let (sw, sh) = (fb.width as i32, fb.height as i32);
    for (w, h) in [(854u32, 356u32), (1280, 534), (1280, 720), (1920, 800), (1920, 1080)] {
        let layout = YuvLayout::packed(w, h, false);
        let mut buffer = gpu.bytes_buffer(layout.bytes())?;
        let decoded = vec![0x80u8; layout.bytes()];
        let rows = layout.bytes().div_ceil(buffer.pitch as usize) as u32;
        let copy = time(ROUNDS, |_| {
            buffer.bytes_mut()[..decoded.len()].copy_from_slice(&decoded);
            Ok(())
        })?;
        let fit_h = (sw as i64 * h as i64 / w as i64).min(sh as i64) as i32;
        let to = Rect::new(0, (sh - fit_h) / 2, sw, fit_h);
        let mut clean = Batch::new();
        clean.clean(&buffer, 0, rows);
        let clean_only = time(ROUNDS, |_| gpu.run(&clean))?;
        let mut draw = Batch::new();
        draw.clean(&buffer, 0, rows).yuv(&buffer, &layout, Rect::new(0, 0, w as i32, h as i32), to, &bt601, true);
        let both = time(ROUNDS, |_| gpu.run(&draw))?;
        println!(
            "{w}x{h} I420 to {}x{}: copy in {copy:.1?}, clean {clean_only:.1?}, clean and draw {both:.1?}",
            to.w, to.h
        );
        gpu.free(buffer)?;
    }
    if gpu.has_vblank() {
        // Behind the scan: where the kernel saw it before and after each draw, and how long it waited.
        let layout = YuvLayout::packed(1920, 1080, false);
        let mut buffer = gpu.bytes_buffer(layout.bytes())?;
        let rows = layout.bytes().div_ceil(buffer.pitch as usize) as u32;
        buffer.bytes_mut().fill(0x80);
        for round in 0..10 {
            let mut batch = Batch::new();
            batch.clean(&buffer, 0, rows).vblank(sh as u32);
            for i in 0..4 {
                let (f, t) = (Rect::new(0, 270 * i, 1920, 270), Rect::new(0, sh * i / 4, sw, sh / 4));
                batch.yuv(&buffer, &layout, f, t, &bt601, true);
            }
            let started = Instant::now();
            gpu.run(&batch)?;
            let took = started.elapsed();
            let status = gpu.status()?;
            let get = |k: &str| status.lines().find_map(|l| l.strip_prefix(k)).unwrap_or("?").trim().to_owned();
            println!(
                "vblank {round}: {took:.1?} in all, waited {} us, scan row {} before the draw and {} after{}",
                get("vblank_last_wait_us"),
                get("vblank_row_before"),
                get("vblank_row_after"),
                if get("vblank_gave_up") != "0" { ", gave up at least once" } else { "" }
            );
            std::thread::sleep(Duration::from_millis(7 * round as u64));
        }
        gpu.free(buffer)?;
    } else {
        println!("this kernel has no vblank");
    }
    if failed > 0 {
        return Err(std::io::Error::other(format!("{failed} quadrants showed the wrong colour")));
    }
    println!("every quadrant showed its colour: correct");
    Ok(())
}

/// A number from the device's status, `key value` per line.
fn status_value(status: &str, key: &str) -> Option<u64> {
    status.lines().find_map(|line| line.strip_prefix(key)?.strip_prefix(' ')?.trim().parse().ok())
}

fn status_word<'a>(status: &'a str, key: &str) -> &'a str {
    status.lines().find_map(|line| line.strip_prefix(key)?.strip_prefix(' ')).unwrap_or("?").trim()
}

/// xorshift64: the same sleeps every run.
fn next(x: &mut u64) -> u64 {
    *x ^= *x << 13;
    *x ^= *x >> 7;
    *x ^= *x << 17;
    *x
}

fn summary(label: &str, times: &[Duration]) {
    if times.is_empty() {
        println!("{label}: none");
        return;
    }
    let mean = times.iter().sum::<Duration>() / times.len() as u32;
    println!("{label}: {} batches, mean {mean:.1?}, max {:.1?}", times.len(), times.iter().max().unwrap());
}

/// See the module's list: `power`.
fn power(gpu: &mut Gpu, cycles: u32, max_sleep_ms: Option<u64>) -> std::io::Result<()> {
    let _graphics = GraphicsMode::enter()?;
    let fb = Fb::open("/dev/fb0")?;
    gpu.fill(Rect::new(0, 0, fb.width as i32, fb.height as i32), 0x000000)?;
    let status = gpu.status()?;
    let idle_ms = status_value(&status, "idle_ms").unwrap_or(0);
    let max_sleep_ms = max_sleep_ms.unwrap_or(if idle_ms == 0 { 200 } else { 3 * idle_ms });
    let (suspends0, resumes0) = (status_value(&status, "suspends").unwrap_or(0), status_value(&status, "resumes").unwrap_or(0));
    if idle_ms == 0 {
        println!("idle_ms 0 (lucina.gpu.idle=off): nothing will suspend; the checks run anyway");
    }
    let identity = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0];
    let bt601 = yuv_matrix(false, false);
    let mut pictures = Vec::new();
    for nv12 in [true, false] {
        let layout = YuvLayout::packed(64, 64, nv12);
        let mut buffer = gpu.bytes_buffer(layout.bytes())?;
        quadrants(&mut buffer, &layout);
        let rows = layout.bytes().div_ceil(buffer.pitch as usize) as u32;
        pictures.push((buffer, layout, rows));
    }
    // (picture, matrix, tolerance against the arithmetic, where)
    let draws: Vec<(usize, &[f32; 12], u8, Rect)> = (0..2)
        .flat_map(|p| {
            [(&identity, 1u8), (&bt601, 2u8)]
                .into_iter()
                .enumerate()
                .map(move |(m, (matrix, by))| (p, matrix, by, Rect::new(100 + 300 * p as i32, 100 + 200 * m as i32, 128, 128)))
        })
        .collect();
    let from = Rect::new(0, 0, 64, 64);
    let batch_of = |pictures: &Vec<(Buffer, YuvLayout, u32)>, colour: Option<u32>| {
        let mut batch = Batch::new();
        for (buffer, _, rows) in pictures {
            batch.clean(buffer, 0, *rows);
        }
        for (p, matrix, _, to) in &draws {
            let (buffer, layout, _) = &pictures[*p];
            batch.yuv(buffer, layout, from, *to, matrix, false);
        }
        if let Some(colour) = colour {
            batch.fill(Rect::new(700, 100, 64, 64), colour);
        }
        batch
    };
    gpu.run(&batch_of(&pictures, None))?;
    let mut mismatches = 0u64;
    let reference: Vec<[[u8; 3]; 4]> = draws.iter().map(|(_, _, _, to)| shown_quadrants(&fb, *to)).collect();
    for ((_, matrix, by, _), shown) in draws.iter().zip(&reference) {
        for (i, px) in shown.iter().enumerate() {
            let meant = through(matrix, QUADRANTS[i]);
            if !close(*px, meant, *by) {
                mismatches += 1;
                println!("reference quadrant {i}: shows {px:?}, meant {meant:?}  WRONG");
            }
        }
    }
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let (mut after_resume, mut others) = (Vec::new(), Vec::new());
    let mut last_resumes = status_value(&gpu.status()?, "resumes").unwrap_or(resumes0);
    for cycle in 0..cycles {
        std::thread::sleep(Duration::from_millis(next(&mut seed) % (max_sleep_ms + 1)));
        let colour = (cycle.wrapping_mul(0x0004_0b17) ^ 0x0055_aa33) & 0x00ff_ffff;
        let batch = batch_of(&pictures, Some(colour));
        let started = Instant::now();
        gpu.run(&batch)?;
        let took = started.elapsed();
        let status = gpu.status()?;
        let resumes = status_value(&status, "resumes").unwrap_or(last_resumes);
        if resumes > last_resumes { after_resume.push(took) } else { others.push(took) }
        last_resumes = resumes;
        for ((_, _, _, to), meant) in draws.iter().zip(&reference) {
            let shown = shown_quadrants(&fb, *to);
            for (i, (px, want)) in shown.iter().zip(meant).enumerate() {
                if px != want {
                    mismatches += 1;
                    if mismatches <= 10 {
                        println!("cycle {cycle}: quadrant {i} of {to:?} shows {px:?}, was {want:?}  WRONG");
                    }
                }
            }
        }
        let seen = fb.read_pixel(732, 132) & 0x00ff_ffff;
        if seen != colour {
            mismatches += 1;
            if mismatches <= 10 {
                println!("cycle {cycle}: fill shows {seen:#08x}, meant {colour:#08x}  WRONG");
            }
        }
        if cycle % 25 == 24 {
            println!(
                "cycle {}: {mismatches} wrong, {} suspends, {} resumes",
                cycle + 1,
                status_value(&status, "suspends").unwrap_or(0).saturating_sub(suspends0),
                resumes.saturating_sub(resumes0)
            );
        }
    }
    let status = gpu.status()?;
    println!(
        "{cycles} cycles, sleeps 0..={max_sleep_ms} ms, idle_ms {idle_ms}: {mismatches} wrong, {} suspends, {} resumes, power {}, forced {}",
        status_value(&status, "suspends").unwrap_or(0).saturating_sub(suspends0),
        status_value(&status, "resumes").unwrap_or(0).saturating_sub(resumes0),
        status_word(&status, "power"),
        status_word(&status, "forced"),
    );
    summary("batches right after a resume", &after_resume);
    summary("batches on a powered GPU", &others);
    for (buffer, _, _) in pictures {
        gpu.free(buffer)?;
    }
    if mismatches > 0 || status_word(&status, "power") == "failed" {
        return Err(std::io::Error::other(format!("{mismatches} wrong, power {}", status_word(&status, "power"))));
    }
    println!("every picture was the same after every resume: correct");
    Ok(())
}

/// See the module's list: `power-race`.
fn power_race(gpu: &mut Gpu, seconds: u64, threads: u32) -> std::io::Result<()> {
    let _graphics = GraphicsMode::enter()?;
    let status = gpu.status()?;
    let idle_ms = status_value(&status, "idle_ms").unwrap_or(0);
    let max_sleep_ms = if idle_ms == 0 { 50 } else { 2 * idle_ms };
    let (suspends0, resumes0) = (status_value(&status, "suspends").unwrap_or(0), status_value(&status, "resumes").unwrap_or(0));
    let until = Instant::now() + Duration::from_secs(seconds);
    let handles: Vec<_> = (0..threads)
        .map(|t| {
            std::thread::spawn(move || -> (u64, u64) {
                let (Some(mut gpu), Ok(fb)) = (Gpu::open(), Fb::open("/dev/fb0")) else {
                    eprintln!("thread {t}: no device");
                    return (0, 1);
                };
                let mut seed = 0x2545_f491_4f6c_dd1du64 ^ (u64::from(t) + 1).wrapping_mul(0x9e37_79b9);
                let (mut batches, mut failures) = (0u64, 0u64);
                let r = Rect::new(100 + 50 * t as i32, 700, 32, 32);
                while Instant::now() < until {
                    std::thread::sleep(Duration::from_millis(next(&mut seed) % (max_sleep_ms + 1)));
                    let colour = ((batches as u32).wrapping_mul(0x0001_0307) ^ (t << 20)) & 0x00ff_ffff;
                    let mut batch = Batch::new();
                    batch.fill(r, colour);
                    batches += 1;
                    if let Err(err) = gpu.run(&batch) {
                        failures += 1;
                        eprintln!("thread {t}: batch {batches}: {err}");
                        continue;
                    }
                    let seen = fb.read_pixel(r.x as u32 + 16, r.y as u32 + 16) & 0x00ff_ffff;
                    if seen != colour {
                        failures += 1;
                        eprintln!("thread {t}: batch {batches} shows {seen:#08x}, meant {colour:#08x}");
                    }
                }
                (batches, failures)
            })
        })
        .collect();
    let mut failed = 0;
    for (t, handle) in handles.into_iter().enumerate() {
        let (batches, failures) = handle.join().unwrap_or((0, 1));
        println!("thread {t}: {batches} batches, {failures} failures");
        failed += failures;
    }
    let status = gpu.status()?;
    println!(
        "{threads} threads, {seconds} s, sleeps 0..={max_sleep_ms} ms: {} suspends, {} resumes, power {}, forced {}",
        status_value(&status, "suspends").unwrap_or(0).saturating_sub(suspends0),
        status_value(&status, "resumes").unwrap_or(0).saturating_sub(resumes0),
        status_word(&status, "power"),
        status_word(&status, "forced"),
    );
    if failed > 0 {
        return Err(std::io::Error::other(format!("{failed} failures")));
    }
    println!("no batch failed and every fill showed its colour: correct");
    Ok(())
}
