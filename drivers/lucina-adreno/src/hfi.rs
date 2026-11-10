// SPDX-License-Identifier: GPL-2.0-only
//! HFI, the host's messages to the GMU's firmware, in the legacy (v1) form the A618 uses: `a6xx_hfi.c` and `a6xx_hfi.h` of
//! Linux. The layouts and tests started from Scarlet's `hfi_abi.rs` (GPL-2.0-only).
//!
//! The queues are 16 KiB of uncached memory the GMU reaches through its stream, at [`crate::gmu::HFI_IOVA`].

use alloc::{vec, vec::Vec};

use lucina_qcom_soc::io::RegIo;

use crate::{Hal, Result, err, poll, regs};

pub const SIZE: usize = 0x4000;
const TABLE_WORDS: u32 = 6;
const QUEUE_HEADER_WORDS: u32 = 12;
const COMMAND_HEADER: u32 = TABLE_WORDS;
const RESPONSE_HEADER: u32 = TABLE_WORDS + QUEUE_HEADER_WORDS;
const DEBUG_HEADER: u32 = TABLE_WORDS + QUEUE_HEADER_WORDS * 2;
const QUEUE_WORDS: u32 = 0x400;
const COMMAND_DATA: u32 = 0x1000 / 4;
const RESPONSE_DATA: u32 = 0x2000 / 4;

const Q_STATUS: u32 = 0;
const Q_IOVA: u32 = 1;
const Q_TYPE: u32 = 2;
const Q_SIZE: u32 = 3;
const Q_DROPPED: u32 = 5;
const Q_RX_WATERMARK: u32 = 6;
const Q_TX_WATERMARK: u32 = 7;
const Q_RX_REQUEST: u32 = 8;
const Q_READ_INDEX: u32 = 10;
const Q_WRITE_INDEX: u32 = 11;

const H2F_INIT: u8 = 0;
const H2F_FW_VERSION: u8 = 1;
const H2F_BW_TABLE: u8 = 3;
const H2F_PERF_TABLE: u8 = 4;
const H2F_TEST: u8 = 5;
const F2H_ERROR: u8 = 100;
const F2H_ACK: u8 = 126;
const MSG_CMD: u32 = 0;
const MSG_ACK: u32 = 1;
const MSG_ACK_V1: u32 = 2;

const MAX_GPU_LEVELS: usize = 16;
const MAX_GMU_LEVELS: usize = 4;
const HOST_INTERRUPT_MESSAGE: u32 = 1;
/// `a6xx_hfi_wait_for_ack`: poll every 100 us for 1 s.
const TIMEOUT_US: u64 = 1_000_000;
/// `GMU_COLD_BOOT` of `a6xx_gmu.h`: the firmware was just written to the GMU's memory.
pub const GMU_COLD_BOOT: u32 = 1;
/// `GMU_WARM_BOOT`: the firmware was kept in the GMU's retained memory across a power collapse (`GMU_GENERAL_7` reads 1).
pub const GMU_WARM_BOOT: u32 = 0;

/// One line of the performance table: an RPMh vote and a frequency.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PerfLevel {
    pub vote: u32,
    pub khz: u32,
}

fn word<M: RegIo>(mem: &M, index: u32) -> u32 {
    mem.read32(index * 4)
}

fn put<M: RegIo>(mem: &M, index: u32, value: u32) {
    mem.write32(index * 4, value);
}

/// `a6xx_hfi_init`, legacy: the command, response and debug queues behind the table.
pub fn init_table<M: RegIo>(mem: &M, iova: u32) {
    for index in 0..(SIZE as u32 / 4) {
        put(mem, index, 0);
    }
    for (index, value) in [
        0,
        (TABLE_WORDS + QUEUE_HEADER_WORDS * 3) * 4,
        TABLE_WORDS,
        QUEUE_HEADER_WORDS,
        3,
        3,
    ]
    .iter()
    .enumerate()
    {
        put(mem, index as u32, *value);
    }
    let queue = |header: u32, at: u32, id: u32| {
        put(mem, header + Q_STATUS, 1);
        put(mem, header + Q_IOVA, iova + at);
        // `HFI_QUEUE_PRI..`: priority 10, the queue's number.
        put(mem, header + Q_TYPE, (10 << 8) | id);
        put(mem, header + Q_SIZE, QUEUE_WORDS);
        put(mem, header + Q_RX_WATERMARK, 1);
        put(mem, header + Q_TX_WATERMARK, 1);
        put(mem, header + Q_RX_REQUEST, 1);
    };
    queue(COMMAND_HEADER, 0x1000, 0);
    queue(RESPONSE_HEADER, 0x2000, 4);
    queue(DEBUG_HEADER, 0x3000, 5);
}

/// `a6xx_hfi_send_gmu_init`'s payload: the debug buffer and the boot state. This driver always loads the GMU firmware
/// `boot_state` is [`GMU_COLD_BOOT`] or [`GMU_WARM_BOOT`], as `a6xx_gmu_resume` found it.
pub fn gmu_init_message(debug_iova: u32, debug_size: u32, boot_state: u32) -> Vec<u32> {
    vec![0, 0, debug_iova, debug_size, boot_state]
}

/// `a6xx_hfi_send_perf_table_v1`.
pub fn perf_table(gx: &[PerfLevel], cx: &[PerfLevel]) -> Result<Vec<u32>> {
    let valid = |levels: &[PerfLevel], max: usize| {
        !levels.is_empty()
            && levels.len() <= max
            && levels[0].khz == 0
            && levels.windows(2).all(|pair| pair[0].khz < pair[1].khz)
    };
    if !valid(gx, MAX_GPU_LEVELS) || !valid(cx, MAX_GMU_LEVELS) {
        return err("HFI: bad performance table", gx.len() as u32);
    }
    let mut message = vec![0; 3 + MAX_GPU_LEVELS * 2 + MAX_GMU_LEVELS * 2];
    message[1] = gx.len() as u32;
    message[2] = cx.len() as u32;
    for (index, level) in gx.iter().enumerate() {
        message[3 + index * 2] = level.vote;
        message[4 + index * 2] = level.khz;
    }
    let base = 3 + MAX_GPU_LEVELS * 2;
    for (index, level) in cx.iter().enumerate() {
        message[base + index * 2] = level.vote;
        message[base + 1 + index * 2] = level.khz;
    }
    Ok(message)
}

/// `a618_build_bw_table`: one "off" entry, "since the 618 GMU doesn't do bus scaling".
pub fn bw_table() -> Vec<u32> {
    let mut message = vec![0; 160];
    message[1] = 1;
    message[2] = 1;
    message[3] = 3;
    message[4] = 1;
    message[5] = 1;
    message[6] = 0x5007c;
    message[12] = 0x4000_0000;
    message[18] = 0x6000_0001;
    message[24] = 0x50000;
    message[25] = 0x5003c;
    message[26] = 0x5000c;
    message[32] = 0x4000_0000;
    message[33] = 0x4000_0000;
    message[34] = 0x4000_0000;
    message
}

/// Whether a response answers this command; `Err` if the firmware refused it.
pub fn ack_matches(response: &[u32], id: u8, sequence: u32) -> Result<bool> {
    let Some(&header) = response.first() else {
        return err("HFI: empty response", 0);
    };
    let response_id = (header & 0xff) as u8;
    let response_type = (header >> 16) & 0xf;
    if response_id == F2H_ERROR
        || response_id != F2H_ACK
        || !matches!(response_type, MSG_ACK | MSG_ACK_V1)
    {
        return Ok(false);
    }
    if response.len() < 3 {
        return err("HFI: short acknowledgement", header);
    }
    let returned = response[1];
    if (returned & 0xff) as u8 != id
        || (returned >> 16) & 0xf != MSG_CMD
        || (returned >> 20) & 0xfff != sequence
    {
        return Ok(false);
    }
    if response[2] != 0 {
        return err("HFI: the GMU refused a message", response[2]);
    }
    Ok(true)
}

/// The queues in shared memory.
pub struct Hfi<M> {
    mem: M,
    sequence: u32,
}

impl<M: RegIo> Hfi<M> {
    pub fn new(mem: M, iova: u32) -> Self {
        init_table(&mem, iova);
        Self { mem, sequence: 0 }
    }

    fn write_command<H: Hal>(&mut self, hal: &H, message: &[u32]) -> Result<()> {
        let read = word(&self.mem, COMMAND_HEADER + Q_READ_INDEX);
        let write = word(&self.mem, COMMAND_HEADER + Q_WRITE_INDEX);
        if read >= QUEUE_WORDS || write >= QUEUE_WORDS {
            return err("HFI: corrupt command queue", read);
        }
        let space = if write >= read {
            QUEUE_WORDS - (write - read) - 1
        } else {
            read - write - 1
        };
        if (space as usize) < message.len() {
            let dropped = word(&self.mem, COMMAND_HEADER + Q_DROPPED);
            put(
                &self.mem,
                COMMAND_HEADER + Q_DROPPED,
                dropped.wrapping_add(1),
            );
            return err("HFI: command queue full", space);
        }
        let mut index = write;
        for value in message {
            put(&self.mem, COMMAND_DATA + index, *value);
            index = (index + 1) % QUEUE_WORDS;
        }
        hal.barrier();
        put(&self.mem, COMMAND_HEADER + Q_WRITE_INDEX, index);
        hal.barrier();
        Ok(())
    }

    fn read_response<H: Hal>(&mut self, hal: &H) -> Result<Option<Vec<u32>>> {
        let read = word(&self.mem, RESPONSE_HEADER + Q_READ_INDEX);
        let write = word(&self.mem, RESPONSE_HEADER + Q_WRITE_INDEX);
        if read >= QUEUE_WORDS || write >= QUEUE_WORDS {
            return err("HFI: corrupt response queue", write);
        }
        if read == write {
            put(&self.mem, RESPONSE_HEADER + Q_RX_REQUEST, 1);
            return Ok(None);
        }
        let header = word(&self.mem, RESPONSE_DATA + read);
        let dwords = (header >> 8) & 0xff;
        if dwords == 0 || dwords > 19 {
            return err("HFI: bad response length", header);
        }
        let mut response = Vec::with_capacity(dwords as usize);
        let mut index = read;
        for _ in 0..dwords {
            response.push(word(&self.mem, RESPONSE_DATA + index));
            index = (index + 1) % QUEUE_WORDS;
        }
        hal.barrier();
        put(&self.mem, RESPONSE_HEADER + Q_READ_INDEX, index);
        hal.barrier();
        Ok(Some(response))
    }

    /// Sends one message, waits for its acknowledgement. `window` is [`regs::WINDOW_BASE`].
    pub fn send<H: Hal, W: RegIo>(
        &mut self,
        hal: &H,
        window: &W,
        id: u8,
        mut message: Vec<u32>,
    ) -> Result<Vec<u32>> {
        self.sequence = ((self.sequence + 1) & 0xfff).max(1);
        let sequence = self.sequence;
        if message.is_empty() || message.len() > 0xff {
            return err("HFI: bad message", message.len() as u32);
        }
        message[0] =
            (sequence << 20) | (MSG_CMD << 16) | ((message.len() as u32) << 8) | u32::from(id);
        self.write_command(hal, &message)?;
        window.write32(regs::GMU_HOST2GMU_INTR_SET * 4, HOST_INTERRUPT_MESSAGE);
        hal.barrier();
        loop {
            poll(
                hal,
                "HFI: no answer from the GMU",
                TIMEOUT_US,
                100,
                || window.read32(regs::GMU_GMU2HOST_INTR_INFO * 4),
                |v| v & HOST_INTERRUPT_MESSAGE != 0,
            )?;
            window.write32(regs::GMU_GMU2HOST_INTR_CLR * 4, HOST_INTERRUPT_MESSAGE);
            while let Some(response) = self.read_response(hal)? {
                if ack_matches(&response, id, sequence)? {
                    return Ok(response);
                }
                hal.log(format_args!(
                    "gpu: hfi: other message {:#x} {:#x}",
                    response[0],
                    response.get(1).copied().unwrap_or(0)
                ));
            }
        }
    }

    /// `a6xx_hfi_stop`: empties every queue for the next boot. Returns which queues still held words, as Linux reports them.
    pub fn stop(&mut self) -> u32 {
        let mut not_empty = 0;
        for (index, header) in [COMMAND_HEADER, RESPONSE_HEADER, DEBUG_HEADER].into_iter().enumerate() {
            if word(&self.mem, header + Q_READ_INDEX) != word(&self.mem, header + Q_WRITE_INDEX) {
                not_empty |= 1 << index;
            }
            put(&self.mem, header + Q_READ_INDEX, 0);
            put(&self.mem, header + Q_WRITE_INDEX, 0);
        }
        not_empty
    }

    /// `a6xx_hfi_start_v1`.
    pub fn start_v1<H: Hal, W: RegIo>(
        &mut self,
        hal: &H,
        window: &W,
        debug_iova: u32,
        debug_size: u32,
        boot_state: u32,
        gx: &[PerfLevel],
        cx: &[PerfLevel],
    ) -> Result<()> {
        hal.log(format_args!("gpu: hfi: GMU_INIT"));
        self.send(hal, window, H2F_INIT, gmu_init_message(debug_iova, debug_size, boot_state))?;
        hal.log(format_args!("gpu: hfi: FW_VERSION"));
        let version = self.send(
            hal,
            window,
            H2F_FW_VERSION,
            vec![0, (1 << 28) | (1 << 19) | (1 << 17)],
        )?;
        hal.log(format_args!(
            "gpu: hfi: firmware says version {:#x}",
            version.get(3).copied().unwrap_or(0)
        ));
        hal.log(format_args!(
            "gpu: hfi: PERF_TABLE, {} GPU levels, {} GMU levels",
            gx.len(),
            cx.len()
        ));
        self.send(hal, window, H2F_PERF_TABLE, perf_table(gx, cx)?)?;
        hal.log(format_args!("gpu: hfi: BW_TABLE"));
        self.send(hal, window, H2F_BW_TABLE, bw_table())?;
        // "Let the GMU know that there won't be any more HFI messages until next boot".
        hal.log(format_args!("gpu: hfi: TEST"));
        self.send(hal, window, H2F_TEST, vec![0])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use core::cell::RefCell;
    use std::vec::Vec as StdVec;

    use super::*;

    struct Mem(RefCell<StdVec<u32>>);

    impl RegIo for Mem {
        fn read32(&self, off: u32) -> u32 {
            self.0.borrow()[(off / 4) as usize]
        }
        fn write32(&self, off: u32, value: u32) {
            self.0.borrow_mut()[(off / 4) as usize] = value;
        }
    }

    #[test]
    fn the_table_is_linuxs_168_bytes_with_three_queues() {
        let mem = Mem(RefCell::new(alloc::vec![0xdead_beef; SIZE / 4]));
        init_table(&mem, 0x6000_9000);
        let words = mem.0.borrow();
        assert_eq!(&words[..6], &[0, 168, 6, 12, 3, 3]);
        assert_eq!(
            &words[6..18],
            &[1, 0x6000_a000, 0x0a00, 0x400, 0, 0, 1, 1, 1, 0, 0, 0]
        );
        assert_eq!(
            &words[18..30],
            &[1, 0x6000_b000, 0x0a04, 0x400, 0, 0, 1, 1, 1, 0, 0, 0]
        );
        assert_eq!(
            &words[30..42],
            &[1, 0x6000_c000, 0x0a05, 0x400, 0, 0, 1, 1, 1, 0, 0, 0]
        );
    }

    #[test]
    fn the_init_message_says_cold_boot() {
        // `a6xx_hfi_send_gmu_init` with `GMU_COLD_BOOT`: the last word is the boot state.
        let message = gmu_init_message(0x6000_1000, 0x4000, GMU_COLD_BOOT);
        assert_eq!(message, alloc::vec![0, 0, 0x6000_1000, 0x4000, 1]);
    }

    #[test]
    fn perf_table_has_the_v1_shape() {
        let gx = [
            PerfLevel { vote: 0x11, khz: 0 },
            PerfLevel {
                vote: 0x22,
                khz: 180_000,
            },
            PerfLevel {
                vote: 0x55,
                khz: 800_000,
            },
        ];
        let cx = [
            PerfLevel { vote: 0x33, khz: 0 },
            PerfLevel {
                vote: 0x44,
                khz: 200_000,
            },
        ];
        let table = perf_table(&gx, &cx).unwrap();
        assert_eq!(table.len(), 43);
        assert_eq!(
            &table[..9],
            &[0, 3, 2, 0x11, 0, 0x22, 180_000, 0x55, 800_000]
        );
        assert_eq!(&table[35..39], &[0x33, 0, 0x44, 200_000]);
        assert!(perf_table(&[gx[1]], &cx).is_err());
    }

    #[test]
    fn bw_table_offsets_match_struct_a6xx_hfi_msg_bw_table() {
        let table = bw_table();
        assert_eq!(table.len(), 160);
        let nonzero: StdVec<_> = table
            .iter()
            .enumerate()
            .filter(|(_, v)| **v != 0)
            .map(|(i, v)| (i, *v))
            .collect();
        assert_eq!(nonzero.len(), 14);
        assert_eq!(nonzero[0], (1, 1));
        assert_eq!(nonzero[13], (34, 0x4000_0000));
    }

    #[test]
    fn only_a_matching_ack_completes_a_command() {
        let returned = (7 << 20) | 4;
        assert_eq!(
            ack_matches(&[(1 << 16) | (3 << 8) | 126, returned, 0], 4, 7),
            Ok(true)
        );
        assert_eq!(
            ack_matches(&[(2 << 16) | (3 << 8) | 126, returned, 0], 4, 7),
            Ok(true)
        );
        assert_eq!(
            ack_matches(&[(3 << 16) | (3 << 8) | 126, returned, 0], 4, 7),
            Ok(false)
        );
        assert_eq!(
            ack_matches(&[(1 << 16) | (3 << 8) | 42, returned, 0], 4, 7),
            Ok(false)
        );
        assert!(ack_matches(&[(1 << 16) | (3 << 8) | 126, returned, 1], 4, 7).is_err());
    }
}
