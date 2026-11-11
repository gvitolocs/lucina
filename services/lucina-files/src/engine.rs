//! The real `lucina-media` for tests: the server the apps talk to through p1, on temporary folders and a port of its own, with no key and
//! no contact with the live library. It takes `LUCINA_MEDIA_BIN`, or the program deployed on buildhost. Elsewhere [`Engine::start`] answers
//! `None`, and the tests that need it say so and pass.

use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::fake::painted;
use crate::media::{Connect, local_connect};

pub struct Engine {
    child: Child,
    dir: PathBuf,
    pub base: String,
}

fn program() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("LUCINA_MEDIA_BIN") {
        return Some(PathBuf::from(p));
    }
    let home = std::env::var_os("HOME")?;
    Some(Path::new(&home).join(".local/bin/lucina-media")).filter(|p| p.is_file())
}

fn free_port() -> Option<u16> {
    Some(TcpListener::bind("127.0.0.1:0").ok()?.local_addr().ok()?.port())
}

fn write(p: &Path, b: &[u8]) {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).expect("make a folder");
    }
    std::fs::write(p, b).expect("write a seed file");
}

impl Engine {
    /// Six photos in an album called Holiday, a folder called Lezioni in the Drive. `None` where there is no such program.
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
        write(&drive.join("notes.txt"), b"hello from the engine\nsecond line\n");
        write(&drive.join("Sub/main.rs"), b"fn main() {\n    println!(\"hi\");\n}\n");
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
                .env("LUCINA_MEDIA_INTERNAL_LISTEN", format!("127.0.0.1:{internal}"))
                .env("LUCINA_MEDIA_FOLDERS", format!("Lezioni={}", drive.display()))
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
        import.arg("import").arg(dir.join("photos")).args(["--source", "phone-test", "--album", "Holiday"]);
        env(&mut import);
        import.status().ok()?.success().then_some(())?;
        // `timeout`: a test killed outright leaves nothing running for long.
        let mut serve = Command::new("timeout");
        serve.arg("300").arg(&exe).arg("serve");
        env(&mut serve);
        let child = serve.spawn().ok()?;
        let e = Engine { child, dir, base: format!("http://127.0.0.1:{port}") };
        let until = Instant::now() + Duration::from_secs(30);
        while TcpStream::connect(("127.0.0.1", port)).is_err() {
            if Instant::now() > until {
                return None;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        // The folder is indexed in the background.
        let mut m = (e.connect())()?;
        while m.get("/v1/media/drive").ok().and_then(|v| v["folders"][0]["files"].as_u64()) != Some(5) {
            if Instant::now() > until {
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
