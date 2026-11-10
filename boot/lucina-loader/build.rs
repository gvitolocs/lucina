//! Rasterizes the theme's four faces of DejaVu Sans (lucinaos `boards/gbgo/theme/theme.txt`: Bold 48, Regular 20,
//! Bold 20, Regular 16) for printable ASCII into 8-bit coverage maps that `src/font.rs` includes. The TTF files are
//! the ones lucinaos' `lib/theme.sh` turns into GRUB fonts: `$LUCINA_LOADER_FONTS`, by default
//! `/usr/share/fonts/truetype/dejavu` (Debian's fonts-dejavu-core). DejaVu's licence allows embedding them.

use std::{env, fmt::Write, fs, path::PathBuf};

/// (name in `src/font.rs`, file, pixel size)
const FACES: [(&str, &str, f32); 4] = [
    ("BOLD48", "DejaVuSans-Bold.ttf", 48.0),
    ("REGULAR20", "DejaVuSans.ttf", 20.0),
    ("BOLD20", "DejaVuSans-Bold.ttf", 20.0),
    ("REGULAR16", "DejaVuSans.ttf", 16.0),
];

fn main() {
    println!("cargo:rerun-if-env-changed=LUCINA_LOADER_FONTS");
    println!("cargo:rerun-if-changed=build.rs");
    let dir = PathBuf::from(
        env::var("LUCINA_LOADER_FONTS")
            .unwrap_or_else(|_| "/usr/share/fonts/truetype/dejavu".into()),
    );
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let mut rs = String::new();
    for (name, file, px) in FACES {
        let path = dir.join(file);
        println!("cargo:rerun-if-changed={}", path.display());
        let ttf = fs::read(&path).unwrap_or_else(|e| {
            panic!(
                "{}: {e} (install fonts-dejavu-core or set LUCINA_LOADER_FONTS)",
                path.display()
            )
        });
        let settings = fontdue::FontSettings {
            scale: px,
            ..Default::default()
        };
        let font = fontdue::Font::from_bytes(ttf, settings).expect("a TrueType font");
        let mut coverage = Vec::new();
        let ascent = font.metrics('l', px);
        writeln!(rs, "pub static {name}: Font = Font {{").unwrap();
        writeln!(rs, "    ascent: {},", ascent.ymin + ascent.height as i32).unwrap();
        writeln!(rs, "    glyphs: &[").unwrap();
        for c in ' '..='~' {
            let (m, cov) = font.rasterize(c, px);
            writeln!(
                rs,
                "        Glyph {{ w: {}, h: {}, left: {}, top: {}, advance: {}, at: {} }},",
                m.width,
                m.height,
                m.xmin,
                m.ymin + m.height as i32,
                (m.advance_width * 64.0).round() as u32,
                coverage.len()
            )
            .unwrap();
            coverage.extend_from_slice(&cov);
        }
        let bin = format!("{}.bin", name.to_lowercase());
        fs::write(out.join(&bin), &coverage).unwrap();
        writeln!(rs, "    ],").unwrap();
        writeln!(
            rs,
            "    coverage: include_bytes!(concat!(env!(\"OUT_DIR\"), \"/{bin}\")),"
        )
        .unwrap();
        writeln!(rs, "}};").unwrap();
    }
    fs::write(out.join("fonts.rs"), rs).unwrap();
}
