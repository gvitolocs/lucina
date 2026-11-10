//! The decoding thread. A frame that takes long to decode, and AV1's hidden frames take many frames' time (368 ms measured against a median
//! of 15), must not hold up one that is due: so the screen's thread only shows, and this one decodes ahead whenever it can. On one CPU this
//! relies on the kernel giving the CPU to the screen's thread when it wakes, and the laptop's lateness numbers are what shows that it does.

use std::collections::VecDeque;
use std::os::fd::RawFd;
use std::sync::{Arc, Condvar, Mutex};

use lucina_video::{Codec, Demuxer};

use crate::decode::{Decoder, Picture, Queue, h264_droppable};

/// The Video app's decoding thread: below its screen thread.
pub const VIDEO_APP_NICE: i32 = 10;

/// Pictures decoded ahead of the reordering. About half a second: more than AV1's worst measured frame, and it costs under 3 MB each.
const AHEAD: usize = 12;

pub enum Command {
    /// To the last key frame before this time. With `forward`, past `from` if the key frames are far apart.
    Seek { pts: i64, forward: bool, from: i64 },
    /// What is queued is out of date: go on from the next key frame.
    ToKey,
}

#[derive(Default)]
pub struct Counts {
    pub decoded: u64,
    /// Of those, the packets the video block decoded; the rest went to a CPU decoder.
    pub on_venus: u64,
    pub skipped: u64,
    pub decode_ms: f64,
    pub worst_decode_ms: f64,
}

pub struct Inner {
    pub queue: Queue,
    pub eof: bool,
    pub error: Option<String>,
    /// Where the last seek landed, once it has.
    pub landed: Option<i64>,
    pub skip_nonref: bool,
    pub counts: Counts,
    commands: VecDeque<Command>,
    limit: usize,
    closing: bool,
    /// The screen's thread is waiting for pictures: tell it through the pipe.
    pub starving: bool,
}

pub struct Worker {
    pub shared: Arc<(Mutex<Inner>, Condvar)>,
}

impl Worker {
    /// Takes over the file. `wake_fd` is written when pictures arrive for a thread that said it was starving.
    pub fn start(demux: Box<dyn Demuxer>, codec: Codec, reorder: usize, wake_fd: RawFd) -> Result<Worker, String> {
        Worker::start_with_nice(demux, codec, reorder, wake_fd, VIDEO_APP_NICE)
    }

    /// As [`Worker::start`], with the decoding thread at `nice`. The Video app's 10 lets its screen thread go first; a program whose
    /// other threads are many and busy (LucinaNet: Servo's script, layout and compositor at 0) starves a decoder at 10, which then
    /// gets about a tenth of a contended CPU, and gives 0.
    pub fn start_with_nice(
        demux: Box<dyn Demuxer>,
        codec: Codec,
        reorder: usize,
        wake_fd: RawFd,
        nice: i32,
    ) -> Result<Worker, String> {
        let (w, h) = (demux.track().width, demux.track().height);
        let decoder = Decoder::for_track(codec, w, h)?;
        let inner = Inner {
            queue: Queue::default(),
            eof: false,
            error: None,
            landed: None,
            skip_nonref: false,
            counts: Counts::default(),
            commands: VecDeque::new(),
            limit: reorder + 1 + AHEAD,
            closing: false,
            starving: false,
        };
        let shared = Arc::new((Mutex::new(inner), Condvar::new()));
        let for_thread = shared.clone();
        std::thread::Builder::new()
            .name("video-decode".into())
            .spawn(move || run(demux, decoder, codec, for_thread, wake_fd, nice))
            .map_err(|e| e.to_string())?;
        Ok(Worker { shared })
    }

    pub fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.shared.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn wake(&self) {
        self.shared.1.notify_all();
    }

    pub fn command(&self, c: Command) {
        let mut s = self.lock();
        s.queue.clear();
        s.eof = false;
        s.landed = None;
        s.commands.push_back(c);
        drop(s);
        self.wake();
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.lock().closing = true;
        self.wake();
    }
}

fn tell(fd: RawFd) {
    let b = 1u8;
    // SAFETY: writes one byte from a live local. The pipe is non-blocking: when it is full the other side is awake already.
    unsafe { libc::write(fd, (&b as *const u8).cast(), 1) };
}

fn seek(demux: &mut Box<dyn Demuxer>, pts: i64, forward: bool, from: i64) -> Result<i64, String> {
    let mut want = pts.max(0);
    let mut landed = 0;
    for _ in 0..8 {
        landed = demux.seek_to(want).map_err(|e| e.to_string())?;
        if !forward || landed > from {
            break;
        }
        want += 10_000_000;
    }
    Ok(landed)
}

fn run(mut demux: Box<dyn Demuxer>, mut decoder: Decoder, codec: Codec, shared: Arc<(Mutex<Inner>, Condvar)>, wake_fd: RawFd, nice: i32) {
    let (m, cv) = &*shared;
    // At `nice`: the Video app's 10 lets its screen thread go first when both want the CPU, where the kernel honours this.
    // SAFETY: changes this thread's own priority; a failure leaves it as it was.
    unsafe { libc::setpriority(libc::PRIO_PROCESS, libc::syscall(libc::SYS_gettid) as libc::id_t, nice) };
    let mut to_key = false;
    let mut out: Vec<Picture> = Vec::with_capacity(2);
    loop {
        let (command, skip_nonref) = {
            let mut s = m.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if s.closing {
                    return;
                }
                if let Some(c) = s.commands.pop_front() {
                    break (Some(c), false);
                }
                if !s.eof && s.error.is_none() && s.queue.len() < s.limit {
                    break (None, s.skip_nonref);
                }
                s = cv.wait(s).unwrap_or_else(|e| e.into_inner());
            }
        };
        if let Some(c) = command {
            let result = match c {
                Command::ToKey => {
                    to_key = true;
                    Ok(None)
                }
                Command::Seek { pts, forward, from } => {
                    to_key = false;
                    seek(&mut demux, pts, forward, from).map(Some)
                }
            };
            // Nothing decoded before this may be referred to by what comes after a seek.
            let fresh = if matches!(result, Ok(Some(_))) {
                // The video block keeps its session and drops what it had (Linux's STREAMOFF and STREAMON on the bitstream queue);
                // if that fails, or for the CPU decoders, a new decoder.
                if let Decoder::Venus(v) = &mut decoder
                    && v.seek().is_ok()
                {
                    Ok(None)
                } else {
                    // The video block takes one session at a time: the old one ends before the new one opens.
                    if matches!(decoder, Decoder::Venus(_))
                        && let Ok(cpu) = Decoder::new(codec)
                    {
                        decoder = cpu;
                    }
                    Decoder::for_track(codec, demux.track().width, demux.track().height).map(Some)
                }
            } else {
                Ok(None)
            };
            let mut s = m.lock().unwrap_or_else(|e| e.into_inner());
            match (result, fresh) {
                (Ok(landed), Ok(new)) => {
                    if let Some(d) = new {
                        decoder = d;
                    }
                    s.landed = landed;
                }
                (Err(e), _) | (_, Err(e)) => s.error = Some(e),
            }
            cv.notify_all();
            tell(wake_fd);
            continue;
        }
        let t0 = std::time::Instant::now();
        let packet = match demux.next_packet() {
            Ok(p) => p,
            Err(e) => {
                let mut s = m.lock().unwrap_or_else(|e| e.into_inner());
                s.error = Some(e.to_string());
                tell(wake_fd);
                continue;
            }
        };
        let Some(packet) = packet else {
            let tail = decoder.flush();
            let mut s = m.lock().unwrap_or_else(|e| e.into_inner());
            for p in tail {
                s.queue.push(p);
            }
            s.eof = true;
            tell(wake_fd);
            continue;
        };
        if to_key && !packet.key {
            m.lock().unwrap_or_else(|e| e.into_inner()).counts.skipped += 1;
            continue;
        }
        to_key = false;
        if skip_nonref && codec == Codec::H264 && h264_droppable(&packet.data) {
            m.lock().unwrap_or_else(|e| e.into_inner()).counts.skipped += 1;
            continue;
        }
        decoder.set_hurrying(skip_nonref);
        let on_venus = matches!(decoder, Decoder::Venus(_));
        let result = decoder.decode(&packet, &mut out);
        // The video block failed in the middle (its memory, its firmware): the CPU goes on from the last key frame at or before this
        // packet, and the session ends with the old decoder.
        if let Err(e) = &result
            && matches!(decoder, Decoder::Venus(_))
            && let Ok(cpu) = Decoder::new(codec)
        {
            eprintln!("lucina-app-video: the video block stopped ({e}); the CPU decodes from the last key frame");
            decoder = cpu;
            out.clear();
            if demux.seek_to(packet.pts_us).is_err() {
                to_key = true;
            }
            continue;
        }
        let took = t0.elapsed().as_secs_f64() * 1000.0;
        let mut s = m.lock().unwrap_or_else(|e| e.into_inner());
        // A command that came while this was decoding has emptied the queue: what is in hand belongs to before it.
        let stale = !s.commands.is_empty();
        match result {
            Ok(()) => {
                for p in out.drain(..) {
                    if !stale {
                        s.queue.push(p);
                    }
                }
            }
            Err(e) => {
                out.clear();
                s.error = Some(e);
            }
        }
        s.counts.decoded += 1;
        s.counts.on_venus += u64::from(on_venus);
        s.counts.decode_ms += took;
        s.counts.worst_decode_ms = s.counts.worst_decode_ms.max(took);
        if std::mem::take(&mut s.starving) {
            tell(wake_fd);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn start(name: &str, reorder: usize) -> (Worker, Codec) {
        let path = format!("{}/../../drivers/lucina-video/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        let demux = lucina_video::open(std::io::Cursor::new(std::fs::read(path).unwrap())).unwrap();
        let codec = demux.track().codec;
        let mut pipe = [-1; 2];
        // SAFETY: pipe2 fills the two-element array.
        assert_eq!(unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) }, 0);
        (Worker::start(demux, codec, reorder, pipe[1]).unwrap(), codec)
    }

    fn until(w: &Worker, what: impl Fn(&Inner) -> bool) {
        let t = Instant::now();
        while !what(&w.lock()) {
            assert!(t.elapsed() < Duration::from_secs(30), "it never happened");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn take_all(w: &Worker) -> Vec<i64> {
        let mut got = Vec::new();
        loop {
            until(w, |s| s.queue.len() > 0 || s.eof);
            let p = w.lock().queue.pop();
            match p {
                Some(p) => got.push(p.pts_us() / 100_000),
                None => return got,
            }
            w.wake();
        }
    }

    #[test]
    fn decodes_ahead_and_stops_at_its_limit() {
        let (w, _) = start("plain.mp4", 4);
        until(&w, |s| s.queue.len() == 12 || s.eof);
        assert_eq!(take_all(&w), (0..12).collect::<Vec<_>>());
    }

    #[test]
    fn a_seek_forward_lands_on_a_key_frame_past_where_it_was() {
        let (w, _) = start("frag.mp4", 0);
        until(&w, |s| s.queue.len() > 0);
        w.command(Command::Seek { pts: 700_000, forward: true, from: 0 });
        until(&w, |s| s.landed.is_some());
        let at = w.lock().landed.take().unwrap();
        assert_eq!(at / 100_000, 5);
        // Everything from there, and nothing from before the seek.
        assert_eq!(take_all(&w), (5..12).collect::<Vec<_>>());
    }

    #[test]
    fn a_seek_back_after_the_end_plays_again() {
        let (w, _) = start("vp9.webm", 0);
        assert_eq!(take_all(&w).len(), 12);
        w.command(Command::Seek { pts: 0, forward: false, from: 1_100_000 });
        until(&w, |s| s.landed.is_some());
        assert_eq!(w.lock().landed.take(), Some(0));
        assert_eq!(take_all(&w), (0..12).collect::<Vec<_>>());
    }

    #[test]
    fn a_jump_goes_on_from_the_next_key_frame() {
        let (w, _) = start("frag.mp4", 0);
        // Read the first frame only, then jump: the queue was filled ahead, so where it lands depends on how far the thread had read.
        until(&w, |s| s.queue.len() > 0);
        w.command(Command::ToKey);
        let got = take_all(&w);
        assert!(got.len() <= 12);
        assert!(got.windows(2).all(|p| p[1] == p[0] + 1), "{got:?}");
    }

    #[test]
    fn what_cannot_be_decoded_is_an_error_not_a_hang() {
        let mut bad = std::fs::read(format!("{}/../../drivers/lucina-video/tests/fixtures/av1.ivf", env!("CARGO_MANIFEST_DIR"))).unwrap();
        for b in bad.iter_mut().skip(60) {
            *b ^= 0x5a;
        }
        let demux = lucina_video::open(std::io::Cursor::new(bad)).unwrap();
        let mut pipe = [-1; 2];
        // SAFETY: pipe2 fills the two-element array.
        assert_eq!(unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) }, 0);
        let w = Worker::start(demux, Codec::Av1, 0, pipe[1]).unwrap();
        until(&w, |s| s.error.is_some() || s.eof);
    }
}
