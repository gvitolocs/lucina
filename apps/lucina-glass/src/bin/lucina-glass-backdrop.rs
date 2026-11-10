//! lucina-glass-backdrop: writes the home dashboard's backdrop and its blurred copy for a panel size as raw little-endian XRGB
//! pixels. The shell makes the sky itself at start; files written by this tool into the image (/usr/share/lucina/home) replace it,
//! for a picture other than the generated sky. Not part of the image build
//! (scripts/gui-initramfs.sh puts it in /usr/share/lucina/home). Made on the build host so the laptop's first frame copies it
//! instead of spending most of a second generating it.
//!
//! ```text
//! lucina-glass-backdrop <dir> [WxH...]     # default 1920x1080
//! ```
use std::io::Write;

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(dir) = args.next() else {
        eprintln!("usage: lucina-glass-backdrop <dir> [WxH...]");
        std::process::exit(2);
    };
    let mut sizes: Vec<(u32, u32)> = args.filter_map(|a| a.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))).collect();
    if sizes.is_empty() {
        sizes.push((1920, 1080));
    }
    std::fs::create_dir_all(&dir).expect("output folder");
    for (w, h) in sizes {
        // The same scale the shell uses: the 1280 x 720 dp dashboard fitted into the panel.
        let k = (w as f32 / lucina_glass::home::W).min(h as f32 / lucina_glass::home::H);
        let img = lucina_glass::imagery::night_sky(w, h, k, 7);
        let blurred = lucina_glass::home::quick_blur(&img);
        let (a, b) = lucina_glass::home::backdrop_names(w, h);
        for (name, c) in [(a, &img), (b, &blurred)] {
            let bytes = lucina_glass::home::raw_bytes(c);
            let path = format!("{dir}/{name}");
            let mut f = std::fs::File::create(&path).expect("backdrop file");
            f.write_all(&bytes).expect("backdrop written");
            println!("{path}: {} bytes", bytes.len());
        }
    }
}
