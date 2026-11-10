//! Video on Juno, below the screen: containers read into packets ([`demux`]), and the clock frames are shown against ([`clock`]).
//!
//! `secretary-player` decides what to play and what the server should send; this crate is what a platform player does underneath it
//! on the other Secretary apps. It reads a local file or anything else that can `Read + Seek`, and [`mse`] reads what a page appends to a
//! Media Source Extensions `SourceBuffer`.

pub mod clock;
pub mod convert;
pub mod demux;
mod ivf;
pub mod limits;
mod mkv;
mod mp4;
pub mod mse;
pub mod mse_rules;
pub mod net;

pub use clock::{AudioOutput, Clock, Silent};
pub use convert::{Matrix, Planes, Quality, Scaler};
pub use demux::{Codec, Demuxer, Error, Packet, Track, annexb_from_avcc, open};
