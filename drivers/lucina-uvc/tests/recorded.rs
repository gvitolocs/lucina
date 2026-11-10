//! The laptop webcam's own configuration descriptor (Chicony "720p HD Camera", 04f2:b72f), as the kernel logged it on the
//! Galaxy Book Go on 2026-11-07, and what the driver makes of it. The stream it chose there, MJPEG 1280x720 at 30 fps on
//! alternate setting 11, ran at 30.25 fps with no frame in error.

use lucina_uvc::desc::{self, Intervals};
use lucina_uvc::probe;

const RAW: &[u8] = include_bytes!("data/chicony-04f2-b72f.desc");

#[test]
fn the_webcam_descriptor() {
    assert_eq!(RAW.len(), 1009);
    assert_eq!(u16::from_le_bytes([RAW[2], RAW[3]]) as usize, RAW.len());
    assert!(desc::is_video(RAW));
    let cam = desc::parse(RAW).unwrap();
    assert_eq!((cam.uvc_version, cam.clock_frequency, cam.control_interface), (0x0100, 48_000_000, 0));
    assert_eq!(probe::control_size(cam.uvc_version), 26);
    assert_eq!(cam.streaming.len(), 1);
    let s = &cam.streaming[0];
    assert_eq!((s.interface, s.endpoint), (1, 0x81));
    let names: Vec<&str> = s.formats.iter().map(|f| f.name()).collect();
    assert_eq!(names, ["YUY2", "MJPG"]);
    let sizes = |f: &desc::Format| f.frames.iter().map(|fr| (fr.width, fr.height)).collect::<Vec<_>>();
    let expected = [(640, 480), (160, 120), (320, 180), (320, 240), (352, 288), (640, 360), (848, 480), (960, 540), (1280, 720)];
    assert_eq!(sizes(&s.formats[0]), expected);
    assert_eq!(sizes(&s.formats[1]), expected);
    let mjpg = s.format(2).unwrap();
    let hd = mjpg.frame(9).unwrap();
    assert_eq!((hd.width, hd.height, hd.max_frame_size), (1280, 720, 1_843_200));
    assert_eq!(hd.intervals, Intervals::Discrete(vec![333_333, 666_666]));
    let yuy2_hd = s.format(1).unwrap().frames.last().unwrap();
    assert_eq!(yuy2_hd.intervals, Intervals::Discrete(vec![1_000_000]));
    for f in &s.formats {
        assert_eq!(f.colour, desc::Colour { primaries: 1, transfer: 1, matrix: 4 });
    }
    let alts: Vec<u32> = s.alts.iter().map(|a| a.bytes_per_interval).collect();
    assert_eq!(alts, [192, 384, 512, 640, 800, 944, 1280, 1600, 1984, 2880, 3060]);
    // The camera asked for 3060 bytes per microframe for MJPEG 1280x720 at 30 fps: the largest setting, 11.
    assert_eq!(probe::choose_alt(s, 3060).unwrap().alt, 11);
}
