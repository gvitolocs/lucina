//! Streaming voice for Juno's Lucina chat: the owner's voice goes up Secretary's voice WebSocket (`/v1/voice/ws`) while he talks,
//! as LucinaVoice protocol sections 2 and 10 describe.
//!
//! * [`rules`]: the integer rules, proved by Verus on this very file: the decimator's windows, the playback expansion, the
//!   client's turn phases, the table of notes waiting for their answer.
//! * [`dsp`]: 48 kHz mono to 16 kHz frames for the socket; the server's 24 kHz speech to 48 kHz stereo.
//! * [`msg`]: the `start` and the server's messages.
//! * [`session`]: the turn, as a pure state machine.
//! * `link` and `voice` (feature `net`): the blocking WebSocket client and the controller that runs a microphone, the socket and a
//!   player together.
pub mod dsp;
pub mod msg;
pub mod rules;
pub mod session;

#[cfg(feature = "net")]
pub mod link;
#[cfg(feature = "net")]
pub mod voice;
