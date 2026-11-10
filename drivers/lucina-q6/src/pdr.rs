//! The service registry listener of `pdr_interface.c`, for one protection domain: Linux holds APR's services back until
//! `avs/audio` reports up. Its locator step is skipped, because the answer is in `adspua.jsn`: `msm/adsp/audio_pd` on
//! notifier instance 74. Messages are `qcom_pdr_msg.c`'s.

use alloc::vec::Vec;

use lucina_qcom_services::qmi::{self, Builder};

/// `SERVREG_NOTIFIER_SERVICE`.
pub const NOTIFIER_SERVICE: u32 = 0x42;
/// `qmi_add_lookup(&pdr->notifier_hdl, pds->service, 1, pds->instance)`.
pub const NOTIFIER_VERSION: u32 = 1;
pub const AUDIO_PD_INSTANCE: u32 = 74;
pub const AUDIO_PD_PATH: &str = "msm/adsp/audio_pd";
/// The word QRTR's name service carries: version in the low byte, instance above.
pub const AUDIO_PD_INSTANCE_WORD: u32 = NOTIFIER_VERSION | (AUDIO_PD_INSTANCE << 8);

const REGISTER_LISTENER_REQ: u16 = 0x20;
const STATE_UPDATED_IND: u16 = 0x22;
const SET_ACK_REQ: u16 = 0x23;

pub const STATE_DOWN: u32 = 0x0FFF_FFFF;
pub const STATE_UP: u32 = 0x1FFF_FFFF;
pub const STATE_EARLY_DOWN: u32 = 0x2FFF_FFFF;
pub const STATE_UNINIT: u32 = 0x7FFF_FFFF;

/// `servreg_register_listener_req` with `enable = true`.
pub fn register_listener(txn: u16, path: &str) -> Vec<u8> {
    let mut b = Builder::new();
    b.u8(0x01, 1);
    b.tlv(0x02, path.as_bytes());
    b.finish(qmi::REQUEST, txn, REGISTER_LISTENER_REQ)
}

/// `servreg_set_ack_req`.
pub fn set_ack(txn: u16, path: &str, transaction_id: u16) -> Vec<u8> {
    let mut b = Builder::new();
    b.tlv(0x01, path.as_bytes());
    b.u16(0x02, transaction_id);
    b.finish(qmi::REQUEST, txn, SET_ACK_REQ)
}

/// What a message from the notifier says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    /// The answer to [`register_listener`], with the state at that moment if it gave one.
    Registered { state: Option<u32> },
    /// The registration was refused.
    Refused { error: u16 },
    /// The state changed; needs a [`set_ack`] with `transaction_id`.
    Changed { state: u32, transaction_id: u16 },
    /// Nothing for us.
    Other,
}

pub fn on_message(buf: &[u8]) -> Notice {
    let Ok(message) = qmi::parse(buf) else {
        return Notice::Other;
    };
    match (message.header.kind, message.header.msg_id) {
        (qmi::RESPONSE, REGISTER_LISTENER_REQ) => match message.result() {
            Ok((qmi::RESULT_SUCCESS, _)) => Notice::Registered {
                state: message.u32(0x10).ok().flatten(),
            },
            Ok((_, error)) => Notice::Refused { error },
            Err(_) => Notice::Other,
        },
        (qmi::INDICATION, STATE_UPDATED_IND) => match (message.u32(0x01), message.u16(0x03)) {
            (Ok(Some(state)), Ok(Some(transaction_id))) => Notice::Changed {
                state,
                transaction_id,
            },
            _ => Notice::Other,
        },
        _ => Notice::Other,
    }
}
