//! The 60-byte header of a mono IMA-ADPCM WAV: RIFF, `fmt ` (20 bytes: tag 0x0011, block 256, 4 bits, 505 samples a
//! block), `fact` (the real sample count) and `data`.

use crate::adpcm::{BLOCK_ALIGN, SAMPLES_PER_BLOCK};
use crate::rules::{self, wav_sizes};

pub const HEADER_LEN: usize = 60;

/// The header for `blocks` blocks holding `samples` real samples. `None` past 32-bit sizes.
pub fn header(sample_rate: u32, blocks: u32, samples: u32) -> Option<[u8; HEADER_LEN]> {
    let (data, riff) = wav_sizes(blocks)?;
    let mut h = [0u8; HEADER_LEN];
    let mut at = 0;
    let mut put = |b: &[u8]| {
        h[at..at + b.len()].copy_from_slice(b);
        at += b.len();
    };
    put(b"RIFF");
    put(&riff.to_le_bytes());
    put(b"WAVEfmt ");
    put(&20u32.to_le_bytes());
    put(&0x0011u16.to_le_bytes());
    put(&1u16.to_le_bytes());
    put(&sample_rate.to_le_bytes());
    put(&((sample_rate as u64 * BLOCK_ALIGN as u64 / SAMPLES_PER_BLOCK as u64) as u32).to_le_bytes());
    put(&(BLOCK_ALIGN as u16).to_le_bytes());
    put(&4u16.to_le_bytes());
    put(&2u16.to_le_bytes());
    put(&(SAMPLES_PER_BLOCK as u16).to_le_bytes());
    put(b"fact");
    put(&4u32.to_le_bytes());
    put(&samples.to_le_bytes());
    put(b"data");
    put(&data.to_le_bytes());
    Some(h)
}

/// Blocks needed for `samples`.
pub fn blocks_for(samples: u32) -> u32 {
    rules::blocks_for(samples)
}

/// Bytes of a whole file of `secs` seconds.
pub fn bytes_for_seconds(sample_rate: u32, secs: u32) -> u32 {
    blocks_for(sample_rate.saturating_mul(secs)) * BLOCK_ALIGN as u32 + HEADER_LEN as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adpcm::{Decoder, Encoder};

    #[test]
    fn a_minute_fits_the_budget() {
        // 60 s at 10.9 kHz: about 330 KB, the voice note's cap.
        let b = bytes_for_seconds(crate::RATE, 60);
        assert!(b > 320_000 && b < 335_000, "{b}");
        assert!(header(crate::RATE, u32::MAX, 0).is_none());
    }

    #[test]
    fn ffmpeg_reads_it_as_we_do() {
        let rate = crate::RATE;
        let n = rate as usize;
        let pcm: Vec<i16> = (0..n).map(|i| ((i as f64 * 0.11).sin() * 10000.0) as i16).collect();
        let mut e = Encoder::new();
        let blocks = blocks_for(n as u32);
        let mut file = header(rate, blocks, n as u32).unwrap().to_vec();
        let mut ours = Vec::new();
        for c in pcm.chunks(SAMPLES_PER_BLOCK) {
            let mut b = [0u8; 256];
            e.encode_block(c, &mut b);
            file.extend_from_slice(&b);
            let mut d = [0i16; SAMPLES_PER_BLOCK];
            Decoder::decode_block(&b, &mut d);
            ours.extend_from_slice(&d);
        }
        let dir = std::env::temp_dir().join(format!("fortuna-voice-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("x.wav");
        std::fs::write(&path, &file).unwrap();
        let out = std::process::Command::new("ffmpeg").args(["-v", "error", "-i"]).arg(&path).args(["-f", "s16le", "-"]).output();
        let _ = std::fs::remove_dir_all(&dir);
        let Ok(out) = out else { return }; // no ffmpeg on this host
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let theirs: Vec<i16> = out.stdout.chunks(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
        // ffmpeg decodes whole blocks (it does not cut at the fact chunk's count): the padding is the held last sample.
        assert_eq!(theirs.len(), blocks as usize * SAMPLES_PER_BLOCK);
        let first_diff = theirs.iter().zip(&ours).position(|(a, b)| a != b);
        assert_eq!(first_diff, None, "theirs {:?} ours {:?}", first_diff.map(|i| &theirs[i.saturating_sub(2)..i + 3]), first_diff.map(|i| &ours[i.saturating_sub(2)..i + 3]));
    }
}
