use crate::preview::{draw_mark, photo_request};

const SAMPLE: &str = "camera: Chicony 720p HD Camera (04f2:b72f) at dwc3@a600000/dev3\n\
format 1 MJPG: 1280x720@30.00/15.00 640x480@30.00 (colour 1/1/4)\n\
format 2 YUY2: 640x480@30.00 1280x720@10.00 (colour 1/1/4)\n\
state: idle\ncapturing: no\n";

#[test]
fn photos_use_the_largest_mjpeg_size() {
    let info = lucina_camera::parse_info(SAMPLE);
    let r = photo_request(&info).unwrap();
    assert_eq!(
        (&r.fourcc, r.width, r.height, r.interval),
        (b"MJPG", 1280, 720, 333_333)
    );
    let none = lucina_camera::parse_info("camera: none\ncapturing: no\n");
    assert!(photo_request(&none).is_none());
}

#[test]
fn the_mark_is_a_red_dot_in_the_top_right() {
    let (w, h) = (640, 360);
    let mut px = vec![0u32; w * h];
    draw_mark(&mut px, w, h);
    let r = 6;
    assert_eq!(px[3 * r * w + (w - 3 * r)], 0x00e0_3030);
    assert_eq!(px[3 * r * w + (w - 3 * r) + r + 1], 0x00ff_ffff);
    assert_eq!(px[0], 0);
    assert_eq!(px[(h - 1) * w], 0);
    // Too small a buffer is left alone.
    let mut small = vec![7u32; 3];
    draw_mark(&mut small, w, h);
    assert_eq!(small, [7, 7, 7]);
}

#[test]
fn render_scales_to_the_screen_and_marks_the_picture() {
    use crate::preview::{Job, render};
    // A grey 4x2 YUY2 frame (Y = U = V = 128) for a 64x36 screen: fitted to 64x32, centred, mark in the top right.
    let job = Job {
        data: vec![128u8; 4 * 2 * 2],
        fourcc: *b"YUY2",
        width: 4,
        height: 2,
        matrix: lucina_camera::convert::Matrix::Bt601,
        screen: (64, 36),
        out: Vec::new(),
        yuv: None,
    };
    let (mut rgb, mut out) = (Vec::new(), Vec::new());
    assert_eq!(render(&job, &mut rgb, &mut out), Some((0, 2, 64, 32)));
    assert_eq!(out.len(), 64 * 32);
    let p = out[31 * 64];
    assert_eq!(p >> 24, 0);
    assert_eq!((p >> 16) & 0xff, p & 0xff);
    assert_eq!(out[18 * 64 + 46], 0x00e0_3030);
    // A JPEG that does not decode gives nothing to show.
    let bad = Job { data: vec![0xff, 0xd8, 0x00], fourcc: *b"MJPG", ..job };
    assert_eq!(render(&bad, &mut rgb, &mut out), None);
}

#[test]
fn the_gpu_mark_matches_the_cpu_mark() {
    use crate::gpu_show::{mark_pixels, mark_radius, mark_rect};
    // A 1440x810 picture at (240, 135): the CPU path's dot, drawn into the picture, and the GPU's, drawn over it, cover the same
    // screen pixels in the same colours.
    let (pw, ph) = (1440usize, 810usize);
    let r = mark_radius(pw, ph);
    assert_eq!(r, 13);
    let (side, px) = mark_pixels(r);
    assert_eq!(side, 2 * (r + 2) + 1);
    let to = mark_rect((240, 135, pw, ph), r);
    assert_eq!((to.w as usize, to.h as usize), (side, side));
    let mut cpu = vec![0u32; pw * ph];
    crate::preview::draw_mark(&mut cpu, pw, ph);
    for y in 0..side {
        for x in 0..side {
            let (sx, sy) = (to.x as usize + x - 240, to.y as usize + y - 135);
            let g = px[y * side + x];
            if g >> 24 == 0 {
                assert_eq!(cpu[sy * pw + sx], 0, "transparent where the CPU mark draws nothing ({x}, {y})");
            } else {
                assert_eq!(g & 0x00ff_ffff, cpu[sy * pw + sx], "same colour at ({x}, {y})");
            }
        }
    }
    // Nothing of the CPU mark lies outside the GPU mark's square.
    let drawn = cpu.iter().filter(|&&p| p != 0).count();
    let opaque = px.iter().filter(|&&p| p >> 24 != 0).count();
    assert_eq!(drawn, opaque);
}

#[test]
fn planes_are_copied_to_the_gpu_layout() {
    use lucina_app::gpu::YuvLayout;
    use lucina_camera::convert::I420;
    let (w, h) = (5usize, 3usize);
    let mut p = I420::default();
    p.width = w;
    p.height = h;
    p.y = (0..(w * h) as u8).collect();
    p.u = (100..106).collect();
    p.v = (200..206).collect();
    let layout = YuvLayout::packed(w as u32, h as u32, false);
    let mut out = vec![0xEEu8; layout.bytes()];
    assert!(crate::gpu_show::copy_planes(&p, &layout, &mut out));
    for y in 0..h {
        assert_eq!(&out[layout.y_offset + y * layout.y_pitch..][..w], &p.y[y * w..][..w]);
    }
    for y in 0..2 {
        assert_eq!(&out[layout.c_offset + y * layout.c_pitch..][..3], &p.u[y * 3..][..3]);
        assert_eq!(&out[layout.v_offset + y * layout.c_pitch..][..3], &p.v[y * 3..][..3]);
    }
    // Padding is left as it was.
    assert_eq!(out[layout.y_offset + w], 0xEE);
    // A short chroma plane, or a buffer too small for the layout, copies nothing.
    let mut short = p.clone();
    short.v.truncate(5);
    let mut fresh = vec![0xEEu8; layout.bytes()];
    assert!(!crate::gpu_show::copy_planes(&short, &layout, &mut fresh));
    assert!(fresh.iter().all(|&b| b == 0xEE));
    let mut small = vec![0xEEu8; layout.bytes() - 1];
    assert!(!crate::gpu_show::copy_planes(&p, &layout, &mut small));
    assert!(small.iter().all(|&b| b == 0xEE));
}
