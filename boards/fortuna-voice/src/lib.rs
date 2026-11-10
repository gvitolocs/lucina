//! Voice notes on the DSi: the microphone's 16-bit samples come from the ARM7 through `fortuna-ring` (the
//! coordinator's proved ring),
//! the ARM9 encodes them to IMA-ADPCM in Microsoft's WAV layout (`adpcm`, `wav`), which the server's ffmpeg reads as
//! `adpcm_ima_wav`, and the screen shows a level meter and a timer (`meter`). `wave` keeps the note's waveform strip,
//! which goes up with it. The integer rules Verus checks are in
//! `rules`. no_std, no allocation, no floating point.
#![cfg_attr(not(test), no_std)]

pub mod adpcm;
pub mod meter;
pub mod rules;
pub mod wav;
pub mod wave;

/// The rate the DSi records at: the I2S clock (32728 Hz with sound at 32 kHz) divided by 3 (MICCNT rate 2).
pub const RATE: u32 = 10_909;
