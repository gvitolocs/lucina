//! What the other remote processors are told when the DSP stops and starts, as Linux's remoteproc does: GLINK's `glink_ssr` channel
//! (`qcom_glink_ssr.c`), so that each edge drops what it had with the DSP, and sysmon's SSCTL events (`qcom_sysmon.c`), which say the DSP
//! is about to stop, has stopped, is about to start and has started. On the laptop the modem died when the DSP restarted without these.
//!
//! The order of `rproc_stop` and `rproc_start` with the subdevices `qcom_q6v5_pas.c` adds (GLINK, sysmon, SSR):
//!
//! | When | Linux | Here |
//! |---|---|---|
//! | before the stop | `sysmon_stop`: [`BEFORE_SHUTDOWN`] to every other running remote | [`subsys_event`] to the modem |
//! | after it | `sysmon_unprepare`: [`AFTER_SHUTDOWN`] | the same |
//! | after it | `glink_subdev_unprepare` -> `qcom_glink_ssr_notify("lpass")`: [`do_cleanup`] on every other edge's `glink_ssr`, a second each | the same, on the modem's edge |
//! | before the start | `sysmon_prepare`: [`BEFORE_POWERUP`] | the same |
//! | after it | `sysmon_start`: [`AFTER_POWERUP`] to the others, and the others' states to the started one | the first half |

use alloc::vec::Vec;

use lucina_qcom_services::qmi::{self, Builder};

// qcom_glink_ssr.c
const GLINK_SSR_DO_CLEANUP: u32 = 0;
const GLINK_SSR_CLEANUP_DONE: u32 = 1;
/// `sizeof(struct do_cleanup_msg)`: four words and a name of 32.
pub const DO_CLEANUP_SIZE: usize = 48;
const NAME_SIZE: usize = 32;
/// `wait_for_completion_timeout(&ssr->completion, HZ)`.
pub const CLEANUP_TIMEOUT_MS: u64 = 1000;
/// The channel's name on every edge.
pub const CHANNEL: &str = "glink_ssr";
/// `ssr_name` of `sm8250_adsp_resource`.
pub const DSP_SSR_NAME: &str = "lpass";

// qcom_sysmon.c
/// `SSCTL` service; its instance is the remote's `ssctl_id`, version 2 in the low byte of QRTR's word.
pub const SSCTL_SERVICE: u32 = 43;
pub const SSCTL_VERSION: u32 = 2;
/// `ssctl_id` of `mpss_resource_init`.
pub const MODEM_SSCTL_ID: u32 = 0x12;
pub const MODEM_SSCTL_INSTANCE_WORD: u32 = SSCTL_VERSION | (MODEM_SSCTL_ID << 8);
/// `sysmon_name` of `sm8250_adsp_resource`.
pub const DSP_SYSMON_NAME: &str = "adsp";
const SSCTL_SUBSYS_EVENT_REQ: u16 = 0x23;
const SSCTL_SUBSYS_NAME_LENGTH: usize = 15;
/// `SSCTL_SSR_EVENT_FORCED`, what Linux always sends.
const SSCTL_SSR_EVENT_FORCED: u32 = 0;
/// `5 * HZ` for the answer.
pub const EVENT_TIMEOUT_MS: u64 = 5000;

pub const BEFORE_POWERUP: u32 = 0;
pub const AFTER_POWERUP: u32 = 1;
pub const BEFORE_SHUTDOWN: u32 = 2;
pub const AFTER_SHUTDOWN: u32 = 3;

/// `struct do_cleanup_msg` for `name`, cut to its 32 bytes as `strscpy` does (with room for the NUL).
pub fn do_cleanup(seq: u32, name: &str) -> Vec<u8> {
    let name = &name.as_bytes()[..name.len().min(NAME_SIZE - 1)];
    let mut out = Vec::with_capacity(DO_CLEANUP_SIZE);
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&GLINK_SSR_DO_CLEANUP.to_le_bytes());
    out.extend_from_slice(&seq.to_le_bytes());
    out.extend_from_slice(&(name.len() as u32).to_le_bytes());
    out.extend_from_slice(name);
    out.resize(DO_CLEANUP_SIZE, 0);
    out
}

/// `qcom_glink_ssr_callback`: the sequence number of a `cleanup_done_msg`, version 0. `None` for anything else.
pub fn cleanup_done(buf: &[u8]) -> Option<u32> {
    let word = |i: usize| {
        buf.get(i * 4..i * 4 + 4)
            .and_then(|b| b.try_into().ok())
            .map(u32::from_le_bytes)
    };
    (word(0)? == 0 && word(1)? == GLINK_SSR_CLEANUP_DONE).then_some(word(2)?)
}

/// `ssctl_send_event`: `ssctl_subsys_event_req` for subsystem `name` and `event`, forced.
pub fn subsys_event(txn: u16, name: &str, event: u32) -> Vec<u8> {
    let name = &name.as_bytes()[..name.len().min(SSCTL_SUBSYS_NAME_LENGTH - 1)];
    let mut value = Vec::with_capacity(1 + name.len());
    value.push(name.len() as u8);
    value.extend_from_slice(name);
    let mut b = Builder::new();
    b.tlv(0x01, &value);
    b.u32(0x02, event);
    b.u32(0x10, SSCTL_SSR_EVENT_FORCED);
    b.finish(qmi::REQUEST, txn, SSCTL_SUBSYS_EVENT_REQ)
}

/// The answer to [`subsys_event`] number `txn`: `Some(true)` taken, `Some(false)` refused, `None` not that answer.
pub fn subsys_event_answer(buf: &[u8], txn: u16) -> Option<bool> {
    let message = qmi::parse(buf).ok()?;
    if message.header.kind != qmi::RESPONSE
        || message.header.msg_id != SSCTL_SUBSYS_EVENT_REQ
        || message.header.txn != txn
    {
        return None;
    }
    Some(matches!(message.result(), Ok((qmi::RESULT_SUCCESS, _))))
}
