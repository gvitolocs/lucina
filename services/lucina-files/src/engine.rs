//! The real `lucina-media` for tests: the server the apps talk to through p1, on temporary folders and a port of its own, with no key and
//! no contact with the live library. The program comes from `LUCINA_MEDIA_BIN` and from nowhere else — a deployed binary whose version this
//! commit does not pin is not a server under test — so [`Engine::start`] answers `None` unless that is set, and every test that needs it is
//! `#[ignore]`d as "needs LUCINA_MEDIA_BIN" and counted as ignored rather than as passed.

use std::collections::BTreeSet;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::fake::painted;
use crate::media::{Connect, Media, local_connect};

/// The routes whose JSON shape the sample library is held against: the ones
/// `the_sample_library_answers_in_the_real_servers_shape` asks the real server about, the ones
/// `tests/fixtures/server-keys.json` was captured over, and the ones its hermetic twin compares `Fake` to.
pub const SHAPE_ROUTES: [&str; 7] = [
    "/v1/media/albums?scope=gallery",
    "/v1/media/drive",
    "/v1/media/drive/browse?folder=Lezioni&limit=200",
    "/v1/media/items?scope=gallery&origin=all&kind=photo,video&limit=1",
    "/v1/media/items?scope=gallery&origin=all&kind=photo,video&limit=2",
    "/v1/media/items/months?origin=all&kind=photo,video",
    "/v1/media/sources",
];

/// The JSON key paths of a response, recursively and sorted: `a.b` under a nested object, `a[].b` under every
/// element of a list. No values and no indexes, so a set says how a route answers and nothing about what it
/// answered; the element count is not part of it either way, since an empty list contributes only its own key.
/// [`SHAPE_ROUTES`] captured with this is `tests/fixtures/server-keys.json`.
pub fn key_paths(v: &Value) -> BTreeSet<String> {
    fn walk(v: &Value, at: &str, out: &mut BTreeSet<String>) {
        match v {
            Value::Object(o) => {
                for (k, x) in o {
                    let p = if at.is_empty() {
                        k.clone()
                    } else {
                        format!("{at}.{k}")
                    };
                    out.insert(p.clone());
                    walk(x, &p, out);
                }
            }
            Value::Array(a) => {
                for x in a {
                    walk(x, &format!("{at}[]"), out);
                }
            }
            _ => {}
        }
    }
    let mut out = BTreeSet::new();
    walk(v, "", &mut out);
    out
}

pub struct Engine {
    child: Child,
    dir: PathBuf,
    pub base: String,
}

/// The server to test against: `LUCINA_MEDIA_BIN`, named by whoever asks for this, and nothing found on its own.
fn program() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("LUCINA_MEDIA_BIN")?)).filter(|p| p.is_file())
}

fn free_port() -> Option<u16> {
    Some(
        TcpListener::bind("127.0.0.1:0")
            .ok()?
            .local_addr()
            .ok()?
            .port(),
    )
}

fn write(p: &Path, b: &[u8]) {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).expect("make a folder");
    }
    std::fs::write(p, b).expect("write a seed file");
}

impl Engine {
    /// Six photos in an album called Holiday, a folder called Lezioni in the Drive. `None` where `LUCINA_MEDIA_BIN` names no program, or
    /// where that program does not come up and list what was seeded.
    pub fn start(name: &str) -> Option<Engine> {
        let exe = program()?;
        let dir = std::env::temp_dir().join(format!("lucina-media-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for s in ["home", "data", "store"] {
            std::fs::create_dir_all(dir.join(s)).ok()?;
        }
        for i in 0..6u32 {
            let (w, h) = if i == 4 { (480, 640) } else { (800, 600) };
            write(&dir.join(format!("photos/p{i}.png")), &painted(i, w, h));
        }
        let drive = dir.join("drive/Lezioni");
        write(
            &drive.join("notes.txt"),
            b"hello from the engine\nsecond line\n",
        );
        write(
            &drive.join("Sub/main.rs"),
            b"fn main() {\n    println!(\"hi\");\n}\n",
        );
        write(&drive.join("data.bin"), &[1, 2, 3, 4, 5]);
        write(&drive.join("board.png"), &painted(9, 640, 480));
        write(&drive.join("a.zip"), &zip_with_one_file());
        let port = free_port()?;
        let internal = free_port()?;
        let env = |c: &mut Command| {
            c.env("HOME", dir.join("home"))
                .env("LUCINA_MEDIA_DATA_DIR", dir.join("data"))
                .env("LUCINA_MEDIA_STORE_DIR", dir.join("store"))
                .env("LUCINA_MEDIA_LISTEN", format!("127.0.0.1:{port}"))
                .env(
                    "LUCINA_MEDIA_INTERNAL_LISTEN",
                    format!("127.0.0.1:{internal}"),
                )
                .env(
                    "LUCINA_MEDIA_FOLDERS",
                    format!("Lezioni={}", drive.display()),
                )
                .env("LUCINA_MEDIA_PREPARE", "0")
                .env("LUCINA_MEDIA_PREPARE_SCOPE", "0")
                .env("LUCINA_MEDIA_VAAPI", "0")
                .env("LUCINA_MEDIA_GPU_LEASE", "0")
                .env("LUCINA_MEDIA_SOFFICE", "0")
                .env("LUCINA_MEDIA_ML", "0")
                .env("RUST_LOG", "warn")
                .stdout(Stdio::null())
                .stderr(Stdio::null());
        };
        let mut init = Command::new(&exe);
        init.arg("init");
        env(&mut init);
        init.status().ok()?.success().then_some(())?;
        let mut import = Command::new(&exe);
        import.arg("import").arg(dir.join("photos")).args([
            "--source",
            "phone-test",
            "--album",
            "Holiday",
        ]);
        env(&mut import);
        import.status().ok()?.success().then_some(())?;
        // `timeout`: a test killed outright leaves nothing running for long.
        let mut serve = Command::new("timeout");
        serve.arg("300").arg(&exe).arg("serve");
        env(&mut serve);
        let child = serve.spawn().ok()?;
        let e = Engine {
            child,
            dir,
            base: format!("http://127.0.0.1:{port}"),
        };
        // The server's own deadline: it is up, or it is not.
        let boot = Instant::now() + Duration::from_secs(30);
        while TcpStream::connect(("127.0.0.1", port)).is_err() {
            if Instant::now() > boot {
                return None;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        // The folder is indexed in the background. Not waited for as a count, though: this fixture sets
        // `LUCINA_MEDIA_PREPARE=0`, and the server's `prepareScopeEnabled()` (`src/modules/library/indexer/
        // folder-scan.c:75`, `if (scope == "") return false;`) reads an empty scope as "nothing to prepare",
        // so `folders[].files` stays 0 however long the wait. What the seeding does prove is the listing, so
        // the five names of the folder are what is waited for — here, on a deadline of its own, never
        // charged to the boot above.
        let mut m = (e.connect())()?;
        let indexed = Instant::now() + Duration::from_secs(30);
        // The four files and the one folder the seeding wrote, by name.
        let listed = |m: &mut Box<dyn Media>| {
            let url = crate::drive::browse_url("Lezioni", "", &crate::drive::Filters::NONE, 0);
            m.get(&url).is_ok_and(|b| {
                let page = crate::drive::parse_browse(&b, "Lezioni", "");
                ["notes.txt", "data.bin", "board.png", "a.zip"]
                    .iter()
                    .all(|n| page.files.iter().any(|f| f.name() == *n))
                    && page.folders.iter().any(|f| f.name == "Sub")
            })
        };
        while !listed(&mut m) {
            if Instant::now() > indexed {
                return None;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Some(e)
    }

    pub fn connect(&self) -> Connect {
        local_connect(&self.base)
    }

    /// Text and archive listings are read in the background: this returns when `ready` says the server has caught up.
    pub fn wait_for(&self, what: &str, mut ready: impl FnMut() -> bool) {
        let until = Instant::now() + Duration::from_secs(60);
        while !ready() {
            assert!(Instant::now() < until, "the engine never got to: {what}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A zip with one stored file, by hand: the listing is what is being tested.
fn zip_with_one_file() -> Vec<u8> {
    let name = b"inside.txt";
    let body = b"inside the zip\n";
    let crc = crate::cache::crc32(body);
    let mut out = Vec::new();
    let le = |v: &mut Vec<u8>, b: &[u8]| v.extend_from_slice(b);
    le(&mut out, b"PK\x03\x04\x14\0\0\0\0\0\0\0!\0");
    le(&mut out, &crc.to_le_bytes());
    le(&mut out, &(body.len() as u32).to_le_bytes());
    le(&mut out, &(body.len() as u32).to_le_bytes());
    le(&mut out, &(name.len() as u16).to_le_bytes());
    le(&mut out, &0u16.to_le_bytes());
    le(&mut out, name);
    le(&mut out, body);
    let central = out.len() as u32;
    le(&mut out, b"PK\x01\x02\x14\0\x14\0\0\0\0\0\0\0!\0");
    le(&mut out, &crc.to_le_bytes());
    le(&mut out, &(body.len() as u32).to_le_bytes());
    le(&mut out, &(body.len() as u32).to_le_bytes());
    le(&mut out, &(name.len() as u16).to_le_bytes());
    le(&mut out, &[0u8; 12]);
    le(&mut out, &0u32.to_le_bytes());
    le(&mut out, name);
    let size = out.len() as u32 - central;
    le(&mut out, b"PK\x05\x06\0\0\0\0\x01\0\x01\0");
    le(&mut out, &size.to_le_bytes());
    le(&mut out, &central.to_le_bytes());
    le(&mut out, &0u16.to_le_bytes());
    out
}
