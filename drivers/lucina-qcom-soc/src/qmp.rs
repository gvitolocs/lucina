//! The AOSS mailbox (QMP), Linux 7.1 `drivers/soc/qcom/qcom_aoss.c` (GPL-2.0-only). The message RAM at 0xc300000 only
//! takes 32-bit accesses. A PAS start sends one message on it, `load_state` of the modem. [`Qmp::describe`] only reads,
//! for `lucina.wifi=report`; [`Qmp::open`] and [`Qmp::send`] write and are never used in that mode.

use crate::io::RegIo;

const DESC_MAGIC: u32 = 0x0;
const DESC_VERSION: u32 = 0x4;
const DESC_UCORE_LINK_STATE: u32 = 0xc;
const DESC_UCORE_LINK_STATE_ACK: u32 = 0x10;
const DESC_UCORE_CH_STATE: u32 = 0x14;
const DESC_UCORE_CH_STATE_ACK: u32 = 0x18;
const DESC_MCORE_LINK_STATE: u32 = 0x24;
const DESC_MCORE_LINK_STATE_ACK: u32 = 0x28;
const DESC_MCORE_CH_STATE: u32 = 0x2c;
const DESC_MCORE_CH_STATE_ACK: u32 = 0x30;
const DESC_MCORE_MBOX_SIZE: u32 = 0x34;
const DESC_MCORE_MBOX_OFFSET: u32 = 0x38;
const STATE_UP: u32 = 0xffff;
const STATE_DOWN: u32 = 0xffff_0000;

pub const MAGIC: u32 = 0x4d41_494c;
pub const VERSION: u32 = 1;
pub const MSG_LEN: usize = 64;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Desc {
    pub magic: u32,
    pub version: u32,
    pub mbox_offset: u32,
    pub mbox_size: u32,
    pub ucore_link: u32,
    pub mcore_link: u32,
    pub ucore_channel: u32,
    pub mcore_channel: u32,
}

impl Desc {
    pub fn valid(&self) -> bool {
        self.magic == MAGIC && self.version == VERSION && self.mbox_size != 0
    }
}

/// Reads the descriptor without touching it.
pub fn describe<R: RegIo>(mem: &R) -> Desc {
    Desc {
        magic: mem.read32(DESC_MAGIC),
        version: mem.read32(DESC_VERSION),
        mbox_offset: mem.read32(DESC_MCORE_MBOX_OFFSET),
        mbox_size: mem.read32(DESC_MCORE_MBOX_SIZE),
        ucore_link: mem.read32(DESC_UCORE_LINK_STATE),
        mcore_link: mem.read32(DESC_MCORE_LINK_STATE),
        ucore_channel: mem.read32(DESC_UCORE_CH_STATE),
        mcore_channel: mem.read32(DESC_MCORE_CH_STATE),
    }
}

/// `"{class: image, res: load_state, name: %s, val: %s}"` of `q6v5_load_state_toggle`, zero padded to [`MSG_LEN`].
/// `None` if it does not fit (`qmp_send` refuses `len >= 64`).
pub fn load_state_message(name: &str, on: bool) -> Option<[u8; MSG_LEN]> {
    let val = if on { "on" } else { "off" };
    let mut buf = [0u8; MSG_LEN];
    let mut at = 0;
    for part in [
        "{class: image, res: load_state, name: ",
        name,
        ", val: ",
        val,
        "}",
    ] {
        let end = at + part.len();
        if end >= MSG_LEN {
            return None;
        }
        buf[at..end].copy_from_slice(part.as_bytes());
        at = end;
    }
    Some(buf)
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum QmpError {
    BadDescriptor,
    /// The always-on processor did not answer in time. Our caller decides how long to poll.
    NotUp,
    TooLong,
    Busy,
}

/// The link and channel handshake and one sender. Polled: the caller rings `kick` (APCS IPC bit 0, the `mboxes` of
/// `power-management@c300000`) and calls the `step`s again after a wait. Linux waits on SPI 389 instead.
pub struct Qmp<R> {
    mem: R,
    offset: u32,
    size: u32,
}

impl<R: RegIo> Qmp<R> {
    /// `qmp_open`, first part: checks, acks the remote's link state, sets ours up. Then the caller kicks.
    pub fn open(mem: R) -> Result<Self, QmpError> {
        let d = describe(&mem);
        if !d.valid() {
            return Err(QmpError::BadDescriptor);
        }
        mem.write32(DESC_UCORE_LINK_STATE_ACK, mem.read32(DESC_UCORE_LINK_STATE));
        mem.write32(DESC_MCORE_LINK_STATE, STATE_UP);
        Ok(Self {
            mem,
            offset: d.mbox_offset,
            size: d.mbox_size,
        })
    }

    /// `qmp_link_acked`: once true the caller calls [`Qmp::channel_up`] and kicks.
    pub fn link_acked(&self) -> bool {
        self.mem.read32(DESC_MCORE_LINK_STATE_ACK) == STATE_UP
    }

    /// `qmp_open`, second part.
    pub fn channel_up(&self) {
        self.mem.write32(DESC_MCORE_CH_STATE, STATE_UP);
    }

    /// `qmp_ucore_channel_up` then `qmp_mcore_channel_acked` with its acknowledgement. True when the channel is open
    /// in both directions; the caller kicks once after the first true result.
    pub fn poll_channel(&self) -> bool {
        if self.mem.read32(DESC_UCORE_CH_STATE) == STATE_UP {
            self.mem.write32(DESC_UCORE_CH_STATE_ACK, STATE_UP);
        }
        self.mem.read32(DESC_UCORE_CH_STATE_ACK) == STATE_UP
            && self.mem.read32(DESC_MCORE_CH_STATE_ACK) == STATE_UP
    }

    /// `qmp_send` up to the kick: the message after the length word, then the length, read back to post it.
    pub fn send(&self, msg: &[u8; MSG_LEN]) -> Result<(), QmpError> {
        if self.size < MSG_LEN as u32 + 4 {
            return Err(QmpError::TooLong);
        }
        if self.mem.read32(self.offset) != 0 {
            return Err(QmpError::Busy);
        }
        for (i, c) in msg.as_chunks::<4>().0.iter().enumerate() {
            let word = u32::from_le_bytes([c[0], c[1], c[2], c[3]]);
            self.mem.write32(self.offset + 4 + i as u32 * 4, word);
        }
        self.mem.write32(self.offset, MSG_LEN as u32);
        self.mem.read32(self.offset);
        Ok(())
    }

    /// `qmp_message_empty`: the remote cleared the length.
    pub fn message_taken(&self) -> bool {
        self.mem.read32(self.offset) == 0
    }

    /// The timeout path of `qmp_send`: clear the message.
    pub fn abandon(&self) {
        self.mem.write32(self.offset, 0);
    }

    /// `qmp_close`: both states down.
    pub fn close(&self) {
        self.mem.write32(DESC_MCORE_CH_STATE, STATE_DOWN);
        self.mem.write32(DESC_MCORE_LINK_STATE, STATE_DOWN);
    }
}
