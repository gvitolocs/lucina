//! What Linux reads from `ath10k/WCN3990/hw1.0/firmware-5.bin`, fixed here.
//!
//! Linux takes the WMI and HTT flavours and the feature bits from that file's IEs (`ath10k_core_fetch_firmware_api_n`,
//! `core.c`, SPDX ISC). It holds no code on this chip: `wlanmdsp.mbn` runs on the modem. The file in Debian's
//! `firmware-atheros` is 60 bytes; `tests/fixtures/wcn3990_firmware5.txt` has them and `tests/driver.rs` checks these
//! constants against [`parse`] of it:
//!
//! | IE | value | source |
//! |---|---|---|
//! | `WMI_OP_VERSION` | 4, TLV | file; `ATH10K_FW_WMI_OP_VERSION_TLV` in `hw.h` |
//! | `HTT_OP_VERSION` | 3, TLV | file; `ATH10K_FW_HTT_OP_VERSION_TLV` in `hw.h` |
//! | `FEATURES` | bits 6, 18, 19: wowlan, `MGMT_TX_BY_REF`, `NON_BMI` | file; `enum ath10k_fw_features` in `core.h` |
//!
//! The absent bits decide things too: no `HAS_WMI_MGMT_TX` (management goes by `WMI_SERVICE_MGMT_TX_WMI`), no
//! `NO_NWIFI_DECAP_4ADDR_PADDING` (native wifi headers are padded to 4), no `PEER_FLOW_CONTROL` (no queue state in the
//! frag bank), no `MFP_SUPPORT` (no `WMI_PEER_PMF`), no `RAW_MODE_SUPPORT` (crypto stays in the hardware).

use alloc::vec::Vec;

/// `ATH10K_FW_WMI_OP_VERSION_TLV`.
pub const WMI_OP_VERSION: u32 = 4;
/// `ATH10K_FW_HTT_OP_VERSION_TLV`.
pub const HTT_OP_VERSION: u32 = 3;
/// `enum ath10k_fw_features`: the bit numbers this driver is built around.
pub const FEATURE_WOWLAN: u32 = 6;
pub const FEATURE_MGMT_TX_BY_REF: u32 = 18;
pub const FEATURE_NON_BMI: u32 = 19;
/// The whole bitmap of the file.
pub const FEATURES: u32 =
    (1 << FEATURE_WOWLAN) | (1 << FEATURE_MGMT_TX_BY_REF) | (1 << FEATURE_NON_BMI);

/// `ATH10K_FIRMWARE_MAGIC`.
const MAGIC: &[u8] = b"QCA-ATH10K\0";

/// The IEs of a `firmware-N.bin`: `(id, value)`, the walk of `ath10k_core_fetch_firmware_api_n`. Each IE is `id: le32`,
/// `len: le32`, the value padded to 4. `None` for a bad magic or an IE running past the end.
pub fn parse(file: &[u8]) -> Option<Vec<(u32, &[u8])>> {
    if file.get(..MAGIC.len())? != MAGIC {
        return None;
    }
    let mut pos = MAGIC.len().div_ceil(4) * 4;
    let mut out = Vec::new();
    while pos < file.len() {
        let h = file.get(pos..pos + 8)?;
        let id = u32::from_le_bytes([h[0], h[1], h[2], h[3]]);
        let len = u32::from_le_bytes([h[4], h[5], h[6], h[7]]) as usize;
        let start = pos + 8;
        out.push((id, file.get(start..start.checked_add(len)?)?));
        pos = start + len.div_ceil(4) * 4;
    }
    Some(out)
}
