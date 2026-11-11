//! lucina-app-player: an audio player on Juno. Files under `/data/music` and `/data/recordings`, and the media library's audio, with
//! the queue and resume positions of Secretary's `secretary-player`. `LUCINA_APP_SAMPLE=1` makes up tracks and uses no account.
//! What is proven, and how: `docs/research/juno-audio.md`. `lucina-app-player <file>` plays that file, with its folder as the list
//! (the shell passes it, for Lucina's `play_file`).

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

mod app;
mod library;
#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::sync::mpsc;

use app::{Job, Msg, Player};
use lucina_audio_decode::engine::{Cmd, Engine};
use library::{Account, Server};
use lucina_account::system;
use lucina_app::Sender;
use lucina_audio_decode::device::Device;

fn data_dir() -> PathBuf {
    // Resume positions should outlast a restart, where `/data` is there. Otherwise, with the other apps, in memory.
    let on_disk = PathBuf::from("/data/lucina/player");
    if std::env::var_os("LUCINA_APP_DATA").is_none() && std::fs::create_dir_all(&on_disk).is_ok() {
        return on_disk;
    }
    lucina_app_support::files::data_dir("player")
}

fn music_dirs() -> Vec<PathBuf> {
    match std::env::var_os("LUCINA_PLAYER_DIRS") {
        Some(dirs) => std::env::split_paths(&dirs).collect(),
        None => vec![PathBuf::from("/data/music"), PathBuf::from("/data/recordings")],
    }
}

fn start_engine(tx: Sender<Msg>, volume: i32) -> app::Engine {
    let (commands, rx) = mpsc::channel::<Cmd>();
    std::thread::spawn(move || {
        let tx_report = tx.clone();
        Engine::new(Device::new(), volume, move |report| tx_report.send(Msg::Engine(report))).run(rx);
    });
    Box::new(move |cmd| {
        let _ = commands.send(cmd);
    })
}

/// One thread for the disk and the network, one job at a time.
fn start_worker(tx: Sender<Msg>, mut server: Option<Box<dyn Server>>, cache: PathBuf, dirs: Vec<PathBuf>) -> app::Link {
    let (jobs, rx) = mpsc::channel::<Job>();
    std::thread::spawn(move || {
        while let Ok(job) = rx.recv() {
            match job {
                Job::ListLocal => tx.send(Msg::Local(library::scan(&dirs))),
                Job::ListRemote => {
                    if let Some(server) = server.as_mut() {
                        tx.send(Msg::Remote(server.audio_items()));
                        if let Ok(entries) = server.progress() {
                            tx.send(Msg::Progress(entries));
                        }
                    }
                }
                Job::Fetch(id) => {
                    let result = match server.as_mut() {
                        Some(server) => fetch(server.as_mut(), &cache, &id),
                        None => Err(String::from("Not signed in")),
                    };
                    tx.send(Msg::Fetched { id, result });
                }
                Job::Push(entries) => {
                    if let Some(server) = server.as_mut()
                        && let Ok(acks) = server.push(&entries)
                    {
                        tx.send(Msg::Pushed(acks));
                    }
                }
            }
        }
    });
    Box::new(move |job| {
        let _ = jobs.send(job);
    })
}

/// A library item's file, kept while there is room: asking twice costs nothing.
pub fn fetch(server: &mut dyn Server, cache: &std::path::Path, id: &str) -> Result<PathBuf, String> {
    let safe: String = id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    let path = cache.join(safe);
    if path.is_file() {
        return Ok(path);
    }
    let bytes = server.file(id)?;
    lucina_app_support::files::write_atomic(&path, &bytes).map_err(|e| format!("cannot keep the file: {e}"))?;
    Ok(path)
}

/// `LUCINA_APP_SAMPLE=1`: a few seconds of tones as WAV files, so that QEMU can play something with no disk and no account.
fn make_sample(dir: &std::path::Path) -> Vec<PathBuf> {
    let _ = std::fs::create_dir_all(dir);
    for (name, hz) in [("A low tone", 220u32), ("A high tone", 880)] {
        let frames = 48_000 * 15;
        let mut data = Vec::with_capacity(frames * 4);
        for i in 0..frames {
            let s = ((2.0 * std::f64::consts::PI * f64::from(hz) * i as f64 / 48_000.0).sin() * 12_000.0) as i16;
            data.extend_from_slice(&s.to_le_bytes());
            data.extend_from_slice(&s.to_le_bytes());
        }
        let mut wav = Vec::with_capacity(data.len() + 44);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&48_000u32.to_le_bytes());
        wav.extend_from_slice(&192_000u32.to_le_bytes());
        wav.extend_from_slice(&4u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(data.len() as u32).to_le_bytes());
        wav.extend_from_slice(&data);
        let _ = std::fs::write(dir.join(format!("{name}.wav")), wav);
    }
    vec![dir.to_path_buf()]
}

fn main() {
    lucina_app::run("lucina-app-player", |tx| {
        let data = data_dir();
        let sample = std::env::var_os("LUCINA_APP_SAMPLE").is_some();
        let file = std::env::args().nth(1).map(PathBuf::from).filter(|_| !sample);
        let file = file.filter(|f| {
            let ok = f.is_file();
            if !ok {
                eprintln!("lucina-app-player: not a file: {}", f.display());
            }
            ok
        });
        let dirs = match (&file, file.as_ref().and_then(|f| f.parent())) {
            (Some(_), Some(parent)) => vec![parent.to_path_buf()],
            _ if sample => make_sample(&data.join("sample")),
            _ => music_dirs(),
        };
        let client = if sample { None } else { system::client() };
        let signed_in = client.is_some();
        let server: Option<Box<dyn Server>> = client.map(|client| Box::new(Account { client }) as Box<dyn Server>);
        let cache = data.join("cache");
        let _ = std::fs::create_dir_all(&cache);
        let engine = start_engine(tx.clone(), app::DEFAULT_VOLUME);
        let link = start_worker(tx, server, cache, dirs);
        let mut player = Player::new(data, signed_in, engine, link);
        player.sample = sample;
        player.open_on_start = file.map(|f| format!("{}{}", library::LOCAL_PREFIX, f.display()));
        player
    });
}
