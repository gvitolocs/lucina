//! The thread that records: microphone to file. It syncs every few seconds, so that a power cut costs seconds, not the recording.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use lucina_audio_decode::device::Mic;

use crate::wav::WavWriter;

const SYNC_EVERY: Duration = Duration::from_secs(5);

/// What the screen reads while it records.
#[derive(Default)]
pub struct Live {
    pub stop: AtomicBool,
    pub bytes: AtomicU64,
    /// The loudest sample lately, for the meter.
    pub peak: AtomicU32,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Finished {
    pub path: PathBuf,
    pub data_bytes: u64,
    pub error: Option<String>,
}

/// Runs until `live.stop` or an error. A failure keeps what reached the file.
pub fn record(mic: &mut dyn Mic, path: &Path, live: Arc<Live>) -> Finished {
    let fail = |error: String, data_bytes| Finished { path: path.to_path_buf(), data_bytes, error: Some(error) };
    if let Err(err) = mic.open() {
        return fail(format!("the microphone: {err}"), 0);
    }
    let mut writer = match WavWriter::create(path) {
        Ok(writer) => writer,
        Err(err) => {
            mic.close();
            return fail(format!("cannot write the recording: {err}"), 0);
        }
    };
    let mut buf = vec![0u8; 4096];
    let mut last_sync = Instant::now();
    let mut error = None;
    while !live.stop.load(Ordering::Relaxed) {
        let n = match mic.read(&mut buf) {
            Ok(0) => {
                error = Some(String::from("the microphone went away"));
                break;
            }
            Ok(n) => n & !1,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => {
                error = Some(format!("the microphone: {err}"));
                break;
            }
        };
        if let Err(err) = writer.write(&buf[..n]) {
            error = Some(format!("cannot write the recording: {err}"));
            break;
        }
        let peak = buf[..n]
            .chunks_exact(2)
            .map(|s| i32::from(i16::from_le_bytes([s[0], s[1]])).unsigned_abs())
            .max()
            .unwrap_or(0);
        live.peak.store(peak, Ordering::Relaxed);
        live.bytes.store(writer.data_bytes(), Ordering::Relaxed);
        if last_sync.elapsed() >= SYNC_EVERY {
            last_sync = Instant::now();
            let _ = writer.sync();
        }
    }
    mic.close();
    let bytes = writer.data_bytes();
    match writer.finish() {
        Ok(data_bytes) => Finished { path: path.to_path_buf(), data_bytes, error },
        Err(err) => fail(format!("cannot finish the recording: {err}"), bytes),
    }
}
