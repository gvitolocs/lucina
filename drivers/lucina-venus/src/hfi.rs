// SPDX-License-Identifier: GPL-2.0-only
//! The host-firmware interface (HFI) packets of Venus 4xx: what Linux's hfi_cmds.c builds and hfi_msgs.c reads, as words. Every
//! packet starts with its size in bytes and its type (`struct hfi_pkt_hdr`); a session's also carry the session id
//! (`struct hfi_session_hdr_pkt`). `tests/oracle.rs` compares what this builds with what Linux's own hfi_cmds.c builds.

use alloc::vec::Vec;

/// Command types, `hfi_cmds.h`.
pub mod cmd {
    pub const SYS_INIT: u32 = 0x1_0001;
    pub const SYS_PC_PREP: u32 = 0x1_0002;
    pub const SYS_SET_PROPERTY: u32 = 0x1_0005;
    pub const SYS_GET_PROPERTY: u32 = 0x1_0006;
    pub const SYS_SESSION_INIT: u32 = 0x1_0007;
    pub const SYS_SESSION_END: u32 = 0x1_0008;
    pub const SESSION_SET_PROPERTY: u32 = 0x1_1001;
    pub const SESSION_SET_BUFFERS: u32 = 0x1_1002;
    pub const SYS_SESSION_ABORT: u32 = 0x21_0001;
    pub const SYS_PING: u32 = 0x21_0002;
    pub const SESSION_LOAD_RESOURCES: u32 = 0x21_1001;
    pub const SESSION_START: u32 = 0x21_1002;
    pub const SESSION_STOP: u32 = 0x21_1003;
    pub const SESSION_EMPTY_BUFFER: u32 = 0x21_1004;
    pub const SESSION_FILL_BUFFER: u32 = 0x21_1005;
    pub const SESSION_FLUSH: u32 = 0x21_1008;
    pub const SESSION_GET_PROPERTY: u32 = 0x21_1009;
    pub const SESSION_RELEASE_BUFFERS: u32 = 0x21_100b;
    pub const SESSION_RELEASE_RESOURCES: u32 = 0x21_100c;
    pub const SESSION_CONTINUE: u32 = 0x21_100d;
}

/// Message types, `hfi_msgs.h`.
pub mod msg {
    pub const SYS_INIT: u32 = 0x2_0001;
    pub const SYS_PC_PREP: u32 = 0x2_0002;
    pub const SYS_RELEASE_RESOURCE: u32 = 0x2_0003;
    pub const SYS_DEBUG: u32 = 0x2_0004;
    pub const SYS_SESSION_INIT: u32 = 0x2_0006;
    pub const SYS_SESSION_END: u32 = 0x2_0007;
    pub const SYS_IDLE: u32 = 0x2_0008;
    pub const SYS_COV: u32 = 0x2_0009;
    pub const SYS_PROPERTY_INFO: u32 = 0x2_000a;
    pub const EVENT_NOTIFY: u32 = 0x2_1001;
    pub const SYS_PING_ACK: u32 = 0x22_0002;
    pub const SYS_SESSION_ABORT: u32 = 0x22_0004;
    pub const SESSION_LOAD_RESOURCES: u32 = 0x22_1001;
    pub const SESSION_START: u32 = 0x22_1002;
    pub const SESSION_STOP: u32 = 0x22_1003;
    pub const SESSION_FLUSH: u32 = 0x22_1006;
    pub const SESSION_EMPTY_BUFFER: u32 = 0x22_1007;
    pub const SESSION_FILL_BUFFER: u32 = 0x22_1008;
    pub const SESSION_PROPERTY_INFO: u32 = 0x22_1009;
    pub const SESSION_RELEASE_RESOURCES: u32 = 0x22_100a;
    pub const SESSION_RELEASE_BUFFERS: u32 = 0x22_100c;
}

/// System properties, `hfi_helper.h`.
pub mod sys_property {
    pub const DEBUG_CONFIG: u32 = 0x1;
    pub const CODEC_POWER_PLANE_CTRL: u32 = 0x5;
    pub const IMAGE_VERSION: u32 = 0x6;
}

/// `HFI_VIDEO_ARCH_OX`.
pub const VIDEO_ARCH_OX: u32 = 1;
/// `HFI_DEBUG_MSG_ERROR | HFI_DEBUG_MSG_FATAL`: Linux's `venus_fw_debug`.
pub const DEBUG_ERROR_FATAL: u32 = 0x08 | 0x10;
/// `HFI_DEBUG_MODE_QUEUE`: debug text into the debug queue.
pub const DEBUG_MODE_QUEUE: u32 = 1;
/// `HFI_EVENT_SYS_ERROR`.
pub const EVENT_SYS_ERROR: u32 = 1;
/// The cookie `pkt_sys_ping` sends and `hfi_sys_ping_done` wants back.
pub const PING_COOKIE: u32 = 0xbeef;

fn packet(kind: u32, body: &[u32]) -> Vec<u32> {
    let mut p = Vec::with_capacity(2 + body.len());
    p.push(((2 + body.len()) * 4) as u32);
    p.push(kind);
    p.extend_from_slice(body);
    p
}

/// `pkt_sys_init`.
pub fn sys_init() -> Vec<u32> {
    packet(cmd::SYS_INIT, &[VIDEO_ARCH_OX])
}

/// `pkt_sys_image_version`: asks for the firmware's version string.
pub fn sys_image_version() -> Vec<u32> {
    packet(cmd::SYS_GET_PROPERTY, &[1, sys_property::IMAGE_VERSION])
}

/// `pkt_sys_debug_config`.
pub fn sys_debug_config(mode: u32, config: u32) -> Vec<u32> {
    packet(cmd::SYS_SET_PROPERTY, &[1, sys_property::DEBUG_CONFIG, config, mode])
}

/// `pkt_sys_power_control`.
pub fn sys_power_control(enable: bool) -> Vec<u32> {
    packet(cmd::SYS_SET_PROPERTY, &[1, sys_property::CODEC_POWER_PLANE_CTRL, enable as u32])
}

/// `pkt_sys_pc_prep`.
pub fn sys_pc_prep() -> Vec<u32> {
    packet(cmd::SYS_PC_PREP, &[])
}

/// `pkt_sys_ping`.
pub fn sys_ping(cookie: u32) -> Vec<u32> {
    packet(cmd::SYS_PING, &[cookie])
}

/// What `venus_core_init` sends, in its order: `SYS_INIT`, the image version, then `venus_sys_set_default_properties` for 4xx (debug
/// text for errors and fatal errors into the debug queue, power collapse of the codec allowed). No idle indicator (1xx only) and no UBWC
/// configuration (sc7180 has none).
pub fn core_init() -> [Vec<u32>; 4] {
    [sys_init(), sys_image_version(), sys_debug_config(DEBUG_MODE_QUEUE, DEBUG_ERROR_FATAL), sys_power_control(true)]
}

/// A message as it came out of the queue, its size already checked by [`crate::rules::read_span`].
#[derive(Clone, Copy, Debug)]
pub struct Message<'a> {
    pub words: &'a [u32],
}

impl<'a> Message<'a> {
    pub fn kind(&self) -> u32 {
        self.words.get(1).copied().unwrap_or(0)
    }

    pub fn word(&self, index: usize) -> Option<u32> {
        self.words.get(index).copied()
    }

    /// The bytes after the first `words` words, for text.
    pub fn bytes_after(&self, words: usize, out: &mut [u8]) -> usize {
        let mut len = 0;
        for w in self.words.iter().skip(words) {
            for b in w.to_le_bytes() {
                if len == out.len() {
                    return len;
                }
                out[len] = b;
                len += 1;
            }
        }
        len
    }
}

/// `hfi_sys_init_done` for 4xx: the firmware's own list of properties is not read there (the platform's tables stand in for it), so
/// only the error and that some properties came. `Err` holds the HFI error: the firmware's, or Linux's `HFI_ERR_SYS_INVALID_PARAMETER`
/// (2) for none, `HFI_ERR_SYS_INSUFFICIENT_RESOURCES` (4) for no room for them.
pub fn sys_init_done(m: &Message) -> Result<u32, u32> {
    let error = m.word(2).unwrap_or(2);
    if error != 0 {
        return Err(error);
    }
    let properties = m.word(3).unwrap_or(0);
    if properties == 0 {
        return Err(2);
    }
    if m.words.len() * 4 <= 16 {
        return Err(4);
    }
    Ok(properties)
}

/// `hfi_sys_property_info` and `sys_get_prop_image_version`: the version text of a `SYS_PROPERTY_INFO` for the image version, at most
/// 128 bytes (`VER_STR_SZ`), up to its NUL. Linux also copies it into SMEM's image version table for other processors to read; that
/// table is not written here.
pub fn image_version<'b>(m: &Message, out: &'b mut [u8; 128]) -> Option<&'b [u8]> {
    let properties = m.word(2)?;
    if properties == 0 || properties > 1 || m.word(3)? != sys_property::IMAGE_VERSION || (m.words.len() * 4).checked_sub(16)? < 128 {
        return None;
    }
    let len = m.bytes_after(4, out);
    if len == 0 || out[0] == 0 {
        return None;
    }
    let end = out[..len].iter().position(|b| *b == 0).unwrap_or(len);
    Some(&out[..end])
}

/// The three version forms `sys_get_prop_image_version` scans (`sscanf` of `%u.%u-%u` after a fixed prefix), as major, minor and
/// revision; `None` if none fits, as Linux logs "error reading F/W version".
pub fn parse_version(text: &[u8]) -> Option<(u32, u32, u32)> {
    const FORMS: [&[u8]; 3] = [b"14:video-firmware.", b"14:VIDEO.VPU.", b"14:VIDEO.VE."];
    let rest = FORMS.iter().find_map(|f| text.strip_prefix(*f))?;
    let mut at = 0;
    let mut number = |sep: Option<u8>| -> Option<u32> {
        let start = at;
        while at < rest.len() && rest[at].is_ascii_digit() {
            at += 1;
        }
        let n = core::str::from_utf8(&rest[start..at]).ok()?.parse().ok()?;
        if let Some(s) = sep {
            if rest.get(at) != Some(&s) {
                return None;
            }
            at += 1;
        }
        Some(n)
    };
    let major = number(Some(b'.'))?;
    let minor = number(Some(b'-'))?;
    let rev = number(None)?;
    Some((major, minor, rev))
}
