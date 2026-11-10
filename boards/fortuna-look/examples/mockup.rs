//! The design mockup: every screen pair drawn by the real crate (generated palette and Glass materials, the real faces
//! and icons, the real layout) into 256x192 BGR555 frames, written as PNGs the way the DSi holds them: the top screen
//! above the bottom, with the hinge between.
//!
//! cargo run -p fortuna-look --features sample --example mockup -- <out dir>
//! Writes <name>.png at 1x and <name>@3x.png (nearest neighbour) for each pair in `sample::SCREENS`.

use std::path::{Path, PathBuf};

use fortuna_look::canvas::Canvas;
use fortuna_look::layout::{SCREEN_H, SCREEN_W};
use fortuna_look::sample::{self, H, W};
use fortuna_look::skin;
use fortuna_look::tokens::rgb888;

const HINGE: usize = 16;

fn write_pair(dir: &Path, name: &str, top: &[u16], bottom: &[u16]) {
    let total_h = H * 2 + HINGE;
    let mut rgb = vec![0u8; W * total_h * 3];
    let put = |rgb: &mut Vec<u8>, y0: usize, f: &[u16]| {
        for y in 0..H {
            for x in 0..W {
                let v = rgb888(f[y * W + x]);
                let i = ((y0 + y) * W + x) * 3;
                rgb[i] = (v >> 16) as u8;
                rgb[i + 1] = (v >> 8) as u8;
                rgb[i + 2] = v as u8;
            }
        }
    };
    put(&mut rgb, 0, top);
    // the hinge, as in lucina-glass layouts/fortuna.rs: a grey band with a small handle in the middle
    let (h, k) = (skin::HINGE, [0x55u8, 0x56, 0x5c]);
    let band = [h.r, h.g, h.b];
    for y in H..H + HINGE {
        for x in 0..W {
            let i = (y * W + x) * 3;
            let handle = (118..138).contains(&x) && (y == H + HINGE / 2 - 1 || y == H + HINGE / 2);
            rgb[i..i + 3].copy_from_slice(if handle { &k } else { &band });
        }
    }
    put(&mut rgb, H + HINGE, bottom);
    save(&dir.join(format!("{name}.png")), W, total_h, &rgb);
    let s = 3;
    let mut big = vec![0u8; W * s * total_h * s * 3];
    for y in 0..total_h * s {
        for x in 0..W * s {
            let (i, j) = ((y * W * s + x) * 3, ((y / s) * W + x / s) * 3);
            big[i..i + 3].copy_from_slice(&rgb[j..j + 3]);
        }
    }
    save(&dir.join(format!("{name}@3x.png")), W * s, total_h * s, &big);
}

fn save(path: &Path, w: usize, h: usize, rgb: &[u8]) {
    let f = std::fs::File::create(path).expect("create png");
    let mut e = png::Encoder::new(std::io::BufWriter::new(f), w as u32, h as u32);
    e.set_color(png::ColorType::Rgb);
    e.set_depth(png::BitDepth::Eight);
    e.write_header().expect("png header").write_image_data(rgb).expect("png data");
    println!("{}", path.display());
}

fn main() {
    let dir = PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| "mockup".into()));
    std::fs::create_dir_all(&dir).expect("out dir");
    for name in sample::SCREENS {
        let (mut tp, mut bp) = (vec![0x8000u16; W * H], vec![0x8000u16; W * H]);
        {
            let mut t = Canvas::new(&mut tp, SCREEN_W, SCREEN_H, SCREEN_W);
            let mut b = Canvas::new(&mut bp, SCREEN_W, SCREEN_H, SCREEN_W);
            sample::render(name, &mut t, &mut b);
        }
        write_pair(&dir, name, &tp, &bp);
    }
}
