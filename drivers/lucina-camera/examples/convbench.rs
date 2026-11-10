//! Times the preview's steps on the machine it runs on: MJPEG decode, YUY2 conversion and the scaling to the screen, each
//! alone, for a JPEG frame and a YUY2 frame read from files (a camcheck recording's frames, or the camera's own photo).
//!
//! `convbench <frame.jpg> <frame.yuy2> <yuy2 width> <yuy2 height> [screen w] [screen h] [rounds] [cpu]`
//!
//! With `cpu` the thread runs on that CPU only; `gold` picks the fastest CPUs (`lucina_camera::affinity`). The MJPEG frame is also
//! timed decoded straight to I420, the preview's GPU path, where nothing is converted or scaled on the CPU.

use std::time::Instant;

use lucina_camera::affinity;
use lucina_camera::convert::{self, I420, Matrix};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let jpeg = std::fs::read(&a[1]).unwrap();
    let yuy2 = std::fs::read(&a[2]).unwrap();
    let (yw, yh): (usize, usize) = (a[3].parse().unwrap(), a[4].parse().unwrap());
    let sw: usize = a.get(5).map_or(1920, |s| s.parse().unwrap());
    let sh: usize = a.get(6).map_or(1080, |s| s.parse().unwrap());
    let rounds: u32 = a.get(7).map_or(20, |s| s.parse().unwrap());
    let cpus = match a.get(8).map(String::as_str) {
        None => Vec::new(),
        Some("gold") => affinity::fastest_cpus(),
        Some(n) => vec![n.parse().unwrap()],
    };
    affinity::pin_current_thread(&cpus).unwrap();
    println!("on CPU {:?} (asked {cpus:?})", affinity::current_cpu());
    let (mut rgb, mut out) = (Vec::new(), Vec::new());
    let ms = |t: Instant| t.elapsed().as_secs_f64() * 1000.0 / f64::from(rounds);

    let t = Instant::now();
    let mut size = (0, 0);
    for _ in 0..rounds {
        size = convert::jpeg_to_xrgb(&jpeg, &mut rgb).unwrap();
    }
    let decode = ms(t);
    let (_, _, dw, dh) = convert::fit(size.0, size.1, sw, sh);
    let t = Instant::now();
    for _ in 0..rounds {
        convert::scale_xrgb(&rgb, size.0, size.1, dw, dh, &mut out);
    }
    let scale = ms(t);
    println!(
        "MJPG {}x{}: decode {decode:.1} ms, scale to {dw}x{dh} {scale:.1} ms",
        size.0, size.1
    );

    let mut yuv = I420::default();
    let t = Instant::now();
    for _ in 0..rounds {
        convert::jpeg_to_i420(&jpeg, &mut yuv).unwrap();
    }
    let i420 = ms(t);
    println!(
        "MJPG {}x{}: decode to I420 {i420:.1} ms (no conversion, no scaling: the GPU does both)",
        yuv.width, yuv.height
    );

    let t = Instant::now();
    for _ in 0..rounds {
        convert::yuy2_to_xrgb(&yuy2, yw, yh, Matrix::Bt601, &mut rgb);
    }
    let conv = ms(t);
    let (_, _, dw, dh) = convert::fit(yw, yh, sw, sh);
    let t = Instant::now();
    for _ in 0..rounds {
        convert::scale_xrgb(&rgb, yw, yh, dw, dh, &mut out);
    }
    let scale = ms(t);
    println!("YUY2 {yw}x{yh}: convert {conv:.1} ms, scale to {dw}x{dh} {scale:.1} ms");
}
