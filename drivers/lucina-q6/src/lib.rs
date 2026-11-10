// Derived from Linux (v7.1) sound/soc/qcom/qdsp6/{q6afe,q6adm,q6asm,q6core,q6routing}.c, sound/soc/qcom/sc7180.c,
// drivers/soc/qcom/{apr,pdr_interface,qcom_pdr_msg}.c, all GPL-2.0: a re-implementation of their behaviour in Rust, under
// the same licence. Nothing here touches hardware.

//! What the kernel says to the audio DSP of the Galaxy Book Go (SC7180) once GLINK is up, as functions from bytes to bytes.
//!
//! * [`apr`]: the packet header.
//! * [`avcs`]: is the DSP's audio framework ready.
//! * [`pdr`]: the service registry listener, which says when the `avs/audio` protection domain is up.
//! * [`stream`]: the commands of one playback, in the order Linux sends them, as a state machine.
//! * [`capture`]: the same for the internal microphone.
//! * [`level`]: the ceiling every sample passes through and the only test tone there is.
//! * [`rules`]: the arithmetic and orders behind them, which Verus checks.
//!
//! Linux reaches these services through ALSA's dynamic PCM: the order in [`stream`] is what `sc7180_qdsp_snd_startup`,
//! `q6afe_dai_prepare`, `q6asm_dai_prepare` and `q6routing_stream_open` add up to for a stream routed to tertiary MI2S.
//! Packets are compared byte for byte with what Linux's own functions send (`oracle/`, `tests/fixtures/`). The DSP in the tests is a model written from reading the C; see `docs/research/juno-audio.md` for
//! what has met the real one.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod apr;
pub mod avcs;
pub mod capture;
pub mod level;
pub mod pdr;
pub mod rules;
pub mod ssr;
pub mod stream;
pub mod va;
