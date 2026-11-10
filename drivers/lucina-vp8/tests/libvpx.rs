//! The real test: what the encoder writes, decoded by libvpx through ffmpeg, must equal the encoder's own reconstruction, bit for bit.
//! A difference anywhere shows up as drift, and drift is the bug a remote desktop can't recover from without a key frame.
//!
//! Skipped with a message where ffmpeg has no libvpx.

use std::io::Write;
use std::process::{Command, Stdio};

use lucina_vp8::{Encoder, Yuv};

const W: usize = 368;
const H: usize = 214;

struct Scene {
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    seed: u32,
}

impl Scene {
    fn new() -> Scene {
        let mut y = vec![0u8; W * H];
        for r in 0..H {
            for c in 0..W {
                y[r * W + c] = ((r * 2 + c) % 200 + 16) as u8;
            }
        }
        let (cw, ch) = (W.div_ceil(2), H.div_ceil(2));
        Scene { y, u: vec![90; cw * ch], v: vec![200; cw * ch], seed: 7 }
    }

    fn rnd(&mut self) -> u32 {
        self.seed = self.seed.wrapping_mul(1664525).wrapping_add(1013904223);
        self.seed >> 16
    }

    /// Text-like speckle and solid rectangles in a few places, as a desktop changes. Returns which macroblocks it touched.
    fn change(&mut self, changed: &mut [bool], mb_w: usize) {
        changed.iter_mut().for_each(|c| *c = false);
        let (cw, _ch) = (W.div_ceil(2), H.div_ceil(2));
        for _ in 0..4 {
            let (x0, y0) = ((self.rnd() as usize) % (W - 40), (self.rnd() as usize) % (H - 40));
            let (w, h) = (8 + (self.rnd() as usize) % 32, 8 + (self.rnd() as usize) % 32);
            let solid = self.rnd() % 2 == 0;
            let level = 16 + (self.rnd() % 219) as u8;
            for r in y0..y0 + h {
                for c in x0..x0 + w {
                    let v = if solid { level } else { 16 + (self.rnd() % 220) as u8 };
                    self.y[r * W + c] = v;
                }
            }
            let (ux, uy) = (x0 / 2, y0 / 2);
            let cv = (self.rnd() % 255) as u8;
            for r in uy..uy + h / 2 {
                for c in ux..ux + w / 2 {
                    self.u[r * cw + c] = cv;
                    self.v[r * cw + c] = 255 - cv;
                }
            }
            for r in y0 / 16..=(y0 + h) / 16 {
                for c in (x0 / 16).saturating_sub(0)..=(x0 + w) / 16 {
                    if c < mb_w && r * mb_w + c < changed.len() {
                        changed[r * mb_w + c] = true;
                    }
                }
            }
        }
    }
}

fn ivf_header(w: usize, h: usize, frames: u32) -> Vec<u8> {
    let mut b = b"DKIF\0\0\x20\0VP80".to_vec();
    b.extend_from_slice(&(w as u16).to_le_bytes());
    b.extend_from_slice(&(h as u16).to_le_bytes());
    b.extend_from_slice(&30u32.to_le_bytes());
    b.extend_from_slice(&1u32.to_le_bytes());
    b.extend_from_slice(&frames.to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    b
}

/// The decoded frames as raw I420, tightly packed.
fn decode(ivf: &[u8], decoder: &str) -> Option<Vec<u8>> {
    let mut child = Command::new("ffmpeg")
        .args(["-loglevel", "error", "-c:v", decoder, "-f", "ivf", "-i", "pipe:0", "-f", "rawvideo", "-pix_fmt", "yuv420p", "-fps_mode", "passthrough", "pipe:1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let mut stdin = child.stdin.take()?;
    let data = ivf.to_vec();
    let t = std::thread::spawn(move || {
        let _ = stdin.write_all(&data);
    });
    let out = child.wait_with_output().ok()?;
    let _ = t.join();
    if !out.status.success() {
        panic!("{decoder} refused the stream: {}", String::from_utf8_lossy(&out.stderr));
    }
    Some(out.stdout)
}

fn has_decoder(name: &str) -> bool {
    Command::new("ffmpeg").args(["-hide_banner", "-decoders"]).output().map(|o| String::from_utf8_lossy(&o.stdout).contains(name)).unwrap_or(false)
}

/// One frame of the reconstruction, cropped to the picture, as ffmpeg writes it.
fn recon(e: &Encoder) -> Vec<u8> {
    let (y, u, v) = e.reconstruction();
    let (ys, uvs) = e.strides();
    let (cw, ch) = (W.div_ceil(2), H.div_ceil(2));
    let mut out = Vec::with_capacity(W * H * 3 / 2);
    for r in 0..H {
        out.extend_from_slice(&y[r * ys..][..W]);
    }
    for p in [u, v] {
        for r in 0..ch {
            out.extend_from_slice(&p[r * uvs..][..cw]);
        }
    }
    out
}

fn check_against(decoder: &str) {
    let mut scene = Scene::new();
    let mut e = Encoder::new(W, H);
    let (mw, mh) = e.macroblocks();
    let mut changed = vec![false; mw * mh];
    let mut ivf = ivf_header(W, H, 40);
    let mut want = Vec::new();
    let mut out = Vec::new();
    let mut coded = 0;
    for n in 0..40u32 {
        if n > 0 {
            scene.change(&mut changed, mw);
        }
        // A varying quantizer, a key frame in the middle, and a frame with no change now and then.
        let q = [4u8, 20, 40, 80, 120][n as usize % 5];
        if n % 7 == 3 {
            changed.iter_mut().for_each(|c| *c = false);
        }
        let pic = Yuv { y: &scene.y, u: &scene.u, v: &scene.v, y_stride: W, uv_stride: W.div_ceil(2) };
        let info = e.encode(&pic, &changed, q, n == 20, if n % 4 == 0 { 64 } else { 0 }, &mut out);
        coded += info.coded;
        ivf.extend_from_slice(&(out.len() as u32).to_le_bytes());
        ivf.extend_from_slice(&(n as u64).to_le_bytes());
        ivf.extend_from_slice(&out);
        want.extend_from_slice(&recon(&e));
    }
    assert!(coded > 200, "the scene should have coded something: {coded}");
    let got = decode(&ivf, decoder).expect("ffmpeg runs");
    let frame = W * H + 2 * W.div_ceil(2) * H.div_ceil(2);
    assert_eq!(got.len(), want.len(), "{} frames against {}", got.len() / frame, want.len() / frame);
    for (n, (g, w)) in got.chunks(frame).zip(want.chunks(frame)).enumerate() {
        if g != w {
            let at = g.iter().zip(w).position(|(a, b)| a != b).unwrap();
            panic!("frame {n} differs from {decoder} first at byte {at}: {} against {}", g[at], w[at]);
        }
    }
}

#[test]
fn libvpx_decodes_exactly_what_we_reconstruct() {
    if !has_decoder("libvpx") {
        eprintln!("skipped: ffmpeg has no libvpx decoder");
        return;
    }
    check_against("libvpx");
}

#[test]
fn ffmpegs_own_decoder_agrees() {
    if !has_decoder("vp8") {
        eprintln!("skipped: ffmpeg has no vp8 decoder");
        return;
    }
    check_against("vp8");
}

#[test]
fn the_picture_is_close_to_the_source_at_a_fine_quantizer() {
    let scene = Scene::new();
    let mut e = Encoder::new(W, H);
    let (mw, mh) = e.macroblocks();
    let pic = Yuv { y: &scene.y, u: &scene.u, v: &scene.v, y_stride: W, uv_stride: W.div_ceil(2) };
    let mut out = Vec::new();
    e.encode(&pic, &vec![true; mw * mh], 4, true, 0, &mut out);
    let (y, _, _) = e.reconstruction();
    let (ys, _) = e.strides();
    let mut worst = 0;
    for r in 0..H {
        for c in 0..W {
            worst = worst.max((y[r * ys + c] as i32 - scene.y[r * W + c] as i32).abs());
        }
    }
    assert!(worst <= 8, "worst difference {worst}");
}

#[test]
fn an_unchanged_frame_costs_almost_nothing() {
    let scene = Scene::new();
    let mut e = Encoder::new(1920, 1080);
    let (mw, mh) = e.macroblocks();
    let y = vec![50u8; 1920 * 1080];
    let c = vec![128u8; 960 * 540];
    let pic = Yuv { y: &y, u: &c, v: &c, y_stride: 1920, uv_stride: 960 };
    let mut out = Vec::new();
    e.encode(&pic, &vec![true; mw * mh], 20, true, 0, &mut out);
    let key = out.len();
    let info = e.encode(&pic, &vec![false; mw * mh], 20, false, 0, &mut out);
    assert_eq!(info.coded, 0);
    assert!(out.len() < 200, "an idle frame of {} bytes after a key frame of {key}", out.len());
    let _ = scene;
}
