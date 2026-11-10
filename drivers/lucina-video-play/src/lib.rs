//! The Video app's player as a library: the decoders behind one face (CPU and the Venus video block), the decoding thread,
//! frame pacing and the sound that the clock follows. Shared by `lucina-app-video` and LucinaNet's media backend.
pub mod decode;
pub mod pace;
pub mod sound;
pub mod venus;
pub mod worker;
