//! Checks a recording of `/dev/lucina-camera0` reads (headers and frames back to back, as `dd bs=8M <&3` writes them): the frame
//! rate from the kernel's timestamps, sequence gaps, error flags, whether every MJPEG frame decodes, and the colours of the middle
//! frame, which it also writes out as `<out>/middle.jpg` (MJPEG) and `<out>/middle.png` for a look.
//!
//! `cargo run --example camcheck -- <recording> <out dir>`

use std::path::PathBuf;

use lucina_camera::convert::{self, Matrix};
use lucina_camera::{Header, photo};
use lucina_uvc::wire::HEADER_LEN;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (input, out) = (PathBuf::from(&args[1]), PathBuf::from(&args[2]));
    std::fs::create_dir_all(&out).unwrap();
    let data = std::fs::read(&input).unwrap();
    let mut frames: Vec<(Header, &[u8])> = Vec::new();
    let mut at = 0;
    while at + HEADER_LEN <= data.len() {
        let h = Header::decode(&data[at..]).expect("a header");
        let end = at + HEADER_LEN + h.bytes as usize;
        frames.push((h, &data[at + HEADER_LEN..end]));
        at = end;
    }
    assert_eq!(at, data.len(), "the recording ends inside a frame");
    let n = frames.len();
    let errors = frames.iter().filter(|(h, _)| h.error()).count();
    let gaps: u32 = frames.windows(2).map(|w| w[1].0.sequence.wrapping_sub(w[0].0.sequence).saturating_sub(1)).sum();
    let span_us = frames[n - 1].0.time_us - frames[0].0.time_us;
    let fps = (n - 1) as f64 / (span_us as f64 / 1e6);
    // The rate once running: from the 10th frame on (the first ones wait for the camera to start).
    let steady = if n > 20 { (n - 11) as f64 / ((frames[n - 1].0.time_us - frames[10].0.time_us) as f64 / 1e6) } else { fps };
    let h0 = frames[0].0;
    println!(
        "{}: {} frames {} {}x{}, {} with the error flag, {} sequence numbers missing, {:.2} fps over {:.2} s ({:.2} fps from frame 10)",
        input.display(),
        n,
        String::from_utf8_lossy(&h0.fourcc),
        h0.width,
        h0.height,
        errors,
        gaps,
        fps,
        span_us as f64 / 1e6,
        steady
    );
    for (i, (h, f)) in frames.iter().enumerate().filter(|(_, (h, _))| h.error()) {
        println!("  error frame {i}: sequence {}, {} bytes", h.sequence, f.len());
    }
    let mut px = Vec::new();
    let mjpeg = &h0.fourcc == b"MJPG";
    if mjpeg {
        let bad = frames.iter().filter(|(_, f)| convert::jpeg_to_xrgb(f, &mut px).is_err()).count();
        let sizes: Vec<usize> = frames.iter().map(|(_, f)| f.len()).collect();
        println!(
            "  {} of {} JPEGs do not decode; {} to {} bytes, mean {}",
            bad,
            n,
            sizes.iter().min().unwrap(),
            sizes.iter().max().unwrap(),
            sizes.iter().sum::<usize>() / n
        );
    }
    let (h, f) = frames[n / 2];
    let (w, hh) = if mjpeg {
        let p = photo::save_jpeg(&out, "middle", f).unwrap();
        std::fs::rename(&p, out.join("middle.jpg")).unwrap();
        convert::jpeg_to_xrgb(f, &mut px).unwrap()
    } else {
        convert::yuy2_to_xrgb(f, h.width as usize, h.height as usize, Matrix::Bt601, &mut px);
        (h.width as usize, h.height as usize)
    };
    let count = px.len() as f64;
    let ch = |s: u32| px.iter().map(|p| f64::from((p >> s) & 0xff)).sum::<f64>() / count;
    let luma: Vec<f64> =
        px.iter().map(|p| 0.299 * f64::from((p >> 16) & 0xff) + 0.587 * f64::from((p >> 8) & 0xff) + 0.114 * f64::from(p & 0xff)).collect();
    let clipped = luma.iter().filter(|l| **l < 2.0 || **l > 253.0).count() as f64 / count;
    let mean = luma.iter().sum::<f64>() / count;
    let sd = (luma.iter().map(|l| (l - mean) * (l - mean)).sum::<f64>() / count).sqrt();
    println!(
        "  middle frame {}x{}: mean R {:.1} G {:.1} B {:.1}, luma mean {:.1} sd {:.1}, {:.2}% clipped",
        w,
        hh,
        ch(16),
        ch(8),
        ch(0),
        mean,
        sd,
        clipped * 100.0
    );
    let rgb: Vec<u8> = px.iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8]).collect();
    image::save_buffer(out.join("middle.png"), &rgb, w as u32, hh as u32, image::ColorType::Rgb8).unwrap();
}
