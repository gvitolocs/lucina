//! The live preview on the whole screen, and photos.
//!
//! The preview stream is `lucina_camera::choose_preview` (MJPEG 1280x720 at 30 fps on this camera). A photo is the camera's own
//! JPEG of the largest MJPEG size, written as it came (with the standard Huffman tables put in, which UVC streams leave out):
//! no second compression. When the preview runs another format or size, the stream switches for the photo and back.
//!
//! Frames are decoded on their own thread, pinned to the Gold cores. With `LUCINA_CAMERA_GPU=1` an MJPEG frame is decoded straight
//! to I420 (no colour conversion on the CPU) and the GPU converts and scales it ([`crate::gpu_show`]); otherwise, and for YUY2, the
//! thread converts to XRGB and scales on the CPU.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TryRecvError, sync_channel};
use std::time::{Duration, Instant};

use lucina_app::app::FullScreen;
use lucina_app::hold::DisplayHold;
use lucina_app::input::{EV_KEY, EV_SYN, SYN_DROPPED};
use lucina_app::keys;
use lucina_camera::affinity;
use lucina_camera::convert::{self, I420, Matrix};
use lucina_camera::{Camera, Frame, Info, StreamRequest, photo};

use crate::gpu_show::GpuShow;
use crate::{Outcome, Start};

const KEY_SPACE: u16 = 57;
/// No frame for this long: the stream is dead.
const NO_FRAMES: Duration = Duration::from_secs(5);
/// Frames are dropped this long after a stream starts, before a photo: the camera's exposure settles first.
const SETTLE: Duration = Duration::from_millis(800);
/// How often the frame rate goes to the log.
const REPORT_EVERY: Duration = Duration::from_secs(5);
pub(crate) const MARK_RED: u32 = 0x00e0_3030;
pub(crate) const WHITE: u32 = 0x00ff_ffff;

/// The camera-on mark: a red dot with a white ring in the top right corner of the picture, as the shell's bar would show it.
pub fn draw_mark(px: &mut [u32], w: usize, h: usize) {
    if w == 0 || h == 0 || px.len() < w * h {
        return;
    }
    let r = (w.min(h) / 60).max(6) as i64;
    let (cx, cy) = (w as i64 - 3 * r, 3 * r);
    let ring = r + 2;
    for y in (cy - ring).max(0)..(cy + ring + 1).min(h as i64) {
        for x in (cx - ring).max(0)..(cx + ring + 1).min(w as i64) {
            let d2 = (x - cx) * (x - cx) + (y - cy) * (y - cy);
            if d2 <= r * r {
                px[y as usize * w + x as usize] = MARK_RED;
            } else if d2 <= ring * ring {
                px[y as usize * w + x as usize] = WHITE;
            }
        }
    }
}

/// The MJPEG offer with the most pixels, at its highest rate: what a photo is taken with.
pub fn photo_request(info: &Info) -> Option<StreamRequest> {
    let o = info
        .offers
        .iter()
        .filter(|o| &o.fourcc == b"MJPG")
        .max_by_key(|o| u32::from(o.width) * u32::from(o.height))?;
    let fps = o.fps.iter().copied().fold(0.0f32, f32::max);
    Some(StreamRequest {
        fourcc: o.fourcc,
        width: o.width,
        height: o.height,
        interval: lucina_camera::interval_for_fps(if fps > 0.0 { fps } else { 30.0 }),
    })
}

fn matrix(info: &Info, fourcc: [u8; 4]) -> Matrix {
    info.matrix_by_fourcc
        .iter()
        .find(|(f, _)| *f == fourcc)
        .map_or(Matrix::Bt601, |(_, m)| *m)
}

fn stamp() -> String {
    chrono::Local::now().format("%Y%m%d_%H%M%S").to_string()
}

struct Shown {
    started: Instant,
    frames: u64,
    window_start: Instant,
    window_frames: u64,
    convert_us: u64,
    upload_us: u64,
    show_us: u64,
    /// The process's CPU time at the window's start, for its share of one core.
    cpu_at_start: Duration,
    /// Where the converting thread runs.
    cpu: Option<usize>,
}

/// User plus system time of the whole process so far.
fn cpu_time() -> Duration {
    // SAFETY: `getrusage` writes one `rusage` into the zeroed value it is given and reads nothing else.
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: as above; `RUSAGE_SELF` with a valid pointer.
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) } != 0 {
        return Duration::ZERO;
    }
    let tv = |t: libc::timeval| Duration::new(t.tv_sec as u64, t.tv_usec as u32 * 1000);
    tv(ru.ru_utime) + tv(ru.ru_stime)
}

/// What a key asked for.
enum Ask {
    Nothing,
    Photo,
    Leave,
}

fn keys_pressed(fs: &mut FullScreen) -> Ask {
    let mut evs = Vec::new();
    fs.inputs.rescan(Instant::now());
    fs.inputs.read(&mut evs);
    let mut ask = Ask::Nothing;
    for (_, ev) in evs {
        if ev.type_ == EV_SYN && ev.code == SYN_DROPPED {
            continue;
        }
        if ev.type_ != EV_KEY || ev.value != 1 {
            continue;
        }
        match ev.code {
            keys::KEY_ESC => return Ask::Leave,
            KEY_SPACE | keys::KEY_ENTER => ask = Ask::Photo,
            _ => {}
        }
    }
    ask
}

/// One frame for the converting thread: its bytes, what they are, the screen it is for, and the buffer to scale into. With `yuv`
/// (an MJPEG frame for the GPU) the thread decodes into that instead and leaves `out` alone.
pub struct Job {
    pub data: Vec<u8>,
    pub fourcc: [u8; 4],
    pub width: usize,
    pub height: usize,
    pub matrix: Matrix,
    pub screen: (usize, usize),
    pub out: Vec<u32>,
    pub yuv: Option<I420>,
}

/// A converted frame: `out` holds `rect`'s pixels (x, y, w, h on the screen), mark drawn, or `yuv` the decoded planes (`rect` is
/// then the picture's own size at 0, 0); `rect` is `None` for a frame that did not decode. `us` is the time it took, `cpu` where.
pub struct Done {
    pub out: Vec<u32>,
    pub yuv: Option<I420>,
    pub rect: Option<(usize, usize, usize, usize)>,
    pub us: u64,
    pub cpu: Option<usize>,
}

/// Converts a frame to XRGB, scales it to fit the screen and draws the camera-on mark. `None` for a frame that does not decode.
pub fn render(
    job: &Job,
    rgb: &mut Vec<u32>,
    out: &mut Vec<u32>,
) -> Option<(usize, usize, usize, usize)> {
    let (w, h) = if &job.fourcc == b"MJPG" {
        convert::jpeg_to_xrgb(&job.data, rgb).ok()?
    } else {
        convert::yuy2_to_xrgb(&job.data, job.width, job.height, job.matrix, rgb);
        if rgb.is_empty() {
            return None;
        }
        (job.width, job.height)
    };
    let (x, y, dw, dh) = convert::fit(w, h, job.screen.0, job.screen.1);
    convert::scale_xrgb(rgb, w, h, dw, dh, out);
    if out.is_empty() {
        return None;
    }
    draw_mark(out, dw, dh);
    Some((x, y, dw, dh))
}

/// Decodes an MJPEG frame into `yuv`: `Some((0, 0, w, h))`, or `None` for a frame that does not decode.
pub fn decode_yuv(job: &Job, yuv: &mut I420) -> Option<(usize, usize, usize, usize)> {
    let (w, h) = convert::jpeg_to_i420(&job.data, yuv).ok()?;
    Some((0, 0, w, h))
}

/// The converting thread: frames in, pictures out, until the job channel closes. The preview shows one picture while this makes
/// the next. It runs on the fastest CPUs (the Gold cores), where a JPEG decodes in about half the time.
fn converter(jobs: Receiver<Job>, done: SyncSender<Done>) {
    // LUCINA_CAMERA_CPUS (a CPU list such as "1" or "0-1") overrides the choice, for measuring.
    let gold = std::env::var("LUCINA_CAMERA_CPUS")
        .ok()
        .and_then(|l| affinity::parse_cpu_list(&l))
        .unwrap_or_else(affinity::fastest_cpus);
    match affinity::pin_current_thread(&gold) {
        Ok(()) => eprintln!("lucina-app-camera: decoding on CPUs {gold:?}"),
        Err(e) => {
            eprintln!("lucina-app-camera: decoding on any CPU, pinning to {gold:?} failed: {e}")
        }
    }
    let mut rgb = Vec::new();
    while let Ok(mut job) = jobs.recv() {
        let t = Instant::now();
        let mut out = std::mem::take(&mut job.out);
        let mut yuv = job.yuv.take();
        let rect = match yuv.as_mut() {
            Some(p) if &job.fourcc == b"MJPG" => decode_yuv(&job, p),
            _ => render(&job, &mut rgb, &mut out),
        };
        let us = t.elapsed().as_micros() as u64;
        let cpu = affinity::current_cpu();
        if done
            .send(Done {
                out,
                yuv,
                rect,
                us,
                cpu,
            })
            .is_err()
        {
            break;
        }
    }
}

/// Puts a converted frame on the screen, after a black screen the first time (the picture may not cover it all).
fn present(
    fs: &mut FullScreen,
    rect: (usize, usize, usize, usize),
    out: &[u32],
    cleared: &mut bool,
) {
    let (sw, sh) = (fs.screen.fb.width as usize, fs.screen.fb.height as usize);
    if !*cleared {
        let black = vec![0u32; sw];
        for y in 0..sh {
            fs.screen.fb.present_image(0, y as i32, sw, &black);
        }
        *cleared = true;
    }
    let (x, y, dw, _) = rect;
    fs.screen.fb.present_image(x as i32, y as i32, dw, out);
}

/// Waits for a whole, decodable MJPEG frame of `req`'s size, after the exposure has settled. `None` after `NO_FRAMES`.
fn grab_photo(
    cam: &mut Camera,
    req: &StreamRequest,
    since: Instant,
) -> Result<Option<Frame>, String> {
    let deadline = Instant::now() + SETTLE + NO_FRAMES;
    let mut scratch = Vec::new();
    while Instant::now() < deadline {
        let Some(f) = cam
            .next_frame(Some(Duration::from_millis(200)))
            .map_err(|e| e.to_string())?
        else {
            continue;
        };
        if since.elapsed() < SETTLE
            || f.header.error()
            || f.header.fourcc != req.fourcc
            || f.header.width != req.width
        {
            continue;
        }
        if convert::jpeg_to_xrgb(&f.data, &mut scratch).is_ok() {
            return Ok(Some(f));
        }
    }
    Ok(None)
}

fn take_photo(
    cam: &mut Camera,
    running: &StreamRequest,
    photo: &StreamRequest,
    last: Option<&Frame>,
    dir: &Path,
) -> Result<PathBuf, String> {
    // The preview's own frame when it is already the photo stream and whole.
    if let Some(f) = last.filter(|f| {
        f.header.fourcc == photo.fourcc && f.header.width == photo.width && !f.header.error()
    }) {
        return photo::save_jpeg(dir, &stamp(), &f.data).map_err(|e| e.to_string());
    }
    let switch = running != photo;
    if switch {
        cam.start(*photo).map_err(|e| e.to_string())?;
    }
    let got = grab_photo(
        cam,
        photo,
        if switch {
            Instant::now()
        } else {
            Instant::now() - SETTLE
        },
    );
    if switch {
        cam.start(*running).map_err(|e| e.to_string())?;
    }
    match got? {
        Some(f) => photo::save_jpeg(dir, &stamp(), &f.data).map_err(|e| e.to_string()),
        None => Err(String::from("no whole frame for the photo")),
    }
}

pub fn run(fs: &mut FullScreen, start: Start, dir: &Path, hold: &Mutex<DisplayHold>) -> Outcome {
    let mut outcome = Outcome::default();
    let fail = |mut o: Outcome, e: String| {
        eprintln!("lucina-app-camera: {e}");
        o.error = Some(e);
        o
    };
    let info = match lucina_camera::read_info() {
        Ok(i) => i,
        Err(e) => {
            return fail(
                outcome,
                format!(
                    "{} does not read ({e}): this kernel has no camera",
                    lucina_camera::PROC
                ),
            );
        }
    };
    let (Some(preview), Some(photo_req)) =
        (lucina_camera::choose_preview(&info), photo_request(&info))
    else {
        return fail(outcome, String::from("no camera on the bus"));
    };
    let mut cam = match Camera::open() {
        Ok(c) => c,
        Err(e) if e.raw_os_error() == Some(libc::EBUSY) => {
            return fail(outcome, String::from("another program has the camera"));
        }
        Err(e) => return fail(outcome, format!("{}: {e}", lucina_camera::DEVICE)),
    };
    let running = if start == Start::Photo {
        photo_req
    } else {
        preview
    };
    if let Err(e) = cam.start(running) {
        return fail(outcome, format!("start: {e}"));
    }
    if let Ok(mut h) = hold.lock() {
        // The shell names the app before the reason ("screen held on by Camera: preview").
        h.set(true, "preview");
    }
    eprintln!(
        "lucina-app-camera: {} {}x{} interval {}",
        String::from_utf8_lossy(&running.fourcc),
        running.width,
        running.height,
        running.interval
    );
    let m = matrix(&info, running.fourcc);
    let now = Instant::now();
    let mut shown = Shown {
        started: now,
        frames: 0,
        window_start: now,
        window_frames: 0,
        convert_us: 0,
        upload_us: 0,
        show_us: 0,
        cpu_at_start: cpu_time(),
        cpu: None,
    };
    let mut gpu = if &running.fourcc == b"MJPG" {
        GpuShow::open()
    } else {
        None
    };
    eprintln!(
        "lucina-app-camera: {}",
        if gpu.is_some() {
            "decoding to I420, the GPU converts and scales"
        } else {
            "converting and scaling on the CPU"
        }
    );
    let screen = (fs.screen.fb.width as usize, fs.screen.fb.height as usize);
    let (job_tx, job_rx) = sync_channel::<Job>(1);
    let (done_tx, done_rx) = sync_channel::<Done>(1);
    let worker = match std::thread::Builder::new()
        .name(String::from("camera-convert"))
        .spawn(move || converter(job_rx, done_tx))
    {
        Ok(w) => w,
        Err(e) => return fail(outcome, format!("no converting thread: {e}")),
    };
    // Two pictures: the thread scales into one while the other is on its way to the screen.
    let mut spare: Vec<Vec<u32>> = vec![Vec::new(), Vec::new()];
    let mut spare_yuv: Vec<I420> = vec![I420::default(), I420::default()];
    let mut busy = false;
    let mut ready: Option<Done> = None;
    let mut cleared = false;
    let mut last: Option<Frame> = None;
    let mut last_frame_at = Instant::now();
    loop {
        if fs.terminating() {
            break;
        }
        let ask = keys_pressed(fs);
        if matches!(ask, Ask::Leave) {
            break;
        }
        if matches!(ask, Ask::Photo)
            || (start == Start::Photo && shown.started.elapsed() >= SETTLE && last.is_some())
        {
            match take_photo(&mut cam, &running, &photo_req, last.as_ref(), dir) {
                Ok(p) => {
                    eprintln!("lucina-app-camera: photo {}", p.display());
                    outcome.photos.push(p);
                }
                Err(e) => outcome = fail(outcome, format!("photo: {e}")),
            }
            if start == Start::Photo {
                break;
            }
            // A white frame says the photo was taken.
            let (sw, sh) = (fs.screen.fb.width as usize, fs.screen.fb.height as usize);
            let white = vec![WHITE; sw];
            for y in 0..sh {
                fs.screen.fb.present_image(0, y as i32, sw, &white);
            }
            cleared = false;
            if let Some(g) = gpu.as_mut() {
                g.screen_changed();
            }
        }
        if busy {
            match done_rx.try_recv() {
                Ok(d) => {
                    busy = false;
                    ready = Some(d);
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    outcome = fail(outcome, String::from("the converting thread ended"));
                    break;
                }
            }
        }
        // A free converter gets the newest frame at once; with a picture waiting to be shown, only a frame that is already
        // there, so the picture is not held back.
        if !busy {
            let wait = if ready.is_some() {
                Duration::ZERO
            } else {
                Duration::from_millis(100)
            };
            let frame = match cam.next_frame(Some(wait)) {
                Ok(f) => f,
                Err(e) => {
                    outcome = fail(outcome, format!("frames: {e}"));
                    break;
                }
            };
            match frame {
                Some(frame) => {
                    last_frame_at = Instant::now();
                    // A frame the kernel flags (data lost on the way) is not shown: a JPEG with a hole decodes to a grey band, a
                    // YUY2 frame is short.
                    if lucina_uvc::rules::shown(frame.header.error()) {
                        let job = Job {
                            data: frame.data.clone(),
                            fourcc: frame.header.fourcc,
                            width: frame.header.width as usize,
                            height: frame.header.height as usize,
                            matrix: m,
                            screen,
                            out: spare.pop().unwrap_or_default(),
                            yuv: gpu.as_ref().map(|_| spare_yuv.pop().unwrap_or_default()),
                        };
                        if job_tx.send(job).is_err() {
                            outcome = fail(outcome, String::from("the converting thread ended"));
                            break;
                        }
                        busy = true;
                    }
                    last = Some(frame);
                }
                None => {
                    if last_frame_at.elapsed() > NO_FRAMES {
                        outcome = fail(outcome, String::from("no frames from the camera"));
                        break;
                    }
                }
            }
        }
        if let Some(d) = ready.take() {
            if let Some(rect) = d.rect {
                let t = Instant::now();
                let mut upload_us = 0;
                match (gpu.as_mut(), d.yuv.as_ref()) {
                    (Some(g), Some(p)) => {
                        let drawn = g.upload(p).and_then(|()| {
                            upload_us = t.elapsed().as_micros() as u64;
                            g.draw(screen.0, screen.1)
                        });
                        if let Err(e) = drawn {
                            // The GPU refused: back to the CPU path for the rest of the preview.
                            eprintln!(
                                "lucina-app-camera: GPU drawing failed ({e}); converting on the CPU from now on"
                            );
                            gpu = None;
                            cleared = false;
                        }
                    }
                    // Decoded for the GPU just before it failed: nothing for the CPU path to show.
                    (None, Some(_)) => {}
                    (_, None) => present(fs, rect, &d.out, &mut cleared),
                }
                shown.frames += 1;
                shown.window_frames += 1;
                shown.convert_us += d.us;
                shown.upload_us += upload_us;
                shown.show_us += t.elapsed().as_micros() as u64 - upload_us;
                shown.cpu = d.cpu;
            }
            spare.push(d.out);
            if let Some(p) = d.yuv {
                spare_yuv.push(p);
            }
        } else if busy {
            // Nothing to show yet: wait for the converter (keys are looked at again within 100 ms).
            match done_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(d) => {
                    busy = false;
                    ready = Some(d);
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    outcome = fail(outcome, String::from("the converting thread ended"));
                    break;
                }
            }
        }
        if shown.window_start.elapsed() >= REPORT_EVERY {
            let secs = shown.window_start.elapsed().as_secs_f32();
            let n = shown.window_frames.max(1) as f32;
            let cpu_now = cpu_time();
            let busy_pct =
                (cpu_now.saturating_sub(shown.cpu_at_start)).as_secs_f32() / secs * 100.0;
            eprintln!(
                "lucina-app-camera: preview {:.1} fps over {:.1} s; per frame {:.1} ms to {} (own thread, CPU {}), {:.2} ms to copy for the GPU, {:.2} ms to draw; process CPU {:.0}% of one core",
                shown.window_frames as f32 / secs,
                secs,
                shown.convert_us as f32 / 1000.0 / n,
                if gpu.is_some() {
                    "decode to I420"
                } else {
                    "convert"
                },
                shown
                    .cpu
                    .map_or_else(|| String::from("?"), |c| c.to_string()),
                shown.upload_us as f32 / 1000.0 / n,
                shown.show_us as f32 / 1000.0 / n,
                busy_pct
            );
            shown.window_start = Instant::now();
            shown.window_frames = 0;
            shown.convert_us = 0;
            shown.upload_us = 0;
            shown.show_us = 0;
            shown.cpu_at_start = cpu_now;
        }
    }
    drop(job_tx);
    drop(done_rx);
    let _ = worker.join();
    drop(cam);
    let secs = shown.started.elapsed().as_secs_f32();
    outcome.frames = shown.frames;
    outcome.fps = if secs > 0.0 {
        shown.frames as f32 / secs
    } else {
        0.0
    };
    eprintln!(
        "lucina-app-camera: preview over: {} frames in {:.1} s ({:.1} fps)",
        outcome.frames, secs, outcome.fps
    );
    outcome
}
