//! Pictures wanted on screen: read from the disk cache or fetched, decoded and fitted off the screen's thread, and handed back small.
//!
//! The screen says which pictures it wants, nearest first, each time it changes. Anything no longer wanted is dropped before any work
//! is done on it, so holding a key down over ten thousand photos fetches only what stopped on screen. One thread reads the cache and
//! decodes, which is all the one CPU can do at a time; the fetchers mostly wait on the network.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use lucina_app::Image;

use crate::cache::DiskCache;
use crate::media::{Connect, Media, MediaError};

/// Fetched at once: enough for a screen on a slow link, and few enough for the server's patience.
const FETCHERS: usize = 3;
/// A rendition is well under this. Anything larger is not what was asked for.
const MAX_BYTES: u64 = 32 << 20;

/// How a picture is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fit {
    /// Cropped to fill a square of this side, from the centre.
    Square(u32),
    /// Within this box, keeping its shape.
    Within(u32, u32),
}

/// A picture to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Want {
    /// What it is called when it comes back, and in the disk cache: the server's path.
    pub url: String,
    pub fit: Fit,
    /// Clockwise, as the Gallery saves it.
    pub rotation: u16,
}

pub enum Done {
    Ready(Want, Image),
    /// There is no picture to be had: the server has none, or it does not decode.
    Failed(Want, String),
    SignedOut,
}

/// Turned clockwise by a quarter turn or more. Anything else is left alone.
pub fn rotated(img: Image, rotation: u16) -> Image {
    let (w, h) = (img.w as usize, img.h as usize);
    match rotation {
        90 => {
            let mut px = vec![0; w * h];
            for y in 0..h {
                for x in 0..w {
                    px[x * h + (h - 1 - y)] = img.px[y * w + x];
                }
            }
            Image { w: img.h, h: img.w, px }
        }
        180 => {
            let mut px = img.px;
            px.reverse();
            Image { w: img.w, h: img.h, px }
        }
        270 => {
            let mut px = vec![0; w * h];
            for y in 0..h {
                for x in 0..w {
                    px[(w - 1 - x) * h + y] = img.px[y * w + x];
                }
            }
            Image { w: img.h, h: img.w, px }
        }
        _ => img,
    }
}

/// The largest centred square of a picture.
pub fn square_crop(img: &Image) -> Image {
    let side = img.w.min(img.h) as usize;
    let (x0, y0) = ((img.w as usize - side) / 2, (img.h as usize - side) / 2);
    let mut px = Vec::with_capacity(side * side);
    for y in 0..side {
        let at = (y + y0) * img.w as usize + x0;
        px.extend_from_slice(&img.px[at..at + side]);
    }
    Image { w: side as u32, h: side as u32, px }
}

/// The bytes as a picture, as it will be shown.
pub fn prepare(bytes: &[u8], fit: Fit, rotation: u16) -> Result<Image, String> {
    let img = rotated(Image::decode(bytes).map_err(|e| e.to_string())?, rotation);
    Ok(match fit {
        Fit::Square(side) => square_crop(&img).fit_within(side, side),
        Fit::Within(w, h) => img.fit_within(w, h),
    })
}

#[derive(Default)]
struct Queue {
    /// What the screen wants, first first.
    wanted: VecDeque<Want>,
    /// Not in the cache: for a fetcher.
    fetch: VecDeque<Want>,
    /// Fetched: for the decoder.
    fetched: VecDeque<(Want, Vec<u8>)>,
    /// Being read, fetched or decoded, so that it is not asked for again.
    busy: HashSet<String>,
    /// Nothing to be had from the server: not asked again while the app runs.
    gone: HashSet<String>,
    /// 429: nothing is fetched before this.
    wait_until: Option<Instant>,
}

pub struct Pictures {
    shared: Arc<(Mutex<Queue>, Condvar)>,
    cache: Option<Arc<DiskCache>>,
}

fn lock(q: &Mutex<Queue>) -> std::sync::MutexGuard<'_, Queue> {
    q.lock().unwrap_or_else(|e| e.into_inner())
}

impl Pictures {
    /// `deliver` is called on these threads: send it to the app. `cache` is `None` when there is nowhere to keep pictures.
    pub fn start(connect: Connect, cache: Option<Arc<DiskCache>>, deliver: impl Fn(Done) + Send + Sync + 'static) -> Pictures {
        let shared = Arc::new((Mutex::new(Queue::default()), Condvar::new()));
        let deliver: Arc<dyn Fn(Done) + Send + Sync> = Arc::new(deliver);
        {
            let (shared, cache, deliver) = (shared.clone(), cache.clone(), deliver.clone());
            std::thread::spawn(move || decoder(shared, cache, deliver));
        }
        for _ in 0..FETCHERS {
            let (shared, cache, deliver, connect) = (shared.clone(), cache.clone(), deliver.clone(), connect.clone());
            std::thread::spawn(move || fetcher(shared, cache, deliver, connect));
        }
        if let Some(c) = &cache {
            // Rarely anything to do, and never in the way of the first frame.
            let c = c.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(5));
                c.prune();
            });
        }
        Pictures { shared, cache }
    }

    /// These are what the screen wants now, first first; anything asked for before and not begun is forgotten.
    pub fn want(&self, wants: Vec<Want>) {
        let (q, cv) = &*self.shared;
        let mut g = lock(q);
        let order: HashMap<String, usize> = wants.iter().enumerate().map(|(i, w)| (w.url.clone(), i)).collect();
        // Those already waiting for a fetcher stay if they are still wanted, nearest first.
        let mut fetch: Vec<Want> = g.fetch.drain(..).filter(|w| order.contains_key(&w.url)).collect();
        fetch.sort_by_key(|w| order[&w.url]);
        let waiting: HashSet<String> = fetch.iter().map(|w| w.url.clone()).collect();
        g.fetch = fetch.into();
        let fresh: VecDeque<Want> =
            wants.into_iter().filter(|w| !g.gone.contains(&w.url) && !g.busy.contains(&w.url) && !waiting.contains(&w.url)).collect();
        g.wanted = fresh;
        cv.notify_all();
    }

    /// The bytes on their way to the disk go too.
    pub fn clear_cache(&self) {
        if let Some(c) = &self.cache {
            c.clear();
        }
    }

    pub fn cache(&self) -> Option<&Arc<DiskCache>> {
        self.cache.as_ref()
    }
}

fn decoder(shared: Arc<(Mutex<Queue>, Condvar)>, cache: Option<Arc<DiskCache>>, deliver: Arc<dyn Fn(Done) + Send + Sync>) {
    let (q, cv) = &*shared;
    loop {
        let mut g = lock(q);
        // What arrived comes first: it was wanted a while ago and the bytes are in memory.
        if let Some((want, bytes)) = g.fetched.pop_front() {
            drop(g);
            let result = prepare(&bytes, want.fit, want.rotation);
            let mut g = lock(q);
            g.busy.remove(&want.url);
            if result.is_err() {
                g.gone.insert(want.url.clone());
            }
            drop(g);
            match result {
                Ok(img) => deliver(Done::Ready(want, img)),
                Err(e) => deliver(Done::Failed(want, e)),
            }
            continue;
        }
        let Some(want) = g.wanted.pop_front() else {
            drop(cv.wait(g));
            continue;
        };
        g.busy.insert(want.url.clone());
        drop(g);
        let cached = cache.as_ref().and_then(|c| c.read(&want.url));
        match cached.map(|b| prepare(&b, want.fit, want.rotation)) {
            Some(Ok(img)) => {
                lock(q).busy.remove(&want.url);
                deliver(Done::Ready(want, img));
            }
            other => {
                if matches!(other, Some(Err(_))) {
                    if let Some(c) = &cache {
                        c.remove(&want.url);
                    }
                }
                let mut g = lock(q);
                g.busy.remove(&want.url);
                g.fetch.push_back(want);
                cv.notify_all();
            }
        }
    }
}

fn fetcher(
    shared: Arc<(Mutex<Queue>, Condvar)>,
    cache: Option<Arc<DiskCache>>,
    deliver: Arc<dyn Fn(Done) + Send + Sync>,
    connect: Connect,
) {
    let (q, cv) = &*shared;
    let mut media: Option<Box<dyn Media>> = None;
    loop {
        let mut g = lock(q);
        if let Some(until) = g.wait_until {
            let now = Instant::now();
            if until > now {
                drop(cv.wait_timeout(g, until - now));
                continue;
            }
            g.wait_until = None;
        }
        let Some(want) = g.fetch.pop_front() else {
            drop(cv.wait(g));
            continue;
        };
        g.busy.insert(want.url.clone());
        drop(g);
        if media.is_none() {
            media = connect();
        }
        let Some(m) = media.as_mut() else {
            lock(q).busy.remove(&want.url);
            deliver(Done::SignedOut);
            continue;
        };
        match m.bytes(&want.url, MAX_BYTES) {
            Ok(bytes) => {
                if let Some(c) = &cache {
                    c.write(&want.url, &bytes);
                }
                let mut g = lock(q);
                g.fetched.push_back((want, bytes));
                cv.notify_all();
            }
            Err(MediaError::RateLimited(s)) => {
                let mut g = lock(q);
                g.wait_until = Some(Instant::now() + Duration::from_secs(s.clamp(1, 120)));
                g.busy.remove(&want.url);
                g.fetch.push_front(want);
            }
            Err(MediaError::SignedOut) => {
                lock(q).busy.remove(&want.url);
                deliver(Done::SignedOut);
            }
            Err(e @ MediaError::Offline(_)) => {
                // Asked for again when the screen next changes.
                lock(q).busy.remove(&want.url);
                deliver(Done::Failed(want, e.text()));
                std::thread::sleep(Duration::from_secs(1));
            }
            Err(e) => {
                let mut g = lock(q);
                g.busy.remove(&want.url);
                g.gone.insert(want.url.clone());
                drop(g);
                deliver(Done::Failed(want, e.text()));
            }
        }
    }
}
