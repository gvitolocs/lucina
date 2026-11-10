//! A recorded frame from the laptop's webcam (Chicony 04f2:b72f, YUY2 640x480, 2026-11-07): its top 48 rows, the room's
//! ceiling, cut from the first frame of a `/dev/lucina-camera0` recording. A white ceiling under daylight must come out as a
//! light, neutral grey, not tinted: the conversion's matrix, range and byte order all show in that.

use lucina_camera::convert::{self, Matrix};

const CEILING: &[u8] = include_bytes!("data/yuy2-640x48-ceiling.raw");

fn means(px: &[u32]) -> (f64, f64, f64) {
    let n = px.len() as f64;
    let c = |s: u32| px.iter().map(|p| f64::from((p >> s) & 0xff)).sum::<f64>() / n;
    (c(16), c(8), c(0))
}

#[test]
fn the_recorded_ceiling_is_a_neutral_grey() {
    let mut px = Vec::new();
    convert::yuy2_to_xrgb(CEILING, 640, 48, Matrix::Bt601, &mut px);
    assert_eq!(px.len(), 640 * 48);
    let (r, g, b) = means(&px);
    eprintln!("ceiling mean R {r:.1} G {g:.1} B {b:.1}");
    for v in [r, g, b] {
        assert!((40.0..220.0).contains(&v), "{r} {g} {b}");
    }
    assert!((r - g).abs() < 12.0 && (g - b).abs() < 12.0 && (r - b).abs() < 12.0, "tinted: {r} {g} {b}");
}

