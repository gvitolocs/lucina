//! USB Video Class cameras for the LucinaOS kernel: the Galaxy Book Go's webcam (Chicony "720p HD Camera", USB `04f2:b72f`,
//! on port 4 of the internal hub, powered by TLMM 13) and any other UVC camera on the bus.
//!
//! * [`desc`] reads the UVC descriptors of a raw configuration descriptor: formats (YUY2 and other uncompressed ones, MJPEG),
//!   their frames and intervals, colour matching, and the streaming interface's alternate settings.
//! * [`probe`] is the probe and commit sequence and the choice of alternate setting.
//! * [`stream`] turns isochronous packets into frames.
//! * [`rules`] holds the buffer rules, checked by Verus: payload bounds, copies into a frame buffer, packet spans in a
//!   transfer, packets per transfer, and the three frame buffers.
//! * [`wire`] is what `/dev/lucina-camera0` speaks, shared with user space.
//!
//! Source of truth for behaviour: Linux `drivers/media/usb/uvc` (v6.16, `uvc_driver.c`, `uvc_video.c`, `uvc_v4l2.c`), read
//! for behaviour; no code is taken from it. Windows runs this camera with its inbox `usbvideo.sys` (no vendor driver in the
//! laptop's driver store), so Linux's generic path is the original for every step here.
//!
//! `no_std` with `alloc`, no `unsafe`.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod desc;
pub mod probe;
pub mod rules;
pub mod stream;
pub mod wire;

#[cfg(test)]
mod tests;
