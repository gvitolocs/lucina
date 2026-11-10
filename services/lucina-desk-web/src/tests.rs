//! The host end to end, in-process: a server on 127.0.0.1 with a test pattern for a screen and a recording `Input`, a viewer
//! with `tungstenite`'s client, and the stripes decoded with FilmCraft's own decoder.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tungstenite::{Message, WebSocket};

use super::*;
use crate::input::tests::{Call, Recorder};

const W: usize = 320;
const H: usize = 240;
const PW: &str = "0123456789abcdef0123456789abcdef";

/// A picture with smooth areas and edges; the test changes it through the shared pixels.
#[derive(Clone)]
struct Pattern(Arc<Mutex<Vec<u32>>>);

impl Pattern {
    fn new() -> Pattern {
        let mut px = vec![0u32; W * H];
        for y in 0..H {
            for x in 0..W {
                let (r, g, b) = ((x * 255 / W) as u32, (y * 255 / H) as u32, ((x + y) * 128 / (W + H)) as u32);
                px[y * W + x] = 0xff00_0000 | r << 16 | g << 8 | b;
                if (40..120).contains(&x) && (60..100).contains(&y) {
                    px[y * W + x] = 0xffe0_4020;
                }
                if (200..280).contains(&x) && (150..200).contains(&y) {
                    px[y * W + x] = 0xff20_a060;
                }
            }
        }
        Pattern(Arc::new(Mutex::new(px)))
    }

    fn pixels(&self) -> Vec<u32> {
        self.0.lock().unwrap().clone()
    }

    /// One 16x16 block, filled.
    fn fill_block(&self, bx: usize, by: usize, c: u32) {
        let mut px = self.0.lock().unwrap();
        for y in by * 16..by * 16 + 16 {
            px[y * W + bx * 16..y * W + bx * 16 + 16].fill(c);
        }
    }
}

impl Screen for Pattern {
    fn size(&self) -> (usize, usize) {
        (W, H)
    }
    fn read_row(&mut self, y: usize, out: &mut [u32]) {
        out.copy_from_slice(&self.0.lock().unwrap()[y * W..(y + 1) * W]);
    }
}

fn start(read_only: bool) -> (Server, SocketAddr, Pattern, Recorder) {
    let pattern = Pattern::new();
    let rec = Recorder::default();
    let cfg = Config {
        listen: vec![Listen {
            addr: "127.0.0.1:0".parse().unwrap(),
            lan: Lan { ip: Ipv4Addr::new(127, 0, 0, 1), mask: Ipv4Addr::new(255, 0, 0, 0) },
        }],
        only: None,
        read_only,
        title: "Juno".into(),
        password: PW.into(),
    };
    let s = Server::start(cfg, Box::new(pattern.clone()), Box::new(rec.clone())).unwrap();
    let addr = s.local_addrs()[0];
    (s, addr, pattern, rec)
}

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Reply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

fn get_with_host(addr: SocketAddr, target: &str, host: &str) -> Reply {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    write!(s, "GET {target} HTTP/1.1\r\nHost: {host}\r\n\r\n").unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).unwrap();
    let end = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8_lossy(&raw[..end]).into_owned();
    let mut lines = head.lines();
    let status = lines.next().unwrap().split(' ').nth(1).unwrap().parse().unwrap();
    let headers = lines.filter_map(|l| l.split_once(": ")).map(|(k, v)| (k.to_owned(), v.to_owned())).collect();
    Reply { status, headers, body: raw[end + 4..].to_vec() }
}

fn get(addr: SocketAddr, target: &str) -> Reply {
    get_with_host(addr, target, &addr.to_string())
}

struct Viewer {
    ws: WebSocket<TcpStream>,
}

impl Viewer {
    fn connect(addr: SocketAddr) -> (Viewer, String) {
        let s = TcpStream::connect(addr).unwrap();
        let (mut ws, _) = tungstenite::client(format!("ws://{addr}/ws?token={PW}"), s).unwrap();
        ws.get_mut().set_read_timeout(Some(Duration::from_millis(50))).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match ws.read() {
                Ok(Message::Text(t)) if t.contains("\"hello\"") => return (Viewer { ws }, t.to_string()),
                Ok(_) => {}
                Err(tungstenite::Error::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => panic!("{e}"),
            }
            assert!(Instant::now() < deadline, "no hello");
        }
    }

    fn send(&mut self, t: &str) {
        self.ws.send(Message::Text(t.into())).unwrap();
    }

    /// The next video message within `timeout`; pings are answered on the way, as the web client's worker does.
    fn next_bin(&mut self, timeout: Duration) -> Option<Vec<u8>> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            match self.ws.read() {
                Ok(Message::Binary(b)) => return Some(b.to_vec()),
                Ok(Message::Text(t)) => {
                    if let Some(s) = t.strip_prefix("{\"t\":\"ping\",\"s\":") {
                        let s = s.trim_end_matches('}').to_owned();
                        self.send(&format!("po,{s}"));
                    }
                }
                Ok(_) => {}
                Err(tungstenite::Error::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => panic!("{e}"),
            }
        }
        None
    }

    /// The parts of the next whole frame, in arrival order.
    fn next_frame(&mut self, timeout: Duration) -> Option<Vec<Part>> {
        let deadline = Instant::now() + timeout;
        let mut got: Vec<Part> = Vec::new();
        loop {
            let left = deadline.checked_duration_since(Instant::now())?;
            let p = Part::parse(&self.next_bin(left)?);
            if let Some(first) = got.first() {
                assert_eq!(first.frame, p.frame, "a part of another frame before this one was whole");
            }
            let need = p.parts as usize;
            got.push(p);
            if got.len() == need {
                return Some(got);
            }
        }
    }
}

#[derive(Debug)]
struct Part {
    key: bool,
    paintover: bool,
    frame: u32,
    stripe: usize,
    count: usize,
    parts: u8,
    y: usize,
    data: Vec<u8>,
}

impl Part {
    fn parse(b: &[u8]) -> Part {
        assert!(b.len() > video::HDR);
        assert_eq!(b[0], 0x01, "type");
        assert_eq!(b[13], 0, "reserved");
        Part {
            key: b[1] & 1 == 1,
            paintover: b[1] & 2 == 2,
            frame: u32::from_be_bytes([b[2], b[3], b[4], b[5]]),
            stripe: b[10] as usize,
            count: b[11] as usize,
            parts: b[12],
            y: u16::from_be_bytes([b[14], b[15]]) as usize,
            data: b[video::HDR..].to_vec(),
        }
    }
}

/// NAL unit types of an Annex-B stream.
fn nal_types(d: &[u8]) -> Vec<u8> {
    (0..d.len().saturating_sub(3)).filter(|&i| d[i..i + 3] == [0, 0, 1]).map(|i| d[i + 3] & 0x1f).collect()
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let mse = a.iter().zip(b).map(|(&x, &y)| (x as f64 - y as f64).powi(2)).sum::<f64>() / a.len() as f64;
    if mse == 0.0 { 99.0 } else { 10.0 * (255.0f64 * 255.0 / mse).log10() }
}

/// One decoder a stripe, as the web client keeps.
struct Decoders(Vec<filmcraft_h264::Decoder>);

impl Decoders {
    fn new(n: usize) -> Decoders {
        Decoders((0..n).map(|_| filmcraft_h264::Decoder::new()).collect())
    }

    /// Decodes a part and checks the stripe's luma against the pattern's.
    fn check(&mut self, p: &Part, screen: &[u32], h: usize) -> f64 {
        let pics = self.0[p.stripe].decode(&p.data, p.frame as i64).unwrap();
        assert_eq!(pics.len(), 1, "one picture a part");
        let pic = &pics[0];
        assert_eq!((pic.width as usize, pic.height as usize), (W, h));
        // The last stripe runs past the bottom edge: compare the rows on the screen.
        let seen = h.min(H - p.y);
        let (_, _, want, _, _) = video::to_i420(&screen[p.y * W..(p.y + seen) * W], W, seen);
        let got: Vec<u8> = (0..seen).flat_map(|r| pic.y[r * pic.y_stride..r * pic.y_stride + W].to_vec()).collect();
        psnr(&got, &want)
    }
}

#[test]
fn passwords_hosts_and_the_public_files() {
    let (s, addr, _, _) = start(false);
    let r = get(addr, "/");
    assert_eq!(r.status, 401);
    assert!(r.header("WWW-Authenticate").unwrap().starts_with("Basic"));
    assert_eq!(get(addr, "/ws").status, 401);
    assert_eq!(get(addr, "/screenshot").status, 401);
    assert_eq!(get(addr, &format!("/?token={}", "f".repeat(32))).status, 401);
    let page = get(addr, &format!("/?token={PW}"));
    assert_eq!(page.status, 200);
    let html = String::from_utf8(page.body).unwrap();
    assert!(html.contains("<title>Juno</title>") && !html.contains("{{title}}"));
    for p in ["/client.js", "/worker.js", "/audio.js", "/mic.js", "/favicon.ico", "/inter.woff2", "/healthz"] {
        assert_eq!(get(addr, p).status, 200, "{p}");
    }
    assert_eq!(get(addr, "/healthz").body, b"ok");
    assert_eq!(get(addr, &format!("/worker.js?token={PW}")).header("Content-Type"), Some("text/javascript"));
    // DNS rebinding: a name for this address is turned away, even with the password.
    assert_eq!(get_with_host(addr, &format!("/?token={PW}"), "evil.example").status, 403);
    assert_eq!(get_with_host(addr, "/healthz", "evil.example").status, 403);
    // Later, and not here.
    assert_eq!(get(addr, &format!("/record?token={PW}")).status, 404);
    assert_eq!(get(addr, &format!("/files/list?token={PW}")).status, 404);
    let stats = String::from_utf8(get(addr, &format!("/stats?token={PW}")).body).unwrap();
    for k in ["captured", "encoded", "bytes", "encode_us_total", "keyframes", "paintovers", "clients"] {
        assert!(stats.contains(&format!("\"{k}\":")), "{stats}");
    }
    s.stop();
}

#[test]
fn a_viewer_gets_the_hello_a_whole_keyframe_then_only_what_changed() {
    let (s, addr, pattern, _) = start(false);
    let layout = video::stripe_layout(H as u32, video::STRIPES);
    let n = layout.len();
    assert_eq!(n, 8);
    let (mut v, hello) = Viewer::connect(addr);
    let stripes: Vec<String> = layout.iter().map(|(y, h)| format!("[{y},{h}]")).collect();
    let want = format!(
        "{{\"t\":\"hello\",\"w\":{W},\"h\":{H},\"os\":\"juno\",\"audio\":false,\"mic\":false,\"stripes\":[{}],\
         \"files\":{{\"upload\":false,\"download\":false,\"dir\":\"\"}}}}",
        stripes.join(",")
    );
    assert_eq!(hello, want);
    // Before `v,1`, nothing.
    assert!(v.next_bin(Duration::from_millis(300)).is_none());
    v.send("v,1");
    let frame = v.next_frame(Duration::from_secs(10)).expect("a keyframe");
    assert_eq!(frame.len(), n);
    let mut dec = Decoders::new(n);
    let screen = pattern.pixels();
    let mut seen = vec![false; n];
    for p in &frame {
        assert!(p.key && !p.paintover, "{p:?}");
        assert_eq!((p.count, p.parts as usize), (n, n));
        assert_eq!(p.y, layout[p.stripe].0 as usize);
        seen[p.stripe] = true;
        let t = nal_types(&p.data);
        assert!(t.contains(&7) && t.contains(&8) && t.contains(&5), "SPS, PPS and IDR: {t:?}");
        // Constrained Baseline, as WebCodecs' avc1.42e0xx.
        let sps = p.data.windows(4).position(|w| w[..3] == [0, 0, 1] && w[3] & 0x1f == 7).unwrap();
        assert_eq!(p.data[sps + 4], 0x42);
        let q = dec.check(p, &screen, layout[p.stripe].1 as usize);
        assert!(q > 30.0, "stripe {} at {q:.1} dB", p.stripe);
    }
    assert!(seen.iter().all(|s| *s));
    v.send(&format!("a,{}", frame[0].frame));
    // Still now: the paint-over burst sharpens every stripe, an IDR at CRF 16 and two refinements.
    let mut paintovers = 0;
    while let Some(f) = v.next_frame(Duration::from_millis(800)) {
        assert!(f.iter().all(|p| p.paintover), "{f:?}");
        assert_eq!(f[0].key, paintovers == 0);
        for p in &f {
            let q = dec.check(p, &screen, layout[p.stripe].1 as usize);
            assert!(q > 35.0, "paint-over of stripe {} at {q:.1} dB", p.stripe);
        }
        paintovers += 1;
        v.send(&format!("a,{}", f[0].frame));
    }
    assert_eq!(paintovers, 3);
    // One 16x16 block in stripe 3 (rows 96..128): that stripe only, as a P picture.
    pattern.fill_block(5, 6, 0xff30_30f0);
    let f = v.next_frame(Duration::from_secs(5)).expect("the change");
    assert_eq!(f.len(), 1, "{f:?}");
    let p = &f[0];
    assert_eq!((p.stripe, p.parts, p.key, p.paintover), (3, 1, false, false));
    let t = nal_types(&p.data);
    assert!(t.contains(&1) && !t.contains(&5) && !t.contains(&7), "{t:?}");
    let q = dec.check(p, &pattern.pixels(), 32);
    assert!(q > 30.0, "{q:.1} dB");
    v.send(&format!("a,{}", p.frame));
    // Nothing changes, nothing comes: it was sent sharp, so no paint-over is owed either.
    assert!(v.next_bin(Duration::from_millis(700)).is_none());
    // `kf`: a whole keyframe again.
    v.send("kf");
    let f = v.next_frame(Duration::from_secs(5)).unwrap();
    assert!(f.len() == n && f.iter().all(|p| p.key));
    s.stop();
}

#[test]
fn a_viewer_that_does_not_ack_gets_at_most_the_limit_then_the_newest_picture() {
    let (s, addr, pattern, _) = start(false);
    let (mut v, _) = Viewer::connect(addr);
    v.send("v,1");
    let key = v.next_frame(Duration::from_secs(10)).unwrap();
    let busy = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let (p2, b2) = (pattern.clone(), busy.clone());
    let changer = std::thread::spawn(move || {
        let mut i = 0u32;
        while b2.load(std::sync::atomic::Ordering::SeqCst) {
            p2.fill_block(1, 1, 0xff00_0000 | i.wrapping_mul(0x010203));
            i += 1;
            std::thread::sleep(Duration::from_millis(5));
        }
    });
    // From the keyframe on, nothing is acknowledged: at most the limit comes. (voxl's other rule, the oldest unacknowledged
    // frame no older than the RTT plus 120 ms, may stop it sooner on a busy machine; `flow_ok` has its own test.)
    v.send(&format!("a,{}", key[0].frame));
    let mut frames = Vec::new();
    while let Some(f) = v.next_frame(Duration::from_millis(1000)) {
        frames.push(f[0].frame);
    }
    assert!((2..=video::MAX_UNACKED).contains(&frames.len()), "{frames:?}");
    // Acked, it goes on, and what comes is the newest picture of the stripe.
    busy.store(false, std::sync::atomic::Ordering::SeqCst);
    changer.join().unwrap();
    v.send(&format!("a,{}", frames.last().unwrap()));
    let mut last = None;
    while let Some(f) = v.next_frame(Duration::from_millis(500)) {
        v.send(&format!("a,{}", f[0].frame));
        last = Some(f);
    }
    let last = last.expect("frames after the ack");
    assert!(last.iter().any(|p| p.stripe == 0));
    let stats = String::from_utf8(get(addr, &format!("/stats?token={PW}")).body).unwrap();
    assert!(!stats.contains("\"flow_blocked\":0"), "{stats}");
    s.stop();
}

fn wait_for(rec: &Recorder, n: usize) -> Vec<Call> {
    let deadline = Instant::now() + Duration::from_secs(3);
    while rec.0.lock().unwrap().len() < n && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(50));
    rec.take()
}

#[test]
fn commands_drive_the_input_and_a_read_only_host_drops_them() {
    use crate::input::{BTN_LEFT, KEY_LEFTSHIFT};
    use Call::*;
    let (s, addr, _, rec) = start(false);
    let (mut v, _) = Viewer::connect(addr);
    v.send("m,10,20");
    v.send("m,5000,-3");
    v.send("b,1,1");
    v.send("b,1,0");
    v.send("sp,0.0,250.0");
    // "!" without Shift: Shift is pressed around it. "a" with Shift held: lifted around it.
    v.send("k,33,1");
    v.send("k,33,0");
    v.send("k,65505,1");
    v.send("k,97,1");
    assert_eq!(
        wait_for(&rec, 15),
        vec![
            Move(10, 20),
            Move(W as i32 - 1, 0),
            Button(BTN_LEFT, true),
            Button(BTN_LEFT, false),
            Wheel(0, 2),
            Key(KEY_LEFTSHIFT, true),
            Key(2, true),
            Key(KEY_LEFTSHIFT, false),
            Key(2, false),
            Key(KEY_LEFTSHIFT, true),
            Key(KEY_LEFTSHIFT, false),
            Key(30, true),
            Key(KEY_LEFTSHIFT, true),
        ]
    );
    v.send("r");
    let mut up = wait_for(&rec, 2);
    up.sort();
    assert_eq!(up, vec![Key(30, false), Key(KEY_LEFTSHIFT, false)]);
    // Held keys are let go when the viewer leaves.
    v.send("k,98,1");
    assert_eq!(wait_for(&rec, 1), vec![Key(48, true)]);
    drop(v);
    assert_eq!(wait_for(&rec, 1), vec![Key(48, false)]);
    s.stop();

    let (s, addr, _, rec) = start(true);
    let (mut v, hello) = Viewer::connect(addr);
    assert!(hello.contains("\"readOnly\":true"));
    for c in ["m,10,20", "b,1,1", "s,0,1", "sp,0,500", "k,97,1", "r"] {
        v.send(c);
    }
    // The picture still comes.
    v.send("v,1");
    assert!(v.next_frame(Duration::from_secs(10)).is_some());
    assert!(rec.take().is_empty());
    s.stop();
}

#[test]
fn screenshots_are_the_screen_and_waits_see_changes() {
    let (s, addr, pattern, _) = start(false);
    let r = get(addr, &format!("/screenshot?token={PW}"));
    assert_eq!(r.status, 200);
    assert_eq!(r.header("Content-Type"), Some("image/png"));
    let seq: u64 = r.header("X-Voxl-Seq").unwrap().parse().unwrap();
    let mut d = png::Decoder::new(std::io::Cursor::new(r.body)).read_info().unwrap();
    let mut buf = vec![0u8; d.output_buffer_size().unwrap()];
    let info = d.next_frame(&mut buf).unwrap();
    assert_eq!((info.width as usize, info.height as usize, info.color_type), (W, H, png::ColorType::Rgb));
    let want: Vec<u8> = pattern.pixels().iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8]).collect();
    assert!(buf[..W * H * 3] == want[..], "the screenshot differs from the screen");

    // Nothing changes: a wait for a change times out.
    let t = Instant::now();
    let r = get(addr, &format!("/wait?token={PW}&change&since={seq}&timeout=400"));
    let body = String::from_utf8(r.body).unwrap();
    assert!(body.contains("\"changed\":false") && body.contains("\"timed_out\":true"), "{body}");
    assert!(t.elapsed() >= Duration::from_millis(400));
    // A change while waiting ends the wait.
    let p2 = pattern.clone();
    let changer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        p2.fill_block(2, 2, 0xffff_ffff);
    });
    let r = get(addr, &format!("/wait?token={PW}&change&timeout=5000"));
    changer.join().unwrap();
    let body = String::from_utf8(r.body).unwrap();
    assert!(body.contains("\"changed\":true") && body.contains("\"timed_out\":false"), "{body}");
    assert_eq!(get(addr, &format!("/wait?token={PW}&region=1,2")).status, 400);
    s.stop();
}

#[test]
fn more_connections_than_the_limit_get_503() {
    let (s, addr, _, _) = start(false);
    // Four long waits hold every slot.
    let waits: Vec<_> = (0..MAX_CONNECTIONS)
        .map(|_| std::thread::spawn(move || get(addr, &format!("/wait?token={PW}&change&timeout=3000")).status))
        .collect();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(get(addr, "/healthz").status, 503);
    for w in waits {
        assert_eq!(w.join().unwrap(), 200);
    }
    assert_eq!(get(addr, "/healthz").status, 200);
    s.stop();
}
