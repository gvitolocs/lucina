//! `lucina-app-glass --render <dir>`: the review set, with no screen. Writes, under `<dir>`:
//!
//! - `screens/<name>.png`: each screen at 1080 x 2400, settled;
//! - `overview.png`: the three side by side;
//! - `variants/juno.png`, `diana-tablet-landscape.png`, `diana-tablet-portrait.png`, `fortuna.png` (and `fortuna-3x.png`):
//!   the other devices;
//! - `anim/<demo>/f000.png...`: every frame of every demo at 30 fps, cropped to its component, at 3x;
//! - `anim/<demo>-strip.png`: six frames of it side by side;
//! - `components.png`: every strip with its name, one under another: each component through its states.

use std::path::Path;

use lucina_glass::tokens::color;
use lucina_glass::layouts::{DianaTablet, Fortuna, Juno, Layout};
use lucina_glass::{Canvas, Color, Font, R, Type};

use crate::stage::{self, DEMOS, SETTLED, Stage};

fn save(cv: &Canvas, path: &Path) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(path, cv.png())
}

/// Six frames side by side, `gap` px apart on carbon.
fn strip(frames: &[Canvas]) -> Canvas {
    let picks: Vec<&Canvas> = (0..6).map(|i| &frames[(i * (frames.len() - 1)) / 5]).collect();
    let gap = 12;
    let (fw, fh) = (picks[0].w, picks[0].h);
    let mut out = Canvas::new(fw * 6 + gap * 7, fh + gap * 2, 1.0);
    out.clear(color::CARBON);
    for (i, f) in picks.iter().enumerate() {
        let x = gap + i as u32 * (fw + gap);
        for y in 0..fh {
            let dst = ((gap + y) * out.w + x) as usize;
            out.px[dst..dst + fw as usize].copy_from_slice(&f.px[(y * fw) as usize..((y + 1) * fw) as usize]);
        }
    }
    out
}

/// Strips stacked with a title over each, scaled to one width.
fn sheet(strips: &[(&str, &str, Canvas)], ty: &mut Type) -> Canvas {
    let width = 2400u32;
    let label = 70u32;
    let scaled: Vec<Canvas> = strips
        .iter()
        .map(|(_, _, s)| {
            let h = (s.h as f32 * width as f32 / s.w as f32).round() as u32;
            s.resized(width, h.max(1))
        })
        .collect();
    let total: u32 = scaled.iter().map(|s| s.h + label).sum::<u32>() + 40;
    let mut out = Canvas::new(width, total, 2.0);
    out.clear(Color::hex(0x08090b));
    let mut y = 20u32;
    for ((name, what, _), s) in strips.iter().zip(&scaled) {
        ty.draw(&mut out, Font::Mono, 13.0, 1.0, 12.0, (y + 26) as f32 / 2.0, color::LIVE, &name.to_uppercase());
        ty.draw(&mut out, Font::Regular, 13.0, 0.0, 12.0, (y + 56) as f32 / 2.0, color::INK_2, what);
        y += label;
        for row in 0..s.h {
            let dst = ((y + row) * width) as usize;
            out.px[dst..dst + width as usize].copy_from_slice(&s.px[(row * width) as usize..((row + 1) * width) as usize]);
        }
        y += s.h;
    }
    out
}

pub fn run(dir: &Path, only: Option<&str>) -> std::io::Result<()> {
    let k = 3.0;
    let t0 = std::time::Instant::now();
    // The screens, settled.
    let mut stage = Stage::new(0.0);
    let names = stage.names();
    let mut shots = Vec::new();
    for (i, name) in names.iter().enumerate() {
        stage.current = i;
        let mut cv = Canvas::new(1080, 2400, k);
        let t = std::time::Instant::now();
        stage.draw(&mut cv, SETTLED);
        eprintln!("screen {name}: {} ms", t.elapsed().as_millis());
        save(&cv, &dir.join(format!("screens/{name}.png")))?;
        shots.push(cv);
    }
    // Half size, side by side.
    let mut over = Canvas::new(3 * 540 + 4 * 40, 1200 + 80, 1.0);
    over.clear(Color::hex(0x0b0c0f));
    for (i, s) in shots.iter().enumerate() {
        over.blit(s, R::new(40.0 + i as f32 * 580.0, 40.0, 540.0, 1200.0), 46.0, 1.0);
    }
    save(&over, &dir.join("overview.png"))?;
    // The icons, 9 to a row.
    let all = lucina_glass::icons::ALL;
    let rows = all.len().div_ceil(9) as u32;
    let mut sheet_cv = Canvas::new(9 * 96 * 3, rows * 96 * 3, 3.0);
    sheet_cv.clear(color::CARBON);
    for (i, icon) in all.iter().enumerate() {
        let (x, y) = ((i % 9) as f32 * 96.0 + 48.0, (i / 9) as f32 * 96.0 + 40.0);
        lucina_glass::icons::draw(&mut sheet_cv, *icon, x, y, 40.0, color::INK);
        stage.ty.draw_centered(&mut sheet_cv, Font::Mono, 8.0, 0.0, x, y + 42.0, color::INK_3, &format!("{icon:?}"));
    }
    save(&sheet_cv, &dir.join("icons.png"))?;
    // The other devices.
    let mut variants: Vec<Box<dyn Layout>> =
        vec![Box::new(Juno::new(0.0)), Box::new(DianaTablet::new(0.0, false)), Box::new(DianaTablet::new(0.0, true)), Box::new(Fortuna::new(0.0))];
    for v in &mut variants {
        let (w, h, vk) = v.size();
        let mut cv = Canvas::new(w, h, vk);
        let t = std::time::Instant::now();
        v.draw(&mut cv, &mut stage.ty, SETTLED);
        eprintln!("variant {}: {} ms", v.name(), t.elapsed().as_millis());
        save(&cv, &dir.join(format!("variants/{}.png", v.name())))?;
        if v.name() == "fortuna" {
            // At 3x, nearest neighbour, to see the pixels.
            let mut big = Canvas::new(w * 3, h * 3, 1.0);
            for y in 0..h * 3 {
                for x in 0..w * 3 {
                    big.px[(y * w * 3 + x) as usize] = cv.px[((y / 3) * w + x / 3) as usize];
                }
            }
            save(&big, &dir.join("variants/fortuna-3x.png"))?;
        }
    }
    // The demos.
    let mut strips = Vec::new();
    for d in DEMOS {
        if only.is_some_and(|o| o != d.name) {
            continue;
        }
        let t = std::time::Instant::now();
        let frames = stage::frames(d, k, 30.0);
        eprintln!("demo {}: {} frames, {} ms", d.name, frames.len(), t.elapsed().as_millis());
        for (i, f) in frames.iter().enumerate() {
            save(f, &dir.join(format!("anim/{}/f{i:03}.png", d.name)))?;
        }
        let s = strip(&frames);
        save(&s, &dir.join(format!("anim/{}-strip.png", d.name)))?;
        strips.push((d.name, d.what, s));
    }
    if only.is_none() {
        let s = sheet(&strips, &mut stage.ty);
        save(&s, &dir.join("components.png"))?;
    }
    eprintln!("done in {} s", t0.elapsed().as_secs_f32());
    Ok(())
}
