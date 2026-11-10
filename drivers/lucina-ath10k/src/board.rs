//! The board file, Linux `drivers/net/wireless/ath/ath10k/core.c` (`ath10k_core_fetch_board_data_api_n`,
//! `ath10k_core_parse_bd_ie_board`) and `hw.h` (SPDX ISC; Copyright (c) 2005-2011 Atheros Communications Inc.,
//! 2011-2017 Qualcomm Atheros, Inc., 2018-2019 The Linux Foundation).
//!
//! `board-2.bin` is the magic `QCA-ATH10K-BOARD\0`, padded to four bytes, and then elements `{ id: u32, len: u32, data }`,
//! each padded to four bytes. An element with id [`IE_BOARD`] holds further elements of the same form: a name and, after
//! it, the data of that board. The file comes from the host, but it is parsed as if it did not: nothing panics on a bad one.

/// `ATH10K_BOARD_MAGIC`.
pub const MAGIC: &[u8] = b"QCA-ATH10K-BOARD\0";
/// `ATH10K_BD_IE_BOARD`.
pub const IE_BOARD: u32 = 0;
/// `ATH10K_BD_IE_BOARD_NAME`.
pub const IE_BOARD_NAME: u32 = 0;
/// `ATH10K_BD_IE_BOARD_DATA`.
pub const IE_BOARD_DATA: u32 = 1;

/// The elements of a buffer, stopping at the first one that does not fit.
fn elements(mut buf: &[u8]) -> impl Iterator<Item = (u32, &[u8])> {
    core::iter::from_fn(move || {
        let id = u32::from_le_bytes(buf.get(..4)?.try_into().ok()?);
        let len = u32::from_le_bytes(buf.get(4..8)?.try_into().ok()?) as usize;
        let data = buf.get(8..8usize.checked_add(len)?)?;
        let next = 8usize.checked_add(len)?.checked_add(3)? & !3;
        buf = buf.get(next..).unwrap_or(&[]);
        Some((id, data))
    })
}

fn body(file: &[u8]) -> Option<&[u8]> {
    if !file.starts_with(MAGIC) {
        return None;
    }
    file.get((MAGIC.len() + 3) & !3..)
}

/// The data of the board called exactly `name`: what `ath10k_core_parse_bd_ie_board` finds, which is the first data element
/// after a matching name in a board element.
///
/// The caller (the kernel's `board_data`) tries two names, `bus=snoc,qmi-board-id=<id>,qmi-chip-id=<id>` and then
/// `bus=snoc,qmi-board-id=<id>`, with no `,variant=` suffix. `ath10k_core_fetch_board_file` tries three: the first
/// with the variant from the DT (`qcom,calibration-variant`, or the older `qcom,ath10k-calibration-variant`) or SMBIOS,
/// then the same without it, then the board id alone. With no variant the first two are the same name, and this
/// laptop's DT has no variant property, so two names find what Linux finds.
pub fn find<'a>(file: &'a [u8], name: &str) -> Option<&'a [u8]> {
    for (id, board) in elements(body(file)?) {
        if id != IE_BOARD {
            continue;
        }
        let mut matched = false;
        for (id, data) in elements(board) {
            match id {
                IE_BOARD_NAME if data == name.as_bytes() => matched = true,
                IE_BOARD_DATA if matched => return Some(data),
                _ => {}
            }
        }
    }
    None
}

/// Every board name in the file, for a log line when none matches.
pub fn names(file: &[u8]) -> impl Iterator<Item = &[u8]> {
    body(file)
        .into_iter()
        .flat_map(elements)
        .filter(|(id, _)| *id == IE_BOARD)
        .flat_map(|(_, board)| elements(board))
        .filter(|(id, _)| *id == IE_BOARD_NAME)
        .map(|(_, name)| name)
}
