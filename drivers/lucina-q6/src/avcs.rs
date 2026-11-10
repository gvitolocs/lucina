//! `q6core.c`: the framework's version, which `q6afe_probe`, `q6asm_probe` and `q6adm_probe` ask for first, and whether it is ready.
//! Linux's sound stack keeps the version without using it, and never calls `q6core_is_adsp_ready` on this path; asking changes nothing in
//! the DSP and says here that the services answer.

use alloc::vec::Vec;

use crate::apr::{self, Packet};

pub const CMD_GET_STATE: u32 = 0x0001_290C;
pub const RSP_GET_STATE: u32 = 0x0001_290D;
pub const CMD_GET_FWK_VERSION: u32 = 0x0001_292C;
pub const RSP_GET_FWK_VERSION: u32 = 0x0001_292D;
/// `ADSP_EUNSUPPORTED`: firmware without the query, which Linux takes as ready.
const EUNSUPPORTED: u32 = 3;
pub const READY_TIMEOUT_MS: u64 = 3000;
/// `Q6_READY_TIMEOUT_MS`, how long one question waits.
pub const ASK_TIMEOUT_MS: u64 = 100;

pub fn get_fwk_version() -> Vec<u8> {
    apr::build(apr::SVC_CORE, 0, 0, 0, CMD_GET_FWK_VERSION, &[])
}

pub fn get_state() -> Vec<u8> {
    apr::build(apr::SVC_CORE, 0, 0, 0, CMD_GET_STATE, &[])
}

/// What an answer says: `Some(true)` ready, `Some(false)` not yet, `None` not an answer to [`get_state`].
pub fn on_packet(packet: &Packet) -> Option<bool> {
    if packet.src_svc != apr::SVC_CORE {
        return None;
    }
    match packet.opcode {
        RSP_GET_STATE => Some(packet.result_word() != 0),
        apr::BASIC_RSP_RESULT => match packet.result()? {
            (CMD_GET_STATE, EUNSUPPORTED) => Some(true),
            _ => None,
        },
        _ => None,
    }
}

impl Packet<'_> {
    fn result_word(&self) -> u32 {
        self.payload
            .get(..4)
            .and_then(|b| b.try_into().ok())
            .map_or(0, u32::from_le_bytes)
    }
}
