// SPDX-License-Identifier: GPL-2.0-only
//! The video block's processor after the secure world started its firmware: telling it where the queues are, letting it run, the
//! interrupt, and the first commands. `venus_run`, `venus_boot_core`, `venus_isr`, `venus_isr_thread`, `venus_core_init` and
//! `venus_halt_axi` of Linux's hfi_venus.c for 4xx (not IRIS2, not AR50_LITE).
//!
//! Two things are stricter than Linux. The boot poll stops on any error code the firmware puts in `CPU_CS_SCIACMDARG0` (Linux stops
//! only on 4, "invalid setting for UC_REGION", and takes anything else nonzero as started), and the watchdog bit of the wrapper's
//! interrupt status is taken as the firmware's death (Linux wakes its thread and handles nothing).

use core::fmt;

use lucina_qcom_soc::io::RegIo;

use crate::{
    Error, Hal, Result, err,
    hfi::{self, Message},
    iface::{Iface, Queue, SHARED_SIZE, Shared},
    regs,
    rules::{MAX_PACKET_WORDS, QueueError},
};

/// How long the boot poll waits: Linux's 100 tries of 0.5 to 1 ms.
const BOOT_TIMEOUT_US: u64 = 100_000;
/// `venus_hw_rsp_timeout`: how long an answer may take.
pub const ANSWER_TIMEOUT_US: u64 = 1_000_000;

/// What the wrapper's interrupt status said.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Interrupt {
    pub status: u32,
}

impl Interrupt {
    /// `WRAPPER_INTR_STATUS_A2HWD_MASK`: the firmware's watchdog fired.
    pub fn watchdog(&self) -> bool {
        self.status & regs::WRAPPER_INTR_STATUS_A2HWD != 0
    }
}

/// The block's processor, its window and the shared queues.
pub struct Core<R, S> {
    pub regs: R,
    pub iface: Iface<S>,
    /// The firmware said `HFI_EVENT_SYS_ERROR`, or its watchdog fired: nothing more is sent (Linux's `VENUS_STATE_DEINIT`).
    dead: bool,
    /// Status the kernel's interrupt handler read and acknowledged ([`Self::note_interrupt`]), not yet taken by
    /// [`Self::acknowledge`]: Linux's `hdev->irq_status`, set in `venus_isr` and read in `venus_isr_thread`.
    seen: u32,
}

impl<R: RegIo, S: Shared> Core<R, S> {
    pub fn new(regs: R, iface: Iface<S>) -> Self {
        Self { regs, iface, dead: false, seen: 0 }
    }

    /// What the kernel's interrupt handler read from `WRAPPER_INTR_STATUS` (and cleared there): the next
    /// [`Self::acknowledge`] reports it, so that a watchdog bit the handler took is not lost.
    pub fn note_interrupt(&mut self, status: u32) {
        self.seen |= status;
    }

    pub fn is_dead(&self) -> bool {
        self.dead
    }

    /// `venus_run` and `venus_boot_core`: where the queues and the SFR are, then the processor out of its init state, then wait for it
    /// to say it started. Returns `CPU_CS_SCIACMDARG0` and the hardware version.
    pub fn run<H: Hal>(&self, hal: &H) -> Result<(u32, u32)> {
        let table = self.iface.table_address();
        self.regs.write32(regs::UC_REGION_ADDR, table);
        self.regs.write32(regs::UC_REGION_SIZE, SHARED_SIZE);
        self.regs.write32(regs::CPU_CS_SCIACMDARG2, table);
        self.regs.write32(regs::CPU_CS_SCIACMDARG1, 0x01);
        self.regs.write32(regs::SFR_ADDR, self.iface.sfr_address());
        self.regs.write32(regs::WRAPPER_INTR_MASK, regs::WRAPPER_INTR_MASK_A2HVCODEC);
        self.regs.write32(regs::VIDC_CTRL_INIT, regs::VIDC_CTRL_INIT_CTRL);
        let start = hal.now_us();
        let status = loop {
            let status = self.regs.read32(regs::CPU_CS_SCIACMDARG0);
            let error = status & regs::SCIACMDARG0_ERROR_STATUS_MASK;
            if error == 4 {
                return err("the firmware refused the UC region", status);
            }
            if error != 0 {
                return err("the firmware reported an error starting", status);
            }
            if status != 0 {
                break status;
            }
            if hal.now_us().saturating_sub(start) >= BOOT_TIMEOUT_US {
                return err("the firmware did not start", status);
            }
            hal.delay_us(500);
        };
        Ok((status, self.regs.read32(regs::WRAPPER_HW_VERSION)))
    }

    /// `venus_iface_cmdq_write_nolock`: one command, and the doorbell if the firmware asked for it.
    pub fn send(&self, packet: &[u32], sync: bool) -> Result<()> {
        if self.dead {
            return err("the firmware is dead", 0);
        }
        match self.iface.write_command(packet, sync) {
            Ok(true) => {
                self.regs.write32(regs::CPU_IC_SOFTINT, regs::CPU_IC_SOFTINT_H2A);
                Ok(())
            }
            Ok(false) => Ok(()),
            Err(e) => err(queue_error(e), packet.get(1).copied().unwrap_or(0)),
        }
    }

    /// `venus_isr`: read and acknowledge the interrupt, with what the kernel's handler already took ([`Self::note_interrupt`]).
    /// The caller then reads the queues ([`Self::messages`]).
    pub fn acknowledge(&mut self) -> Interrupt {
        let status = self.regs.read32(regs::WRAPPER_INTR_STATUS);
        self.regs.write32(regs::CPU_CS_A2HSOFTINTCLR, 1);
        self.regs.write32(regs::WRAPPER_INTR_CLEAR, status);
        let i = Interrupt { status: status | core::mem::take(&mut self.seen) };
        if i.watchdog() {
            self.dead = true;
        }
        i
    }

    /// `venus_isr_thread`: every message waiting, to `each`, then the debug text to `debug`. A `SYS_ERROR` event marks the firmware
    /// dead, as Linux's `venus_process_msg_sys_error` does, after `each` has seen it.
    pub fn messages(
        &mut self,
        buf: &mut [u32; MAX_PACKET_WORDS as usize],
        mut each: impl FnMut(&Message),
        mut debug: impl FnMut(&[u8]),
    ) -> Result<usize> {
        let mut count = 0;
        loop {
            match self.iface.read(Queue::Message, buf) {
                Ok(Some((len, tx_req))) => {
                    if tx_req {
                        self.regs.write32(regs::CPU_IC_SOFTINT, regs::CPU_IC_SOFTINT_H2A);
                    }
                    let m = Message { words: &buf[..len] };
                    if m.kind() == hfi::msg::EVENT_NOTIFY && m.word(3) == Some(hfi::EVENT_SYS_ERROR) {
                        self.dead = true;
                    }
                    each(&m);
                    count += 1;
                }
                Ok(None) => break,
                Err(QueueError::BadPacket) => debug(b"venus: a message that cannot be right was dropped"),
                Err(e) => return err(queue_error(e), 0),
            }
        }
        let mut text = [0u8; 256];
        while let Ok(Some((len, tx_req))) = self.iface.read(Queue::Debug, buf) {
            if tx_req {
                self.regs.write32(regs::CPU_IC_SOFTINT, regs::CPU_IC_SOFTINT_H2A);
            }
            let m = Message { words: &buf[..len] };
            // struct hfi_msg_sys_debug_pkt: header, msg_type, msg_size, time stamp high and low, then the text.
            if m.kind() == hfi::msg::SYS_DEBUG {
                let n = m.bytes_after(6, &mut text);
                let end = text[..n].iter().position(|b| *b == 0).unwrap_or(n);
                debug(&text[..end]);
            }
        }
        Ok(count)
    }

    /// `venus_halt_axi` for 4xx: stop the processor's bus master and wait until it is idle.
    pub fn halt_axi<H: Hal>(&self, hal: &H) -> Result<u32> {
        let v = self.regs.read32(regs::WRAPPER_CPU_AXI_HALT);
        self.regs.write32(regs::WRAPPER_CPU_AXI_HALT, v | 1 << 16);
        crate::poll(hal, "the processor's bus did not halt", regs::VBIF_AXI_HALT_ACK_TIMEOUT_US, 50, || {
            self.regs.read32(regs::WRAPPER_CPU_AXI_HALT_STATUS)
        }, |s| s & 1 << 24 != 0)
    }
}

fn queue_error(e: QueueError) -> &'static str {
    match e {
        QueueError::Full => "the command queue is full",
        QueueError::BadPacket => "a packet of the wrong size",
        QueueError::Corrupt => "a queue index past its end: the shared memory is not what it was",
    }
}

impl fmt::Display for Interrupt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "interrupt status {:#x}{}", self.status, if self.watchdog() { " (watchdog)" } else { "" })
    }
}

/// Waits for one message of `kind` with [`Core::acknowledge`] and [`Core::messages`] in a loop, for those times the kernel polls rather
/// than takes the interrupt (the first start). Other messages go to `other`.
pub fn wait_for<R: RegIo, S: Shared, H: Hal>(
    core: &mut Core<R, S>,
    hal: &H,
    kind: u32,
    timeout_us: u64,
    buf: &mut [u32; MAX_PACKET_WORDS as usize],
    mut found: impl FnMut(&Message),
    mut other: impl FnMut(&Message),
) -> Result<()> {
    let start = hal.now_us();
    loop {
        let mut seen = false;
        let i = core.acknowledge();
        if i.watchdog() {
            return err("the firmware's watchdog fired", i.status);
        }
        core.messages(
            buf,
            |m| {
                if m.kind() == kind && !seen {
                    seen = true;
                    found(m);
                } else {
                    other(m);
                }
            },
            |text| hal.log(format_args!("venus: firmware: {}", core::str::from_utf8(text).unwrap_or("(not text)"))),
        )?;
        if seen {
            return Ok(());
        }
        if core.is_dead() {
            return err("the firmware reported a system error", 0);
        }
        if hal.now_us().saturating_sub(start) >= timeout_us {
            return Err(Error { what: "no answer from the firmware", value: kind });
        }
        hal.delay_us(200);
    }
}
