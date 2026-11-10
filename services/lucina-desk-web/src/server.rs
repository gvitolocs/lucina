//! HTTP/1.1 and the WebSocket on plain threads: voxl's `server.rs` routes and its socket protocol, without axum or tokio. Juno
//! runs static musl programs on Asterinas' Linux ABI, where threads and blocking sockets are proven; a thread a connection,
//! at most `MAX_CONNECTIONS` at once, and every request answers `Connection: close`.

use std::collections::VecDeque;
use std::io::{BufWriter, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::hub::{Client, Hub, Out, lock};
use crate::login::{Login, RequestFacts, Verdict};
use crate::ws;

const HEAD_LIMIT: usize = 16 << 10;
/// Nothing is uploaded to this host: no message from a viewer is anywhere near this.
pub const MAX_MESSAGE: usize = 1 << 20;
/// A request's head must arrive in this time; a viewer must answer pings (every 500 ms) within the longer one.
const HEAD_TIMEOUT: Duration = Duration::from_secs(10);
const SOCKET_IDLE: Duration = Duration::from_secs(30);
/// A viewer that stops reading is dropped rather than holding a thread.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const PING_EVERY: Duration = Duration::from_millis(500);

/// voxl's `READ_ONLY_COMMANDS`: what a read-only viewer may send. Everything else acts on the desktop and is dropped here,
/// whatever the page does with the hello's `readOnly`.
const READ_ONLY_COMMANDS: &[&str] = &["a", "v", "au", "kf", "po"];

pub struct Assets {
    pub index: Vec<u8>,
}

/// voxl's `index_html`: the page with its title filled in, HTML-escaped.
pub fn index_html(title: &str) -> Vec<u8> {
    let mut escaped = String::with_capacity(title.len());
    for c in title.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            c => escaped.push(c),
        }
    }
    include_str!("../web/index.html").replace("{{title}}", &escaped).into_bytes()
}

const CLIENT_JS: &[u8] = include_bytes!("../web/client.js");
const WORKER_JS: &[u8] = include_bytes!("../web/worker.js");
const AUDIO_JS: &[u8] = include_bytes!("../web/audio.js");
const MIC_JS: &[u8] = include_bytes!("../web/mic.js");
const FAVICON: &[u8] = include_bytes!("../web/favicon.ico");
const INTER: &[u8] = include_bytes!("../web/inter.woff2");

struct Request {
    path: String,
    query: String,
    headers: Vec<(String, String)>,
    method: String,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

/// Reads up to the end of the head. Returns the head parsed and whatever came after it.
fn read_request(s: &mut TcpStream) -> Result<(Request, Vec<u8>), String> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 2048];
    loop {
        let n = s.read(&mut chunk).map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("closed before a request".into());
        }
        buf.extend_from_slice(&chunk[..n]);
        let mut headers = [httparse::EMPTY_HEADER; 64];
        let mut req = httparse::Request::new(&mut headers);
        match req.parse(&buf) {
            Ok(httparse::Status::Complete(len)) => {
                let target = req.path.unwrap_or("/");
                let (path, query) = target.split_once('?').unwrap_or((target, ""));
                let r = Request {
                    path: path.to_owned(),
                    query: query.to_owned(),
                    method: req.method.unwrap_or("").to_owned(),
                    headers: req
                        .headers
                        .iter()
                        .map(|h| (h.name.to_owned(), String::from_utf8_lossy(h.value).into_owned()))
                        .collect(),
                };
                return Ok((r, buf[len..].to_vec()));
            }
            Ok(httparse::Status::Partial) if buf.len() < HEAD_LIMIT => {}
            Ok(httparse::Status::Partial) => return Err("request head too long".into()),
            Err(e) => return Err(format!("bad request: {e}")),
        }
    }
}

fn respond(s: &mut TcpStream, status: &str, ctype: &str, extra: &[(&str, &str)], body: &[u8]) {
    let mut head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-cache\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in extra {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    let _ = s.write_all(head.as_bytes()).and_then(|_| s.write_all(body)).and_then(|_| s.flush());
}

fn text(s: &mut TcpStream, status: &str, body: &str) {
    respond(s, status, "text/plain; charset=utf-8", &[], body.as_bytes());
}

fn json(s: &mut TcpStream, status: &str, body: &str) {
    respond(s, status, "application/json", &[("Cache-Control", "no-store")], body.as_bytes());
}

/// One connection, from its first byte to its close.
pub fn handle(mut s: TcpStream, hub: Arc<Hub>, login: Arc<Mutex<Login>>, assets: Arc<Assets>) {
    let _ = s.set_read_timeout(Some(HEAD_TIMEOUT));
    let _ = s.set_write_timeout(Some(WRITE_TIMEOUT));
    let (req, rest) = match read_request(&mut s) {
        Ok(r) => r,
        Err(e) => {
            if !e.starts_with("closed") {
                text(&mut s, "400 Bad Request", &e);
            }
            return;
        }
    };
    let facts = RequestFacts {
        path: &req.path,
        query: &req.query,
        host: req.header("host"),
        origin: req.header("origin"),
        authorization: req.header("authorization"),
    };
    match lock(&login).check(&facts, Instant::now()) {
        Verdict::Allow => {}
        Verdict::Forbidden => return text(&mut s, "403 Forbidden", "this host answers only to its IP address"),
        Verdict::LockedOut => {
            return text(&mut s, "429 Too Many Requests", "too many wrong passwords: try again in a minute");
        }
        Verdict::Unauthorized => {
            return respond(
                &mut s,
                "401 Unauthorized",
                "text/plain; charset=utf-8",
                &[("WWW-Authenticate", "Basic realm=\"Juno\", charset=\"UTF-8\"")],
                b"Juno needs its password: open the link the Remote desktop app shows, sign in when the browser asks, \
                  or send Authorization: Bearer <password>",
            );
        }
    }
    if req.method != "GET" {
        return text(&mut s, "405 Method Not Allowed", "only GET");
    }
    let js = "text/javascript";
    match req.path.as_str() {
        "/" | "/index.html" => respond(&mut s, "200 OK", "text/html; charset=utf-8", &[], &assets.index),
        "/client.js" => respond(&mut s, "200 OK", js, &[], CLIENT_JS),
        "/worker.js" => respond(&mut s, "200 OK", js, &[], WORKER_JS),
        "/audio.js" => respond(&mut s, "200 OK", js, &[], AUDIO_JS),
        "/mic.js" => respond(&mut s, "200 OK", js, &[], MIC_JS),
        "/favicon.ico" => respond(&mut s, "200 OK", "image/x-icon", &[], FAVICON),
        "/inter.woff2" => respond(&mut s, "200 OK", "font/woff2", &[], INTER),
        "/healthz" => respond(&mut s, "200 OK", "text/plain; charset=utf-8", &[], b"ok"),
        "/stats" => json(&mut s, "200 OK", &hub.stats_json()),
        "/screenshot" => screenshot(&mut s, &hub),
        "/wait" => {
            hub.waiters.fetch_add(1, Ordering::SeqCst);
            hub.wake();
            let (code, body) = hub.activity.wait(&req.query);
            hub.waiters.fetch_sub(1, Ordering::SeqCst);
            json(&mut s, if code == 200 { "200 OK" } else { "400 Bad Request" }, &body);
        }
        "/ws" => websocket(s, &req, rest, hub),
        // voxl's recordings (`/record`) and file transfer (`/files/*`): later.
        _ => text(&mut s, "404 Not Found", "not here"),
    }
}

/// voxl's `record::screenshot`: the screen as the capture thread reads it now, as a PNG, with the `/wait` seq it covers.
fn screenshot(s: &mut TcpStream, hub: &Hub) {
    let (tx, rx) = mpsc::channel();
    lock(&hub.snapshots).push(tx);
    hub.wake();
    let Ok(shot) = rx.recv_timeout(Duration::from_secs(5)) else {
        return json(s, "503 Service Unavailable", "{\"error\":\"no frame from capture\"}");
    };
    let rgb: Vec<u8> = shot.xrgb.iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8]).collect();
    let mut png = Vec::new();
    let r = {
        let mut enc = png::Encoder::new(&mut png, shot.width as u32, shot.height as u32);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Fast);
        enc.write_header().and_then(|mut w| w.write_image_data(&rgb))
    };
    match r {
        Ok(()) => respond(s, "200 OK", "image/png", &[("Cache-Control", "no-store"), ("X-Voxl-Seq", &shot.seq.to_string())], &png),
        Err(e) => json(s, "500 Internal Server Error", &format!("{{\"error\":\"png: {e}\"}}")),
    }
}

fn websocket(mut s: TcpStream, req: &Request, rest: Vec<u8>, hub: Arc<Hub>) {
    let upgrade = req.header("upgrade").is_some_and(|v| v.eq_ignore_ascii_case("websocket"));
    let connection = req.header("connection").is_some_and(|v| v.split(',').any(|t| t.trim().eq_ignore_ascii_case("upgrade")));
    let (Some(key), true, true) = (req.header("sec-websocket-key"), upgrade, connection) else {
        return text(&mut s, "400 Bad Request", "a WebSocket upgrade is expected here");
    };
    if req.header("sec-websocket-version").map(str::trim) != Some("13") {
        return respond(&mut s, "426 Upgrade Required", "text/plain", &[("Sec-WebSocket-Version", "13")], b"");
    }
    let head = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
        ws::accept_key(key)
    );
    if s.write_all(head.as_bytes()).is_err() {
        return;
    }
    let _ = s.set_nodelay(true);
    let _ = s.set_read_timeout(Some(SOCKET_IDLE));
    let (Ok(wstream), Ok(cstream)) = (s.try_clone(), s.try_clone()) else { return };
    let (tx, rx) = mpsc::channel::<Out>();
    let client = hub.add_client(tx, cstream);
    eprintln!("lucina-desk-web: viewer {} connected", client.id);
    client.send(Out::Text(hub.hello()));
    let epoch = hub.epoch;
    let writer = std::thread::Builder::new().name("desk-web-write".into()).spawn(move || write_loop(wstream, rx, epoch));
    if writer.is_err() {
        hub.remove_client(client.id);
        return;
    }
    let mut reader = std::io::Cursor::new(rest).chain(&s);
    let mut rtts = VecDeque::new();
    let mut messages = ws::Reader::default();
    loop {
        match messages.read(&mut reader, MAX_MESSAGE) {
            Ok(ws::Message::Text(t)) => handle_text(&hub, &client, &t, &mut rtts),
            Ok(ws::Message::Ping(p)) => client.send(Out::Pong(p)),
            // Binary from a viewer is clipboard images and the microphone, which this host does not take.
            Ok(ws::Message::Binary(_) | ws::Message::Pong) => {}
            Ok(ws::Message::Close) | Err(_) => break,
        }
    }
    lock(&hub.input).release_all();
    hub.remove_client(client.id);
    let _ = s.shutdown(Shutdown::Both);
    // The writer ends when the hub drops the client and with it the channel's last sender, or at its next write.
    if let Ok(w) = writer {
        drop(client);
        let _ = w.join();
    }
    eprintln!("lucina-desk-web: a viewer left");
}

/// Sends what the hub and the reader queue, and a ping every 500 ms (`{"t":"ping","s":<µs since start>}`), as voxl's.
fn write_loop(s: TcpStream, rx: mpsc::Receiver<Out>, epoch: Instant) {
    let mut w = BufWriter::with_capacity(64 << 10, &s);
    let mut next_ping = Instant::now();
    loop {
        let now = Instant::now();
        let r = if now >= next_ping {
            next_ping = now + PING_EVERY;
            let ping = format!("{{\"t\":\"ping\",\"s\":{}}}", epoch.elapsed().as_micros());
            ws::write_frame(&mut w, ws::TEXT, ping.as_bytes())
        } else {
            match rx.recv_timeout(next_ping - now) {
                Ok(Out::Text(t)) => ws::write_frame(&mut w, ws::TEXT, t.as_bytes()),
                Ok(Out::Bin(b)) => ws::write_frame(&mut w, ws::BINARY, &b),
                Ok(Out::Pong(p)) => ws::write_frame(&mut w, ws::PONG, &p),
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    let _ = ws::write_frame(&mut w, ws::CLOSE, &[]);
                    break;
                }
            }
        };
        if r.and_then(|_| w.flush()).is_err() {
            break;
        }
    }
    drop(w);
    let _ = s.shutdown(Shutdown::Both);
}

fn num<T: std::str::FromStr>(it: &mut std::str::Split<'_, char>) -> Option<T> {
    it.next()?.trim().parse().ok()
}

/// A viewer's command, as voxl's `handle_text`. `c` (clipboard), `au` (audio), `mi` (microphone) and `sz` (resize) are
/// ignored: this host offers none of them.
pub fn handle_text(hub: &Hub, c: &Arc<Client>, t: &str, rtts: &mut VecDeque<u32>) {
    let (cmd, rest) = t.split_once(',').unwrap_or((t, ""));
    if hub.read_only && !READ_ONLY_COMMANDS.contains(&cmd) {
        return;
    }
    let mut it = rest.split(',');
    match cmd {
        "a" => {
            if let Some(id) = num::<u32>(&mut it) {
                c.on_ack(id);
                hub.wake();
            }
        }
        "m" => {
            if let (Some(x), Some(y)) = (num::<i32>(&mut it), num::<i32>(&mut it)) {
                let (x, y) = (x.clamp(0, hub.width as i32 - 1), y.clamp(0, hub.height as i32 - 1));
                lock(&hub.input).move_to(x, y);
            }
        }
        "b" => {
            if let (Some(b), Some(d)) = (num::<u8>(&mut it), num::<u8>(&mut it)) {
                lock(&hub.input).button(b, d == 1);
            }
        }
        "s" => {
            if let (Some(dx), Some(dy)) = (num::<i32>(&mut it), num::<i32>(&mut it)) {
                lock(&hub.input).scroll(dx, dy);
            }
        }
        "sp" => {
            if let (Some(dx), Some(dy)) = (num::<f64>(&mut it), num::<f64>(&mut it)) {
                lock(&hub.input).scroll_px(dx, dy);
            }
        }
        "k" => {
            if let (Some(keysym), Some(d)) = (num::<u32>(&mut it), num::<u8>(&mut it)) {
                lock(&hub.input).key(keysym, d == 1);
            }
        }
        "r" => lock(&hub.input).release_all(),
        "v" => {
            let on = rest == "1";
            c.video.store(on, Ordering::Relaxed);
            if on {
                lock(&c.inflight).clear();
                c.started.store(false, Ordering::Relaxed);
                c.needs_keyframe.store(true, Ordering::Relaxed);
            }
            hub.wake();
        }
        "kf" => {
            c.started.store(false, Ordering::Relaxed);
            c.needs_keyframe.store(true, Ordering::Relaxed);
            hub.wake();
        }
        "po" => {
            if let Some(sent) = num::<u64>(&mut it) {
                let now = hub.epoch.elapsed().as_micros() as u64;
                let rtt = (now.saturating_sub(sent) / 1000) as u32;
                rtts.push_back(rtt.max(1));
                if rtts.len() > 20 {
                    rtts.pop_front();
                }
                c.min_rtt_ms.store(rtts.iter().copied().min().unwrap_or(0), Ordering::Relaxed);
            }
        }
        _ => {}
    }
}
