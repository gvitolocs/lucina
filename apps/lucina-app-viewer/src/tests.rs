use super::*;
use lucina_app::Harness;

fn png(path: &Path, w: u32, h: u32, rgb: [u8; 3]) {
    // A stored deflate block would do, but the toolkit's own decoder must read it: write it with a tiny encoder.
    let mut out = Vec::new();
    out.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let mut crc = !0u32;
        for &b in kind.iter().chain(data) {
            crc ^= b as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
            }
        }
        out.extend_from_slice(&(!crc).to_be_bytes());
    }
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    let mut raw = Vec::new();
    for _ in 0..h {
        raw.push(0);
        for _ in 0..w {
            raw.extend_from_slice(&rgb);
        }
    }
    let mut z = vec![0x78, 0x01];
    let mut chunks = raw.chunks(65535).peekable();
    while let Some(c) = chunks.next() {
        z.push(chunks.peek().is_none() as u8);
        z.extend_from_slice(&(c.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(c.len() as u16)).to_le_bytes());
        z.extend_from_slice(c);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    z.extend_from_slice(&(b << 16 | a).to_be_bytes());
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    std::fs::write(path, out).unwrap();
}

fn folder(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lucina-viewer-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("sub")).unwrap();
    png(&d.join("a.png"), 40, 30, [255, 0, 0]);
    png(&d.join("b.png"), 40, 30, [0, 255, 0]);
    png(&d.join("c.png"), 40, 30, [0, 0, 255]);
    std::fs::write(d.join("notes.txt"), "not a picture").unwrap();
    std::fs::write(d.join("z-broken.jpg"), "not a JPEG at all").unwrap();
    d
}

/// Runs the decoding on the test's thread, so that the screens can be looked at without waiting.
struct Rig {
    h: Harness<Viewer>,
    rx: mpsc::Receiver<Job>,
}

impl Rig {
    fn new(target: Option<PathBuf>) -> Rig {
        let (jobs, rx) = mpsc::channel();
        Rig { h: Harness::new(Viewer::new(Some(jobs), (400, 300), target), 400, 300), rx }
    }

    fn pump(&mut self) {
        while let Ok(job) = self.rx.try_recv() {
            let msgs = run_job(&job);
            for m in msgs {
                self.h.core.message(m);
            }
        }
        self.h.core.paint();
    }

    fn centre(&self) -> u32 {
        self.h.core.frame.rows(Rect::new(200, 150, 1, 1)).next().unwrap().1[0] & 0xffffff
    }
}

/// `decode_one` without a `Sender`: same steps.
fn run_job(job: &Job) -> Vec<Msg> {
    let bytes = match std::fs::read(&job.path) {
        Ok(b) => b,
        Err(e) => return vec![Msg::Failed { path: job.path.clone(), error: e.to_string() }],
    };
    match load(&bytes, |_| ()) {
        Ok(i) => {
            let i = flattened(i, BG);
            let fitted = i.fit_within(job.screen.0, job.screen.1);
            let stats = Stats { read_ms: 0, decode_ms: 0, fit_ms: 0, size: (i.w, i.h), bytes: bytes.len() as u64 };
            vec![Msg::Ready { path: job.path.clone(), fitted, full: job.keep_full.then_some(i), stats }]
        }
        Err(e) => vec![Msg::Failed { path: job.path.clone(), error: format!("Cannot show this picture: {e}") }],
    }
}

#[test]
fn the_list_shows_folders_and_pictures_only() {
    let d = folder("list");
    let (rows, pictures) = list_folder(&d);
    assert_eq!(pictures.len(), 4, "a b c and the broken one, by name");
    assert!(matches!(rows[0], Row::Up(_)));
    assert!(matches!(&rows[1], Row::Dir(n, _) if n == "sub"));
    assert!(!rows.iter().any(|r| matches!(r, Row::Picture(n, _) if n == "notes.txt")));
}

#[test]
fn opens_from_the_list_walks_and_goes_back() {
    let d = folder("walk");
    let mut r = Rig::new(Some(d.clone()));
    r.h.key(keys::KEY_TAB);
    // Up, sub, a.png.
    r.h.key(keys::KEY_DOWN);
    r.h.key(keys::KEY_DOWN);
    r.h.key(keys::KEY_ENTER);
    assert_eq!(r.h.app().index, Some(0));
    r.pump();
    assert_eq!(r.centre(), 0xff0000);
    // The neighbour was decoded with it, so the next one is there at once.
    assert!(r.h.app().fitted.contains_key(&d.join("b.png")));
    r.h.key(keys::KEY_RIGHT);
    assert_eq!(r.centre(), 0x00ff00);
    r.pump();
    r.h.key(keys::KEY_RIGHT);
    r.pump();
    assert_eq!(r.centre(), 0x0000ff);
    r.h.key(keys::KEY_LEFT);
    r.pump();
    assert_eq!(r.centre(), 0x00ff00);
    r.h.key(keys::KEY_ESC);
    assert_eq!(r.h.app().index, None);
    assert!(r.h.app().fitted.is_empty());
}

#[test]
fn zoomed_arrows_pan_instead_of_walking() {
    let d = folder("pan");
    let mut r = Rig::new(Some(d.join("a.png")));
    r.pump();
    r.h.key(13);
    assert!(r.h.app().picture.is_zoomed());
    r.h.key(keys::KEY_RIGHT);
    assert_eq!(r.h.app().index, Some(0));
    r.h.key(keys::KEY_ESC);
    assert!(!r.h.app().picture.is_zoomed());
    assert_eq!(r.h.app().index, Some(0));
    r.h.key(keys::KEY_RIGHT);
    assert_eq!(r.h.app().index, Some(1));
}

#[test]
fn a_damaged_file_says_so_and_can_be_walked_past() {
    let d = folder("broken");
    let mut r = Rig::new(Some(d.join("z-broken.jpg")));
    r.pump();
    assert!(r.h.app().failed.as_deref().unwrap_or("").starts_with("Cannot show"));
    r.h.key(keys::KEY_LEFT);
    r.pump();
    assert_eq!(r.h.app().failed, None);
    assert_eq!(r.centre(), 0x0000ff);
}

#[test]
fn a_neighbour_decodes_again_only_when_zoomed() {
    let d = folder("lazy");
    let mut r = Rig::new(Some(d.join("a.png")));
    r.pump();
    r.h.key(keys::KEY_RIGHT);
    // b.png came as a neighbour: shown, with nothing to decode.
    let asked_for_b = std::iter::from_fn(|| r.rx.try_recv().ok()).any(|j| j.path == d.join("b.png"));
    assert!(!asked_for_b);
    assert!(r.h.app().picture.source().is_none());
    r.h.key(13);
    assert!(r.h.app().picture.is_zoomed());
    r.pump();
    assert!(r.h.app().picture.source().is_some());
}

#[test]
fn opened_on_a_file_escape_ends_the_app() {
    let d = folder("quit");
    let mut r = Rig::new(Some(d.join("a.png")));
    r.pump();
    r.h.key(keys::KEY_ESC);
    assert!(r.h.core.quit_requested());
}

#[test]
fn a_missing_folder_is_an_empty_list() {
    let mut r = Rig::new(Some(PathBuf::from("/nonexistent/lucina")));
    r.pump();
    assert!(r.h.app().pictures.is_empty());
}

#[test]
fn the_ends_stop_home_and_end_jump_and_escape_selects_the_picture_in_the_list() {
    let d = folder("ends");
    let mut r = Rig::new(Some(d.clone()));
    // Up, sub, a, b, c, z-broken.
    r.h.key(keys::KEY_END);
    r.h.key(keys::KEY_DOWN);
    r.h.key(keys::KEY_ENTER);
    assert_eq!(r.h.app().index, Some(3), "End then Down stays on the last row: no wrap");
    r.h.key(keys::KEY_RIGHT);
    assert_eq!(r.h.app().index, Some(3), "the last picture: Right stays");
    r.h.key(keys::KEY_HOME);
    assert_eq!(r.h.app().index, Some(0));
    r.h.key(keys::KEY_LEFT);
    assert_eq!(r.h.app().index, Some(0), "the first picture: Left stays");
    r.h.key(keys::KEY_END);
    assert_eq!(r.h.app().index, Some(3));
    r.h.key(keys::KEY_LEFT);
    r.h.key(keys::KEY_ESC);
    assert_eq!(r.h.app().index, None);
    let at = r.h.app().list.selected.unwrap();
    assert!(matches!(&r.h.app().rows[at], Row::Picture(n, _) if n == "c.png"), "the list has the picture we left on selected");
}

#[test]
fn the_list_goes_up_with_backspace_and_alt_left_and_keeps_left_and_right() {
    let d = folder("up");
    let mut r = Rig::new(Some(d.join("sub")));
    assert_eq!(r.h.app().folder, d.join("sub"));
    r.h.key(keys::KEY_RIGHT);
    r.h.key(keys::KEY_LEFT);
    r.h.key(keys::KEY_DOWN);
    assert_eq!(r.h.app().folder, d.join("sub"), "Left and Right did not leave the list");
    r.h.press(keys::KEY_LEFT, keys::Mods { alt: true, ..keys::Mods::default() });
    assert_eq!(r.h.app().folder, d);
    let at = r.h.app().list.selected.unwrap();
    assert!(matches!(&r.h.app().rows[at], Row::Dir(n, _) if n == "sub"), "the folder we came from is selected");
    r.h.key(keys::KEY_ENTER);
    assert_eq!(r.h.app().folder, d.join("sub"));
    r.h.key(keys::KEY_BACKSPACE);
    assert_eq!(r.h.app().folder, d);
}
