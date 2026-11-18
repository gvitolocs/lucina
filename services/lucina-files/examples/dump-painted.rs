//! The first half of the recipe of `apps/lucina-app-gallery/tests/fixtures`: the twelve pictures `Fake`
//! paints for the imported library, as PNG, ready for a JPEG encoder. Kept so that the fixtures can be made
//! again from what is in the tree; the sizes are the server's two shapes, 320x240 and 960x720, and the
//! portrait `4` turned the same way.
//!
//! `cargo run -p lucina-files --example dump-painted -- <folder>`

use lucina_files::fake::painted;

fn main() {
    let dir = std::env::args()
        .nth(1)
        .expect("a folder to write the PNGs in");
    std::fs::create_dir_all(&dir).expect("make the folder");
    for i in 0..6u32 {
        let (small, large) = if i == 4 {
            ((240, 320), (720, 960))
        } else {
            ((320, 240), (960, 720))
        };
        for (tag, (w, h)) in [("s", small), ("l", large)] {
            let bytes = painted(i, w, h);
            let p = std::path::Path::new(&dir).join(format!("{i}-{tag}.png"));
            std::fs::write(&p, &bytes).expect("write a PNG");
            println!("{} {w}x{h} {} bytes", p.display(), bytes.len());
        }
    }
}
