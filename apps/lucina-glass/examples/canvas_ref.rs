//! Pixel reference for the canvas: renders fixed scenes (Juno's home with the fixture data, the concept's layouts) and either saves
//! them (`--save <dir>`) or compares against what was saved (`--check <dir>`), allowing at most 1 per channel of difference, and
//! prints how long each scene took. Used to make the canvas faster without changing what it draws.
//!
//! `cargo run --release -p lucina-glass --features fixture --example canvas_ref -- --check <dir>`
use std::time::Instant;

use lucina_glass::home::{self, Home};
use lucina_glass::layouts::{DianaTablet, Fortuna, Juno, Layout};
use lucina_glass::{Canvas, Type, imagery};

fn scenes() -> Vec<(String, Canvas, f64)> {
    let mut out = Vec::new();
    let mut ty = Type::new();
    let (w, h) = (1920u32, 1080u32);
    let bg = imagery::night_sky(w, h, home::K, 7);
    let blurred = bg.blurred((lucina_glass::tokens::glass::BLUR * home::K / 3.0).round().max(1.0) as u32);
    for (i, t) in [0.3f64, 0.9, 2.5].iter().enumerate() {
        let mut hm = Home::new(0.0);
        hm.set_data(0.0, home::fixture::full(0.0));
        hm.enter(0.0);
        let mut cv = Canvas::new(w, h, home::K);
        // Warm the glyph cache so the timing is the drawing.
        hm.draw(&mut cv, &mut ty, *t, &bg, &blurred);
        let s = Instant::now();
        hm.draw(&mut cv, &mut ty, *t, &bg, &blurred);
        out.push((format!("home-{i}"), cv, s.elapsed().as_secs_f64() * 1000.0));
    }
    let mut all: Vec<Box<dyn Layout>> = vec![Box::new(Juno::new(0.0)), Box::new(DianaTablet::new(0.0, false)), Box::new(Fortuna::new(0.0))];
    for l in &mut all {
        let (w, h, k) = l.size();
        let mut cv = Canvas::new(w, h, k);
        l.draw(&mut cv, &mut ty, 3.0);
        let s = Instant::now();
        l.draw(&mut cv, &mut ty, 3.0);
        out.push((l.name().to_string(), cv, s.elapsed().as_secs_f64() * 1000.0));
    }
    out
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (mode, dir) = (args.get(1).map(String::as_str).unwrap_or(""), args.get(2).cloned().unwrap_or_else(|| ".".into()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut bad = false;
    let mut total = 0.0;
    for (name, cv, ms) in scenes() {
        total += ms;
        let path = format!("{dir}/{name}.raw");
        let bytes: Vec<u8> = cv.px.iter().flat_map(|p| p.to_le_bytes()).collect();
        match mode {
            "--save" => {
                std::fs::write(&path, &bytes).unwrap();
                std::fs::write(format!("{dir}/{name}.png"), cv.png()).unwrap();
                println!("{name}: saved, {ms:.1} ms");
            }
            "--check" => {
                let want = std::fs::read(&path).expect("reference: run --save first");
                let mut off = 0usize;
                let mut worst = 0i32;
                for (a, b) in want.chunks(4).zip(bytes.chunks(4)) {
                    let d = (0..3).map(|c| (a[c] as i32 - b[c] as i32).abs()).max().unwrap();
                    worst = worst.max(d);
                    if d > 1 {
                        off += 1;
                    }
                }
                let ok = off == 0 && want.len() == bytes.len();
                bad |= !ok;
                println!("{name}: {} ({off} pixels off by more than 1, worst {worst}), {ms:.1} ms", if ok { "same" } else { "DIFFERENT" });
                if !ok {
                    std::fs::write(format!("{dir}/{name}.new.png"), cv.png()).unwrap();
                }
            }
            _ => {
                eprintln!("usage: canvas_ref --save|--check <dir>");
                std::process::exit(2);
            }
        }
    }
    println!("total drawing time {total:.1} ms");
    if bad {
        std::process::exit(1);
    }
}
