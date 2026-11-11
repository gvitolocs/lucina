//! Times the pieces of the shell's frames on one CPU, so that a slow frame can be split into "the work" and "the core it ran on":
//! the page slide's compose, a whole-frame copy, the darkening pass, the backdrop and its blur, a whole home frame and the
//! damaged regions of a focus walk. The scenario suite's CPU half (`docs/research/juno-perf.md`).
//!
//! `cargo run --release -p lucina-glass --features fixture --example frame_bench -- [--cpu <n>] [--reps <n>]`
//!
//! One `name value unit` line per result, prefixed with the CPU it ran on, so that runs on a Silver and a Gold core compare with
//! `diff`. With `--cpu` the thread is pinned there first (`sched_setaffinity`); without it, it runs wherever the kernel puts it.
use std::time::Instant;

use lucina_glass::home::{self, Home, Key};
use lucina_glass::{Canvas, Type};

fn pin(cpu: usize) -> bool {
    // SAFETY: a zeroed cpu_set_t is the empty set; CPU_SET writes inside it; sched_setaffinity reads `size_of` bytes of it.
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_SET(cpu, &mut set);
        libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set) == 0
    }
}

fn cpu_now() -> i32 {
    // SAFETY: no arguments, no memory.
    unsafe { libc::sched_getcpu() }
}

/// Median and maximum of `reps` runs of `f`, in ms.
fn time(reps: usize, mut f: impl FnMut()) -> (f64, f64) {
    let mut v: Vec<f64> = (0..reps)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    (v[v.len() / 2], v[v.len() - 1])
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut cpu, mut reps) = (None, 15usize);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--cpu" => cpu = args.next().and_then(|v| v.parse().ok()),
            "--reps" => reps = args.next().and_then(|v| v.parse().ok()).unwrap_or(15),
            _ => {
                eprintln!("usage: frame_bench [--cpu <n>] [--reps <n>]");
                std::process::exit(2);
            }
        }
    }
    if let Some(c) = cpu
        && !pin(c)
    {
        eprintln!(
            "frame_bench: cannot pin to CPU {c}: {}",
            std::io::Error::last_os_error()
        );
        std::process::exit(1);
    }
    let (w, h) = (1920usize, 1080usize);
    let n = w * h;
    let report = |name: &str, (med, max): (f64, f64)| {
        println!("cpu{} {name} {med:.2} ms (max {max:.2})", cpu_now());
    };

    // Memory: what any full-screen step costs at least.
    let src: Vec<u32> = (0..n as u32).map(|i| i.wrapping_mul(2654435761)).collect();
    let src2: Vec<u32> = src.iter().map(|v| v ^ 0x00ff_00ff).collect();
    let mut out = vec![0u32; n];
    report(
        "copy-frame",
        time(reps, || out.copy_from_slice(std::hint::black_box(&src))),
    );
    report(
        "fill-frame",
        time(reps, || out.fill(std::hint::black_box(0x0010_2030))),
    );

    // The page slide, as the shell runs it: one compose of two whole pages per frame, across the move.
    let mut step = 0usize;
    report(
        "slide-compose",
        time(reps * 2, || {
            let t = (step % 30) as f32 / 29.0;
            step += 1;
            home::slide_compose(&mut out, &src, &src2, w, h, t, 1);
        }),
    );

    // The backdrop the shell makes at start, and the glass's blurred copy.
    let k = home::K;
    let t = Instant::now();
    let (bg, blurred) = home::sky_backdrop(w as u32, h as u32, k);
    println!(
        "cpu{} sky-backdrop {:.2} ms",
        cpu_now(),
        t.elapsed().as_secs_f64() * 1000.0
    );
    report(
        "quick-blur",
        time(reps.min(5), || {
            drop(home::quick_blur(std::hint::black_box(&bg)))
        }),
    );

    // A whole home frame, warm, then the damage of a focus walk.
    let mut ty = Type::new();
    let mut cv = Canvas::new(w as u32, h as u32, k);
    let mut hm = Home::new(0.0);
    hm.set_data(0.0, home::fixture::full(0.0));
    hm.enter(0.0);
    let _ = hm.damage(0.0);
    hm.draw(&mut cv, &mut ty, 0.0, &bg, &blurred);
    let mut at = 2.5;
    report(
        "home-whole",
        time(reps.min(8), || {
            at += 0.5;
            hm.draw(&mut cv, &mut ty, at, &bg, &blurred);
        }),
    );
    let mut now = at + 5.0;
    let _ = hm.damage(now);
    let mut focus = Vec::new();
    for key in [
        Key::Down,
        Key::Right,
        Key::Right,
        Key::Down,
        Key::Down,
        Key::Right,
    ] {
        let _ = hm.key(now, key);
        let end = now + 0.4;
        while now < end {
            let rects = hm.damage(now);
            let t = Instant::now();
            for r in &rects {
                cv.push_clip(*r, 0.0);
                hm.draw(&mut cv, &mut ty, now, &bg, &blurred);
                cv.pop_clip();
            }
            if !rects.is_empty() {
                focus.push(t.elapsed().as_secs_f64() * 1000.0);
            }
            now += 1.0 / 60.0;
        }
    }
    focus.sort_by(|a, b| a.partial_cmp(b).unwrap());
    if !focus.is_empty() {
        println!(
            "cpu{} focus-damage {:.2} ms (p95 {:.2}, max {:.2}, {} frames)",
            cpu_now(),
            focus[focus.len() / 2],
            focus[focus.len() * 95 / 100],
            focus[focus.len() - 1],
            focus.len()
        );
    }
}
