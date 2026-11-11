//! A WAV file that can be cut off at any moment: the sizes in its header are filled in when it is closed, and [`repair`] fills them in
//! after a crash or a power cut, from the file's length. Whatever had reached the disk is kept.

use std::fs::{File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::path::Path;

pub const RATE: u32 = 48_000;
pub const BYTES_PER_FRAME: u32 = 2;
const HEADER: u64 = 44;

fn header(data_bytes: u32) -> [u8; HEADER as usize] {
    let mut h = [0u8; HEADER as usize];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&(36 + data_bytes).to_le_bytes());
    h[8..16].copy_from_slice(b"WAVEfmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes());
    h[22..24].copy_from_slice(&1u16.to_le_bytes());
    h[24..28].copy_from_slice(&RATE.to_le_bytes());
    h[28..32].copy_from_slice(&(RATE * BYTES_PER_FRAME).to_le_bytes());
    h[32..34].copy_from_slice(&(BYTES_PER_FRAME as u16).to_le_bytes());
    h[34..36].copy_from_slice(&16u16.to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&data_bytes.to_le_bytes());
    h
}

pub struct WavWriter {
    file: File,
    data_bytes: u64,
}

impl WavWriter {
    pub fn create(path: &Path) -> io::Result<WavWriter> {
        let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
        file.write_all(&header(0))?;
        Ok(WavWriter { file, data_bytes: 0 })
    }

    pub fn write(&mut self, samples: &[u8]) -> io::Result<()> {
        self.file.write_all(samples)?;
        self.data_bytes += samples.len() as u64;
        Ok(())
    }

    /// To the disk, so that a power cut loses at most what came since.
    pub fn sync(&mut self) -> io::Result<()> {
        self.file.sync_data()
    }

    pub fn data_bytes(&self) -> u64 {
        self.data_bytes
    }

    pub fn finish(mut self) -> io::Result<u64> {
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(&header(self.data_bytes.min(u64::from(u32::MAX - 36)) as u32))?;
        self.file.sync_all()?;
        Ok(self.data_bytes)
    }
}

/// How long `data_bytes` of samples last, in seconds.
pub fn seconds(data_bytes: u64) -> f64 {
    data_bytes as f64 / f64::from(RATE * BYTES_PER_FRAME)
}

/// Fills in the header of a file a crash left without sizes. Says how many bytes of samples it has.
pub fn repair(path: &Path) -> io::Result<u64> {
    let len = std::fs::metadata(path)?.len();
    let data = len.saturating_sub(HEADER) & !u64::from(BYTES_PER_FRAME - 1);
    let mut file = OpenOptions::new().write(true).open(path)?;
    file.write_all(&header(data.min(u64::from(u32::MAX - 36)) as u32))?;
    file.set_len(HEADER + data)?;
    file.sync_all()?;
    Ok(data)
}
