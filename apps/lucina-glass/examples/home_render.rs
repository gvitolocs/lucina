//! Renders Juno's home with the fixture data (`--features fixture`) to PNG, and times what the shell will do: the backdrop, the
//! first whole frame, and the damaged regions of a few animations, frame by frame.
//!
//! `cargo run --release -p lucina-glass --features fixture --example home_render -- <dir>`
use std::time::Instant;

use lucina_glass::home::{self, Home, Key};
use lucina_glass::{Canvas, Type};

fn main() {
    let dir = std::env::args().nth(1).unwrap_or_else(|| ".".into());
    std::fs::create_dir_all(&dir).unwrap();
    let (w, h) = (1920u32, 1080u32);
    let t = Instant::now();
    let (bg, blurred) = home::sky_backdrop(w, h, home::K);
    println!("backdrop and its glass copy, made: {:?}", t.elapsed());

    let mut ty = Type::new();
    let mut cv = Canvas::new(w, h, home::K);
    let mut hm = Home::new(0.0);
    hm.set_data(0.0, home::fixture::full(0.0));
    hm.enter(0.0);
    let t = Instant::now();
    let _ = hm.damage(0.0);
    hm.draw(&mut cv, &mut ty, 0.0, &bg, &blurred);
    println!("first whole frame (cold glyphs): {:?}", t.elapsed());
    let t = Instant::now();
    hm.draw(&mut cv, &mut ty, 2.5, &bg, &blurred);
    println!("whole frame (warm): {:?}", t.elapsed());
    std::fs::write(format!("{dir}/home.png"), cv.png()).unwrap();

    // Frame by frame through the entrance, then a day change, the segments, a key walk: time only the damage.
    let mut frames = Vec::new();
    let mut now = 3.0;
    let _ = hm.damage(now);
    let mut run = |hm: &mut Home, cv: &mut Canvas, label: &str, from: f64, to: f64, frames: &mut Vec<(String, f64, usize)>| {
        let mut n = from;
        while n < to {
            let rects = hm.damage(n);
            let px: f32 = rects.iter().map(|r| r.w * r.h * home::K * home::K).sum();
            let t = Instant::now();
            for r in &rects {
                cv.push_clip(*r, 0.0);
                hm.draw(cv, &mut ty, n, &bg, &blurred);
                cv.pop_clip();
            }
            frames.push((label.to_string(), t.elapsed().as_secs_f64() * 1000.0, px as usize));
            n += 1.0 / 60.0;
        }
    };
    hm.enter(now);
    run(&mut hm, &mut cv, "entrance", now, now + 1.7, &mut frames);
    now += 2.0;
    let _ = hm.key(now, Key::Down);
    for k in [Key::Right, Key::Right, Key::Down, Key::Down, Key::Down] {
        let _ = hm.key(now, k);
    }
    run(&mut hm, &mut cv, "focus walk + day", now, now + 1.2, &mut frames);
    std::fs::write(format!("{dir}/home-after-keys.png"), cv.png()).unwrap();
    now += 1.5;
    let _ = hm.key(now, Key::Esc);
    run(&mut hm, &mut cv, "idle (live pill, spinners, timer)", now, now + 1.0, &mut frames);

    let mut by: std::collections::BTreeMap<String, Vec<(f64, usize)>> = Default::default();
    for (l, ms, px) in frames {
        by.entry(l).or_default().push((ms, px));
    }
    for (l, v) in by {
        let mut ms: Vec<f64> = v.iter().map(|x| x.0).collect();
        ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let px = v.iter().map(|x| x.1).max().unwrap_or(0);
        println!("{l}: {} frames, median {:.1} ms, p95 {:.1} ms, max {:.1} ms, largest damage {} px", ms.len(), ms[ms.len() / 2], ms[ms.len() * 95 / 100], ms[ms.len() - 1], px);
    }
}
