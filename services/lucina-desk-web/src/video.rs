//! The picture: scan -> changed stripes -> I420 -> FilmCraft's H.264 -> viewers, with voxl's stripes, paint-over and
//! ack-based flow control (`video.rs` in voxl).
//!
//! As voxl, the screen is cut into horizontal stripes (`stripe_layout(h, 14)`: 80 rows each on 1080p, the last running past the bottom edge), each its own
//! H.264 stream with its own encoder, and a round encodes only the stripes that changed, in parallel. That matters more here
//! than in voxl: FilmCraft's encoder costs 35-70 ms for a whole 1080p picture on an x86 desktop even when nothing changed,
//! and Juno's CPU is slower.
//!
//! Where FilmCraft differs from x264, and what that changes:
//! - It has no "force an IDR" and no way to change the rate on a running encoder. A keyframe is a new encoder (which starts
//!   with an IDR), as voxl does when it switches a stripe's threads. So is a change of quality.
//! - Paint-over (voxl: once the screen has been still for 150 ms, re-encode what was sent at motion quality at CRF 16, a
//!   burst of 3): the first pass of a burst is a new CRF 16 encoder, so an IDR; passes 2 and 3 are P pictures of the same
//!   screen on it, which refine what the first left. The stripe then stays at CRF 16: small changes (typing, a cursor) are
//!   sent sharp at once and need no paint-over. A change to more than a quarter of the stripe goes back to a CRF 24 encoder.
//! - No bitrate cap or adaptive rate (voxl's `RateCtl`): CRF only. On a LAN the frame budget, not the link, is the limit.
//! - No regional refine (voxl's `Refiner`): paint-over is per stripe, after the whole screen is still.

use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use filmcraft_h264enc::{ColorConfig, Encoder, EncoderConfig, Preset, Profile, RateControl, YuvFrame};

use crate::Screen;
use crate::capture::Capture;
use crate::hub::{Client, Hub, Out, Snapshot, lock};

pub const STRIPES: u32 = 14;
pub const CRF: f32 = 24.0;
pub const PAINTOVER_CRF: f32 = 16.0;
const PAINTOVER_DELAY: Duration = Duration::from_millis(150);
const PAINTOVER_BURST: u32 = 3;
/// At most 30 rounds a second.
const ROUND: Duration = Duration::from_micros(1_000_000 / 30);
/// Frames a viewer may have unacknowledged before rounds wait for it.
pub const MAX_UNACKED: usize = 3;
/// A viewer this far behind (a hidden tab, a dead socket) stops holding everyone back and starts over with a keyframe.
const STALLED: Duration = Duration::from_secs(4);
/// A stripe at paint-over quality goes back to motion quality when more than this share of its blocks change at once.
const BIG_CHANGE_PERCENT: usize = 25;
/// The 16-byte header of a video message.
pub const HDR: usize = 16;

/// Splits the screen into horizontal stripes, each an independent H.264 stream: voxl's `stripe_layout`, except that every
/// stripe has the same height and the last one runs past the bottom edge (its extra rows repeat the screen's last row).
/// voxl's cut gives 1080p a last stripe of 40 rows, and Chrome on macOS refused that 1920x40 picture ("Decoding error",
/// seen on the laptop 2026-11-04) while ffmpeg decodes it; the 1920x80 stripes decode everywhere. voxl's client draws
/// each stripe at its y on a canvas the size of the hello, so the rows past the edge are clipped.
pub fn stripe_layout(height: u32, n: u32) -> Vec<(u32, u32)> {
    let n = n.clamp(1, 16);
    let each = (height.div_ceil(n) + 15) & !15;
    (0..height.div_ceil(each)).map(|i| (i * each, each)).collect()
}

/// One stripe's encoder and the quality it encodes at.
struct Codec {
    enc: Option<Encoder>,
    crf: f32,
    pts: i64,
}

enum Mode {
    /// A new encoder at this CRF: the picture is an IDR.
    Fresh(f32),
    /// The next picture of the running stream.
    Next,
}

struct Job {
    idx: usize,
    codec: Box<Codec>,
    /// The stripe's rows, XRGB.
    px: Vec<u32>,
    w: usize,
    h: usize,
    mode: Mode,
}

/// (stripe, its codec back, a keyframe? and the Annex-B picture, or what went wrong).
type Done = (usize, Box<Codec>, Result<(bool, Vec<u8>), String>);

fn config(w: usize, h: usize, crf: f32) -> EncoderConfig {
    let mut c = EncoderConfig::new(w as u32, h as u32, 30, 1);
    // Constrained Baseline (CAVLC, no B-frames): `avc1.42e0xx`, which WebCodecs decodes everywhere, Safari included.
    c.profile = Profile::Baseline;
    c.preset = Preset::Speed;
    c.rate = RateControl::Crf(crf);
    c.keyint = u32::MAX;
    c.bframes = 0;
    c.scenecut = false;
    c.threads = 1;
    // What voxl's conversion makes: BT.601 limited range (primaries 1, transfer 13, matrix 6, as voxl's x264 VUI).
    c.color = ColorConfig { primaries: 1, transfer: 13, matrix: 6, full_range: false };
    c
}

/// XRGB rows into I420, BT.601 limited range with chroma averaged over each 2x2, the matrix of lucina-app-desk's capture and
/// of voxl's libyuv ARGBToI420. The planes are padded to even sizes by repeating the last row and column.
pub fn to_i420(px: &[u32], w: usize, h: usize) -> (usize, usize, Vec<u8>, Vec<u8>, Vec<u8>) {
    let (ew, eh) = (w.next_multiple_of(2), h.next_multiple_of(2));
    let at = |x: usize, y: usize| px[y.min(h - 1) * w + x.min(w - 1)];
    let mut y = vec![0u8; ew * eh];
    for yy in 0..eh {
        for xx in 0..ew {
            let p = at(xx, yy);
            let (r, g, b) = ((p >> 16 & 255) as i32, (p >> 8 & 255) as i32, (p & 255) as i32);
            y[yy * ew + xx] = (((66 * r + 129 * g + 25 * b + 128) >> 8) + 16) as u8;
        }
    }
    let (cw, ch) = (ew / 2, eh / 2);
    let (mut u, mut v) = (vec![0u8; cw * ch], vec![0u8; cw * ch]);
    for cy in 0..ch {
        for cx in 0..cw {
            let (mut r, mut g, mut b) = (0i32, 0i32, 0i32);
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let p = at(cx * 2 + dx, cy * 2 + dy);
                r += (p >> 16 & 255) as i32;
                g += (p >> 8 & 255) as i32;
                b += (p & 255) as i32;
            }
            let (r, g, b) = (r / 4, g / 4, b / 4);
            u[cy * cw + cx] = (((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128) as u8;
            v[cy * cw + cx] = (((112 * r - 94 * g - 18 * b + 128) >> 8) + 128) as u8;
        }
    }
    (ew, eh, y, u, v)
}

fn work(job: Job) -> Done {
    let Job { idx, mut codec, px, w, h, mode } = job;
    let (ew, eh, y, u, v) = to_i420(&px, w, h);
    if let Mode::Fresh(crf) = mode {
        codec.enc = None;
        codec.crf = crf;
    }
    if codec.enc.is_none() {
        match Encoder::new(config(ew, eh, codec.crf)) {
            Ok(e) => codec.enc = Some(e),
            Err(e) => return (idx, codec, Err(format!("stripe {idx}: encoder: {e}"))),
        }
    }
    let Some(enc) = codec.enc.as_mut() else { return (idx, codec, Err("no encoder".into())) };
    let frame = YuvFrame { y: &y, u: &u, v: &v, y_stride: ew, uv_stride: ew / 2 };
    let pts = codec.pts;
    codec.pts += 1;
    let r = match enc.encode(&frame, pts) {
        Ok(packets) => {
            let key = packets.iter().any(|p| p.keyframe);
            let mut out = vec![0u8; HDR];
            for p in packets {
                out.extend_from_slice(&p.data);
            }
            Ok((key, out))
        }
        Err(e) => {
            // Starts over with a new encoder next time, so viewers get an IDR rather than a broken stream.
            codec.enc = None;
            Err(format!("stripe {idx}: {e}"))
        }
    };
    (idx, codec, r)
}

/// Encoder threads kept for the whole run, so that a round costs no thread starts.
struct Pool {
    jobs: mpsc::Sender<Job>,
    done: mpsc::Receiver<Done>,
}

impl Pool {
    fn new(n: usize) -> Pool {
        let (jobs, rx) = mpsc::channel::<Job>();
        let (dtx, done) = mpsc::channel();
        let rx = Arc::new(Mutex::new(rx));
        for i in 0..n {
            let (rx, dtx) = (rx.clone(), dtx.clone());
            let spawned = std::thread::Builder::new().name(format!("desk-web-enc-{i}")).spawn(move || {
                loop {
                    let job = match lock(&rx).recv() {
                        Ok(j) => j,
                        Err(_) => return,
                    };
                    if dtx.send(work(job)).is_err() {
                        return;
                    }
                }
            });
            if let Err(e) = spawned {
                eprintln!("lucina-desk-web: encoder thread {i}: {e}");
            }
        }
        Pool { jobs, done }
    }
}

/// Encoder threads: what the CPU has, less two for the person at the laptop and the capture, and at least one.
pub fn workers() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get().saturating_sub(2).max(1))
}

struct Stripe {
    y: usize,
    h: usize,
    codec: Option<Box<Codec>>,
    /// Changed since it was last encoded, and in how many blocks.
    dirty: bool,
    dirty_blocks: usize,
    /// Sent at motion quality since its last sharp picture: a paint-over is owed.
    touched: bool,
}

/// Rounds may go on: no started viewer has too many frames unacknowledged, or waits too long for the oldest. voxl's
/// `flow_ok`, with the frame count fixed at `MAX_UNACKED`. A stalled viewer is reset to start over with a keyframe.
fn flow_ok(clients: &[Arc<Client>]) -> bool {
    let mut ok = true;
    for c in clients {
        if !c.started.load(Ordering::Relaxed) {
            continue;
        }
        let (n, age) = c.backlog();
        if n == 0 {
            continue;
        }
        if age > STALLED {
            eprintln!("lucina-desk-web: viewer {} stalled ({n} frames, {} ms); starting it over", c.id, age.as_millis());
            lock(&c.inflight).clear();
            c.started.store(false, Ordering::Relaxed);
            c.needs_keyframe.store(true, Ordering::Relaxed);
            continue;
        }
        let rtt = match c.min_rtt_ms.load(Ordering::Relaxed) {
            0 => 40,
            r => r,
        };
        if n >= MAX_UNACKED || age > Duration::from_millis(rtt as u64 + 120) {
            ok = false;
        }
    }
    ok
}

/// The capture thread, until the hub stops.
pub fn run(hub: Arc<Hub>, mut screen: Box<dyn Screen>) {
    let (w, h) = (hub.width as usize, hub.height as usize);
    let mut cap = Capture::new(w, h);
    cap.scan(screen.as_mut(), true);
    let cols = cap.cols();
    let mut stripes: Vec<Stripe> = hub
        .stripes
        .iter()
        .map(|&(y, sh)| Stripe {
            y: y as usize,
            h: sh as usize,
            codec: Some(Box::new(Codec { enc: None, crf: CRF, pts: 0 })),
            dirty: false,
            dirty_blocks: 0,
            touched: false,
        })
        .collect();
    let n = stripes.len();
    let pool = Pool::new(workers().min(n));
    let mut last_round = Instant::now() - ROUND;
    let mut last_motion = Instant::now();
    let mut paintover_left = 0u32;
    let mut exact_next = false;
    let mut frame_id: u32 = 0;
    while !hub.stopping() {
        let watched = !hub.video_clients().is_empty();
        let wanted = hub.waiters.load(Ordering::SeqCst) > 0 || !lock(&hub.snapshots).is_empty();
        if !watched && !wanted {
            // Nobody looks: no scanning. Whatever is drawn meanwhile is found by an exact scan when someone looks again.
            exact_next = true;
            hub.waker.wait(Duration::from_millis(500));
            continue;
        }
        let since = last_round.elapsed();
        if since < ROUND {
            hub.waker.wait(ROUND - since);
            continue;
        }
        let t0 = Instant::now();
        last_round = t0;
        let snaps = std::mem::take(&mut *lock(&hub.snapshots));
        let exact = std::mem::take(&mut exact_next) || !snaps.is_empty();
        if cap.scan(screen.as_mut(), exact) > 0 {
            hub.activity.note_blocks(cap.changed());
            hub.stats.captured.fetch_add(1, Ordering::Relaxed);
            last_motion = t0;
            paintover_left = PAINTOVER_BURST;
            for s in stripes.iter_mut() {
                let end = ((s.y + s.h).div_ceil(16) * cols).min(cap.changed().len());
                let blocks = cap.changed()[s.y / 16 * cols..end].iter().filter(|c| **c).count();
                if blocks > 0 {
                    s.dirty = true;
                    s.dirty_blocks += blocks;
                }
            }
        }
        if !snaps.is_empty() {
            // The pixels the scan just read: the screenshot's seq covers what it shows.
            let shot = Arc::new(Snapshot { width: w, height: h, xrgb: cap.screen().to_vec(), seq: hub.activity.seq() });
            for tx in snaps {
                let _ = tx.send(shot.clone());
            }
        }
        let clients = hub.video_clients();
        if clients.is_empty() {
            continue;
        }
        let want_key = clients.iter().any(|c| c.needs_keyframe.load(Ordering::Relaxed));
        let motion = stripes.iter().any(|s| s.dirty);
        if paintover_left > 0 && !stripes.iter().any(|s| s.touched) {
            paintover_left = 0;
        }
        let paintover_due = paintover_left > 0 && last_motion.elapsed() >= PAINTOVER_DELAY;
        if !want_key && !motion && !paintover_due {
            continue;
        }
        // A slow link drops states, not time: while a viewer is behind, rounds are skipped and the changes add up, and the
        // next round sends the newest picture of every stripe that changed meanwhile.
        if !want_key && !flow_ok(&clients) {
            hub.stats.flow_blocked.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        let paintover = !want_key && !motion;
        if want_key {
            for c in &clients {
                c.needs_keyframe.store(false, Ordering::Relaxed);
            }
            // A keyframe is at motion quality: sharpen all of it once the screen is still, as voxl's `mark_all`.
            last_motion = t0;
            paintover_left = PAINTOVER_BURST;
        }
        let mut pending = 0;
        for (idx, s) in stripes.iter_mut().enumerate() {
            let Some(codec) = s.codec.take() else { continue };
            let mode = if want_key {
                Some(Mode::Fresh(CRF))
            } else if s.dirty {
                let all = s.h.div_ceil(16) * cols;
                let big = s.dirty_blocks.min(all) * 100 > all * BIG_CHANGE_PERCENT;
                Some(if codec.crf == PAINTOVER_CRF && big { Mode::Fresh(CRF) } else { Mode::Next })
            } else if paintover && s.touched {
                Some(if codec.crf == PAINTOVER_CRF { Mode::Next } else { Mode::Fresh(PAINTOVER_CRF) })
            } else {
                None
            };
            let Some(mode) = mode else {
                s.codec = Some(codec);
                continue;
            };
            // Rows past the bottom edge (the last stripe) repeat the screen's last row: see `stripe_layout`.
            let rows = cap.screen().len() / w;
            let mut px = cap.screen()[s.y * w..(s.y + s.h).min(rows) * w].to_vec();
            while px.len() < s.h * w {
                px.extend_from_within(px.len() - w..);
            }
            if let Err(e) = pool.jobs.send(Job { idx, codec, px, w, h: s.h, mode }) {
                // No encoder threads: nothing can be sent any more.
                eprintln!("lucina-desk-web: encoders gone: {e}");
                return;
            }
            pending += 1;
        }
        let mut parts: Vec<(usize, bool, Vec<u8>)> = Vec::with_capacity(pending);
        for _ in 0..pending {
            let Ok((idx, codec, r)) = pool.done.recv() else {
                eprintln!("lucina-desk-web: encoders gone");
                return;
            };
            let s = &mut stripes[idx];
            let sharp = codec.crf == PAINTOVER_CRF;
            s.codec = Some(codec);
            match r {
                Ok((key, out)) if out.len() > HDR => {
                    if want_key || s.dirty {
                        s.touched |= !sharp;
                    }
                    s.dirty = false;
                    s.dirty_blocks = 0;
                    parts.push((idx, key, out));
                }
                Ok(_) => {}
                Err(e) => eprintln!("lucina-desk-web: {e}"),
            }
        }
        if paintover {
            paintover_left -= 1;
            hub.stats.paintovers.fetch_add(1, Ordering::Relaxed);
            if paintover_left == 0 {
                stripes.iter_mut().for_each(|s| s.touched = false);
            }
        }
        if parts.is_empty() {
            continue;
        }
        parts.sort_by_key(|p| p.0);
        let all_key = parts.len() == n && parts.iter().all(|p| p.1);
        if want_key && !all_key {
            // A stripe failed: whoever waits for a whole keyframe asks again next round.
            for c in &clients {
                if !c.started.load(Ordering::Relaxed) {
                    c.needs_keyframe.store(true, Ordering::Relaxed);
                }
            }
        }
        let total: usize = parts.iter().map(|p| p.2.len()).sum();
        hub.stats.encode_us.fetch_add(t0.elapsed().as_micros() as u64, Ordering::Relaxed);
        hub.stats.encoded.fetch_add(1, Ordering::Relaxed);
        hub.stats.bytes.fetch_add(total as u64, Ordering::Relaxed);
        if all_key {
            hub.stats.keyframes.fetch_add(1, Ordering::Relaxed);
        }
        frame_id = frame_id.wrapping_add(1);
        // Capture to sent, in 0.1 ms, for the viewer's stats.
        let pipe = (t0.elapsed().as_micros() / 100).min(u32::MAX as u128) as u32;
        let nparts = parts.len() as u8;
        let msgs: Vec<Arc<Vec<u8>>> = parts
            .into_iter()
            .map(|(i, key, mut out)| {
                // As voxl's video.rs: type, flags (bit 0 key, bit 1 paint-over), frame id, pipe time, stripe, stripe count,
                // parts in this frame, reserved, stripe y.
                out[0] = 0x01;
                out[1] = key as u8 | if paintover { 2 } else { 0 };
                out[2..6].copy_from_slice(&frame_id.to_be_bytes());
                out[6..10].copy_from_slice(&pipe.to_be_bytes());
                out[10] = i as u8;
                out[11] = n as u8;
                out[12] = nparts;
                out[13] = 0;
                out[14..16].copy_from_slice(&(stripes[i].y as u16).to_be_bytes());
                Arc::new(out)
            })
            .collect();
        let now = Instant::now();
        for c in hub.video_clients() {
            if !c.started.load(Ordering::Relaxed) {
                if !all_key {
                    continue;
                }
                c.started.store(true, Ordering::Relaxed);
                lock(&c.inflight).clear();
            }
            {
                let mut q = lock(&c.inflight);
                if q.len() > 600 {
                    eprintln!("lucina-desk-web: viewer {} hopelessly behind; dropping it", c.id);
                    c.close();
                    continue;
                }
                q.push_back((frame_id, now));
            }
            for m in &msgs {
                c.send(Out::Bin(m.clone()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stripes_as_voxl_cuts_1080p_and_small_screens() {
        let l = stripe_layout(1080, STRIPES);
        assert_eq!(l.len(), 14);
        assert!(l.iter().enumerate().all(|(i, &(y, h))| y == i as u32 * 80 && h == 80));
        assert_eq!(stripe_layout(200, STRIPES), (0..13).map(|i| (i * 16, 16)).collect::<Vec<_>>());
        assert_eq!(stripe_layout(10, 1), vec![(0, 16)]);
    }

    #[test]
    fn flow_waits_for_the_unacked_limit_and_the_oldest_frames_age_and_starts_a_stalled_viewer_over() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let s = std::net::TcpStream::connect(l.local_addr().unwrap()).unwrap();
        let hub = Hub::new(64, 64, stripe_layout(64, STRIPES), false, crate::input::Keys::new(Box::new(crate::input::tests::Recorder::default())));
        let c = hub.add_client(mpsc::channel().0, s);
        let now = Instant::now();
        let set = |frames: &[(u32, Instant)]| *lock(&c.inflight) = frames.iter().copied().collect();
        // Not started: never holds anyone back.
        set(&[(1, now), (2, now), (3, now)]);
        assert!(flow_ok(&[c.clone()]));
        c.started.store(true, Ordering::Relaxed);
        set(&[(1, now), (2, now)]);
        assert!(flow_ok(&[c.clone()]));
        set(&[(1, now), (2, now), (3, now)]);
        assert!(!flow_ok(&[c.clone()]));
        c.on_ack(2);
        assert!(flow_ok(&[c.clone()]));
        // The oldest is older than the RTT (40 ms before any ping) plus 120 ms.
        set(&[(4, now - Duration::from_millis(200))]);
        assert!(!flow_ok(&[c.clone()]));
        c.min_rtt_ms.store(150, Ordering::Relaxed);
        assert!(flow_ok(&[c.clone()]));
        // Stalled: starts over with a keyframe and stops gating.
        set(&[(5, now - Duration::from_secs(5))]);
        c.needs_keyframe.store(false, Ordering::Relaxed);
        assert!(flow_ok(&[c.clone()]));
        assert!(!c.started.load(Ordering::Relaxed) && c.needs_keyframe.load(Ordering::Relaxed));
        assert_eq!(c.backlog().0, 0);
    }

    #[test]
    fn conversion_is_limited_range_bt601_and_pads_odd_sizes() {
        let (ew, eh, y, u, v) = to_i420(&[0xffff_ffff, 0xff00_0000, 0xffff_ffff], 3, 1);
        assert_eq!((ew, eh), (4, 2));
        assert_eq!(&y[..4], &[235, 16, 235, 235]);
        assert_eq!(&y[4..], &[235, 16, 235, 235]);
        assert_eq!((u[1], v[1]), (128, 128));
        let (_, _, y, u, v) = to_i420(&[0xffff_0000; 4], 2, 2);
        assert_eq!((y[0], u[0], v[0]), (82, 90, 240));
    }
}
