//! lucina-app-recorder: a voice recorder on Juno. Samples go from `/dev/lucina-audio-in` to a WAV file in `/data/recordings`, and from there in
//! resumable chunks to lucina-memo, whose transcription runs as for a memo from the phone. What is proven, and how: `docs/research/juno-audio.md`.

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

mod app;
mod capture;
mod upload;
mod wav;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use app::{Job, Msg, Recorder};
use lucina_account::system;
use lucina_app::Sender;
use lucina_audio_decode::device::{Device, Microphone};
use lucina_audio_decode::engine::{Cmd, Engine};
use upload::Memos;

fn recordings_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("LUCINA_RECORDINGS") {
        return PathBuf::from(dir);
    }
    PathBuf::from("/data/recordings")
}

/// "VA_DEC Volume" for speech, in dB. Windows leaves the codec at 0 dB for this mic and raises speech after it (not decoded);
/// here the decimator does it. A MEMS DMIC of about -26 dBFS at 94 dB SPL puts speech at 0.5 m (60 to 65 dB SPL) near -60 dBFS
/// at 0 dB, so +30 dB brings it to about -30 dBFS RMS with peaks near -15, with room before clipping (juno-audio.md section 31).
const SPEECH_GAIN_DB: i32 = 30;
/// The volume a recording plays back at, in dB of the device's range: a little above the Player's start (-24).
const PLAY_VOLUME_DB: i32 = -18;

/// Best effort: a kernel without the control records at its own gain.
fn set_mic_gain(db: i32) {
    use std::io::Write;
    if let Ok(mut ctl) = std::fs::OpenOptions::new().write(true).open("/dev/lucina-audio-ctl") {
        let _ = ctl.write_all(format!("mic-gain {db}\n").as_bytes());
    }
}

fn start_player(tx: Sender<Msg>) -> app::Play {
    let (commands, rx) = mpsc::channel::<Cmd>();
    std::thread::spawn(move || {
        Engine::new(Device::new(), PLAY_VOLUME_DB, move |report| tx.send(Msg::Played(report))).run(rx);
    });
    Box::new(move |cmd| {
        let _ = commands.send(cmd);
    })
}

fn timezone() -> String {
    std::fs::read_to_string("/etc/lucina/timezone")
        .map(|s| s.trim().to_owned())
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| String::from("Europe/Copenhagen"))
}

fn send_all(memos: &mut dyn Memos, dir: &Path, timezone: &str, tx: &Sender<Msg>) {
    for row in app::list(dir, timezone).into_iter().rev().filter(|r| !r.done) {
        let Some(mut meta) = upload::load_meta(&row.path) else {
            continue;
        };
        let path = row.path.clone();
        let result = upload::send(memos, &path, &mut meta, &mut |uploaded| {
            tx.send(Msg::Progress { path: path.clone(), uploaded });
        });
        let offline = matches!(result, Err(upload::UploadError::Offline(_)));
        tx.send(Msg::Uploaded {
            path,
            result: result.map_err(|e| match e {
                upload::UploadError::Offline(m) | upload::UploadError::Refused(m) => m,
            }),
        });
        if offline {
            return;
        }
    }
}

fn start_worker(tx: Sender<Msg>, mut memos: Option<Box<dyn Memos>>, dir: PathBuf, timezone: String) -> app::Link {
    let (jobs, rx) = mpsc::channel::<Job>();
    std::thread::spawn(move || {
        while let Ok(job) = rx.recv() {
            match job {
                Job::List => tx.send(Msg::Listed(app::list(&dir, &timezone))),
                Job::Send => {
                    if let Some(memos) = memos.as_mut() {
                        send_all(memos.as_mut(), &dir, &timezone, &tx);
                    }
                }
            }
        }
    });
    Box::new(move |job| {
        let _ = jobs.send(job);
    })
}

fn main() {
    lucina_app::run("lucina-app-recorder", |tx| {
        let dir = recordings_dir();
        let _ = std::fs::create_dir_all(&dir);
        let client = system::client();
        let signed_in = client.is_some();
        let memos: Option<Box<dyn Memos>> = client.map(|client| Box::new(upload::Account { client }) as Box<dyn Memos>);
        let zone = timezone();
        let link = start_worker(tx.clone(), memos, dir.clone(), zone.clone());
        let play = start_player(tx.clone());
        let start: app::Start = Box::new(move |path, live| {
            let tx = tx.clone();
            std::thread::spawn(move || {
                set_mic_gain(SPEECH_GAIN_DB);
                let mut mic = Microphone::default();
                tx.send(Msg::Finished(capture::record(&mut mic, &path, live)));
            });
        });
        Recorder::new(dir, signed_in, zone, start, link, play)
    });
}
