//! `GET /wait`, with voxl's `wait.rs` semantics: returns when the screen settles or changes, for scripts that would otherwise
//! sleep after every action and hope it was long enough.
//!
//! - `idle=300` (the default): once nothing has changed for that many ms, counted from the request at the earliest.
//! - `change`: at the first change.
//! - `region=x,y,w,h` counts only changes in that rectangle (to the 16-pixel block), `timeout=ms` gives up (default 10 s,
//!   at most 600 s), `since=n` counts changes after an earlier `seq` (from `/wait` or a screenshot's `X-Voxl-Seq`).
//!
//! The answer is JSON: `changed`, `timed_out`, `seq`, `waited_ms`. voxl hears about changes from the X server; here the
//! capture thread's scan finds them (`Capture`), and it scans while a wait is open even when no viewer watches.

use std::collections::HashMap;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

const DEFAULT_IDLE: Duration = Duration::from_millis(300);
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_WAIT: Duration = Duration::from_secs(600);

/// A change: its sequence number (counting up from 1) and when it happened.
type Mark = (u64, Instant);

struct Map {
    width: u32,
    height: u32,
    cols: usize,
    /// Last change of each 16x16 block, row-major.
    blocks: Vec<Mark>,
    last: Mark,
    /// The latest new capture, which changed everything.
    reset: Mark,
}

impl Map {
    /// Blocks covering [x0, x1) x [y0, y1), clipped to the screen: (col0, col1, row0, row1).
    fn span(&self, x0: u32, y0: u32, x1: u32, y1: u32) -> (usize, usize, usize, usize) {
        let (x1, y1) = (x1.min(self.width), y1.min(self.height));
        if x1 <= x0 || y1 <= y0 {
            return (0, 0, 0, 0);
        }
        (x0 as usize / 16, (x1 as usize).div_ceil(16), y0 as usize / 16, (y1 as usize).div_ceil(16))
    }
}

#[derive(Clone, Copy)]
pub struct Rect {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

/// When each part of the screen last changed.
pub struct Activity {
    map: Mutex<Map>,
    /// Notified at every change: wakes waiting requests.
    changed: Condvar,
}

impl Default for Activity {
    fn default() -> Self {
        let start = (0, Instant::now());
        Activity {
            map: Mutex::new(Map { width: 0, height: 0, cols: 0, blocks: Vec::new(), last: start, reset: start }),
            changed: Condvar::new(),
        }
    }
}

impl Activity {
    fn lock(&self) -> std::sync::MutexGuard<'_, Map> {
        self.map.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A new capture: everything on screen is new.
    pub fn reset(&self, width: u32, height: u32) {
        let mut m = self.lock();
        let now = (m.last.0 + 1, Instant::now());
        let cols = (width as usize).div_ceil(16);
        *m = Map { width, height, cols, blocks: vec![now; cols * (height as usize).div_ceil(16)], last: now, reset: now };
        self.changed.notify_all();
    }

    /// The 16x16 blocks that changed, row-major on the same grid (`Capture::changed`).
    pub fn note_blocks(&self, changed: &[bool]) {
        let mut m = self.lock();
        let now = (m.last.0 + 1, Instant::now());
        let mut any = false;
        for (b, c) in m.blocks.iter_mut().zip(changed) {
            if *c {
                *b = now;
                any = true;
            }
        }
        if any {
            m.last = now;
            self.changed.notify_all();
        }
    }

    /// Sequence number of the latest change.
    pub fn seq(&self) -> u64 {
        self.lock().last.0
    }

    /// The latest change in `region` (or anywhere) and the latest sequence number, read together.
    fn latest(m: &Map, region: Option<Rect>) -> (Mark, u64) {
        let Some(r) = region else { return (m.last, m.last.0) };
        let (c0, c1, r0, r1) = m.span(r.x, r.y, r.x.saturating_add(r.w), r.y.saturating_add(r.h));
        let mark = (r0..r1)
            .flat_map(|row| &m.blocks[row * m.cols + c0..row * m.cols + c1])
            .fold(m.reset, |a, &b| if b.0 > a.0 { b } else { a });
        (mark, m.last.0)
    }

    fn on_screen(&self, r: Rect) -> bool {
        let m = self.lock();
        m.span(r.x, r.y, r.x.saturating_add(r.w), r.y.saturating_add(r.h)) != (0, 0, 0, 0)
    }

    /// Answers a `/wait` with `query` (without the `?`): the HTTP status and the JSON body. Blocks until it is done.
    pub fn wait(&self, query: &str) -> (u16, String) {
        let start = Instant::now();
        let q = parse_query(query);
        let bad = |msg: &str| (400, format!("{{\"error\":\"{msg}\"}}"));
        let Some(idle) = millis(&q, "idle", DEFAULT_IDLE) else { return bad("idle must be a whole number of milliseconds") };
        let Some(timeout) = millis(&q, "timeout", DEFAULT_TIMEOUT) else {
            return bad("timeout must be a whole number of milliseconds");
        };
        let change = q.get("change").is_some_and(|v| !matches!(v.as_str(), "0" | "false"));
        let region = match q.get("region") {
            None => None,
            Some(v) => match parse_region(v) {
                Some(r) if self.on_screen(r) => Some(r),
                Some(_) => return bad("region is off screen"),
                None => return bad("region must be x,y,w,h"),
            },
        };
        let since = match q.get("since") {
            None => self.seq(),
            Some(v) => match v.parse() {
                Ok(n) => n,
                Err(_) => return bad("since must be a seq from an earlier /wait or screenshot"),
            },
        };
        let deadline = start + timeout;
        let mut m = self.lock();
        let timed_out = loop {
            let ((seq, at), _) = Self::latest(&m, region);
            let now = Instant::now();
            let done_at = if change { (seq > since).then_some(now) } else { Some(at.max(start) + idle) };
            if done_at.is_some_and(|t| t <= now) {
                break false;
            }
            if now >= deadline {
                break true;
            }
            let wake = done_at.map_or(deadline, |t| t.min(deadline));
            m = self.changed.wait_timeout(m, wake - now).unwrap_or_else(|e| e.into_inner()).0;
        };
        let ((latest, _), seq) = Self::latest(&m, region);
        let body = format!(
            "{{\"changed\":{},\"timed_out\":{timed_out},\"seq\":{seq},\"waited_ms\":{}}}",
            latest > since,
            start.elapsed().as_millis()
        );
        (200, body)
    }
}

/// `a=1&b&c=x` into a map; values are numbers and flags here, so no percent-decoding is needed beyond what a form sends.
pub fn parse_query(q: &str) -> HashMap<String, String> {
    q.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| match p.split_once('=') {
            Some((k, v)) => (k.to_owned(), v.replace("%2C", ",").replace("%2c", ",")),
            None => (p.to_owned(), String::new()),
        })
        .collect()
}

fn millis(q: &HashMap<String, String>, key: &str, default: Duration) -> Option<Duration> {
    match q.get(key) {
        None => Some(default),
        Some(v) => v.parse().ok().map(|ms| Duration::from_millis(ms).min(MAX_WAIT)),
    }
}

fn parse_region(v: &str) -> Option<Rect> {
    let n: Vec<u32> = v.split(',').map(|s| s.trim().parse().ok()).collect::<Option<_>>()?;
    match n[..] {
        [x, y, w, h] if w > 0 && h > 0 => Some(Rect { x, y, w, h }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn activity() -> Arc<Activity> {
        let a = Arc::new(Activity::default());
        a.reset(64, 64);
        a
    }

    #[test]
    fn bad_arguments_are_400() {
        let a = activity();
        assert_eq!(a.wait("idle=x").0, 400);
        assert_eq!(a.wait("region=1,2,3").0, 400);
        assert_eq!(a.wait("region=100,100,5,5").0, 400);
        assert_eq!(a.wait("since=-1").0, 400);
    }

    #[test]
    fn change_returns_at_a_change_in_the_region_only() {
        let a = activity();
        let b = a.clone();
        let t = std::thread::spawn(move || b.wait("change&region=0,0,16,16&timeout=5000"));
        std::thread::sleep(Duration::from_millis(50));
        // Block (1, 0): outside the region.
        let mut c = vec![false; 16];
        c[1] = true;
        a.note_blocks(&c);
        std::thread::sleep(Duration::from_millis(50));
        c[0] = true;
        a.note_blocks(&c);
        let (code, body) = t.join().unwrap();
        assert_eq!(code, 200);
        assert!(body.contains("\"changed\":true") && body.contains("\"timed_out\":false"), "{body}");
    }

    #[test]
    fn since_catches_a_change_from_before_the_request_and_idle_times_out_while_busy() {
        let a = activity();
        let seq = a.seq();
        a.note_blocks(&[true]);
        let (_, body) = a.wait(&format!("change&since={seq}&timeout=1000"));
        assert!(body.contains("\"changed\":true") && body.contains("\"timed_out\":false"), "{body}");
        let (_, body) = a.wait("timeout=100&idle=50");
        assert!(body.contains("\"timed_out\":false"), "{body}");
        // Changing every 20 ms never settles for 200 ms.
        let b = a.clone();
        let busy = std::thread::spawn(move || {
            for _ in 0..20 {
                b.note_blocks(&[true]);
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        let (_, body) = a.wait("idle=200&timeout=250");
        assert!(body.contains("\"timed_out\":true"), "{body}");
        busy.join().unwrap();
    }
}
