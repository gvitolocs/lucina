//! Bakes SecretaryIOS's item illustrations into the one file this app ships: no SVG renderer on Juno, and no file per picture.
//!
//! `cargo run -p lucina-app-shopping --example bake_art -- <SecretaryIOS>/assets/icons/items apps/lucina-app-shopping/art/items.bin`
//!
//! The format: for each picture, in name order, a one-byte name length, the catalog id, a four-byte little-endian length, a PNG.

use resvg::{tiny_skia, usvg};

/// The size on the laptop's panel, where a tile's picture is 40 reference pixels at 1.4 to one.
const SIZE: u32 = 56;

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = args.next().expect("the items directory");
    let out = args.next().expect("the output file");
    let mut names: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
        .expect("read")
        .map(|e| e.expect("entry").path())
        .collect();
    names.retain(|p| p.extension().is_some_and(|e| e == "svg"));
    names.sort();
    let mut blob = Vec::new();
    for p in &names {
        let id = p.file_stem().and_then(|s| s.to_str()).expect("a name");
        let tree =
            usvg::Tree::from_data(&std::fs::read(p).expect("svg"), &usvg::Options::default())
                .expect("parse");
        let mut map = tiny_skia::Pixmap::new(SIZE, SIZE).expect("pixmap");
        let k = SIZE as f32 / tree.size().width().max(tree.size().height());
        resvg::render(
            &tree,
            tiny_skia::Transform::from_scale(k, k),
            &mut map.as_mut(),
        );
        let png = map.encode_png().expect("png");
        blob.push(id.len() as u8);
        blob.extend_from_slice(id.as_bytes());
        blob.extend_from_slice(&(png.len() as u32).to_le_bytes());
        blob.extend_from_slice(&png);
    }
    std::fs::write(&out, &blob).expect("write");
    println!("{} pictures, {} bytes", names.len(), blob.len());
}
