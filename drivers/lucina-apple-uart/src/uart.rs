// SPDX-License-Identifier: GPL-2.0-only
// Derived from drivers/tty/serial/samsung_tty.c of AsahiLinux/linux (asahi, rev 6ba0637d1ed9; Ben Dooks, Copyright (c)
// 2003-2008 Simtec Electronics) and include/linux/serial_s3c.h (Shane Nay, Simtec Electronics). See the crate README.
// This is a Rust port of the behaviour of drivers/tty/serial/samsung_tty.c and include/linux/serial_s3c.h; the original is the source of truth.

//! The polled driver of the Apple S5L UART over [`Mmio`].

use crate::mmio::Mmio;
use crate::regs::*;

/// Which "transmitter ready" rule to apply in non-FIFO mode. In FIFO mode both rules are the same (`UFSTAT.TXFULL` clear).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxRule {
    /// The early console (`samsung_early_putc`, samsung_tty.c:2755-2764): FIFO mode tests `UFSTAT.TXFULL`
    /// (:2747-2753), otherwise `UTRSTAT.TXFE` (:2741-2745).
    Early,
    /// The runtime console and poll_put_char (`s3c24xx_serial_console_txrdy`, samsung_tty.c:2208-2225): FIFO mode tests
    /// `UFSTAT.TXFULL`, otherwise `UTRSTAT.TXE`.
    Console,
}

/// Pending interrupt causes read from `UTRSTAT` (`apple_serial_handle_irq`, samsung_tty.c:955-975).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Pending {
    /// `RXTHRESH | RXTO | RXTO_LEGACY` is set (samsung_tty.c:962-963).
    pub rx: bool,
    /// `TXTHRESH` is set (samsung_tty.c:969).
    pub tx: bool,
}

/// An Apple S5L UART.
pub struct AppleUart<M: Mmio> {
    pub(crate) mmio: M,
}

impl<M: Mmio> AppleUart<M> {
    pub fn new(mmio: M) -> Self {
        Self { mmio }
    }

    pub fn into_inner(self) -> M {
        self.mmio
    }

    pub fn mmio(&self) -> &M {
        &self.mmio
    }

    /// `UFCON.FIFOMODE` is set (samsung_tty.c:2757).
    pub fn fifo_mode(&self) -> bool {
        self.mmio.read32(UFCON) & UFCON_FIFOMODE != 0
    }

    /// The port is configured when `UCON & 0xf != 0` (`s3c24xx_port_configured`, samsung_tty.c:2228-2232). The Linux console
    /// and `poll_put_char` send nothing on an unconfigured port (samsung_tty.c:2259, :2288); the early console does
    /// not test it.
    pub fn configured(&self) -> bool {
        self.mmio.read32(UCON) & UCON_CONFIGURED_MASK != 0
    }

    /// Whether the transmitter can take a byte, under `rule`.
    pub fn tx_ready(&self, rule: TxRule) -> bool {
        if self.fifo_mode() {
            // samsung_tty.c:2216-2218 (console), :2747-2753 (early)
            self.mmio.read32(UFSTAT) & UFSTAT_TXFULL == 0
        } else {
            match rule {
                TxRule::Early => self.mmio.read32(UTRSTAT) & UTRSTAT_TXFE != 0, // samsung_tty.c:2743
                TxRule::Console => self.mmio.read32(UTRSTAT) & UTRSTAT_TXE != 0, // samsung_tty.c:2223-2224
            }
        }
    }

    /// Sends `b` if the transmitter is ready, under `rule`; `false` (nothing written) otherwise.
    pub fn try_send(&mut self, b: u8, rule: TxRule) -> bool {
        if !self.tx_ready(rule) {
            return false;
        }
        self.mmio.write32(UTXH, u32::from(b)); // samsung_tty.c:2763
        true
    }

    /// Waits for the transmitter and sends `b` (`samsung_early_putc`, samsung_tty.c:2755-2764). Spins forever if the
    /// transmitter never becomes ready, as the C does.
    pub fn send(&mut self, b: u8) {
        while !self.try_send(b, TxRule::Early) {
            core::hint::spin_loop();
        }
    }

    /// Bytes waiting in the receive FIFO (`s3c24xx_serial_rx_fifocnt`, samsung_tty.c:588-596): the FIFO size when
    /// `UFSTAT.RXFULL` is set, else the count field.
    pub fn rx_count(&self) -> u32 {
        let ufstat = self.mmio.read32(UFSTAT);
        if ufstat & UFSTAT_RXFULL != 0 {
            FIFO_SIZE
        } else {
            (ufstat & UFSTAT_RXMASK) >> UFSTAT_RXSHIFT
        }
    }

    /// Receives one byte if one is waiting (`s3c24xx_serial_get_poll_char`, samsung_tty.c:2240-2250). The early console's
    /// read tests `UFSTAT & (RXFULL | RXMASK)` (samsung_tty.c:2773-2792), which is the same condition.
    pub fn recv(&mut self) -> Option<u8> {
        if self.rx_count() == 0 {
            return None;
        }
        Some(self.mmio.read32(URXH) as u8) // the C returns `rd_reg(port, S3C2410_URXH)` as a char
    }

    /// Masks the four Apple interrupt enables in `UCON` and clears every Apple flag in `UTRSTAT`, as `apple_s5l_serial_shutdown`
    /// does (samsung_tty.c:1182-1195). The probe path clears three of the four enables (samsung_tty.c:1915-1924); clearing the
    /// fourth too is harmless.
    pub fn mask_all_irqs(&mut self) {
        let ucon = self.mmio.read32(UCON);
        self.mmio.write32(UCON, ucon & !UCON_IRQ_MASK);
        self.mmio.write32(UTRSTAT, UTRSTAT_ALL_FLAGS);
    }

    /// Clears every Apple flag in `UTRSTAT` (samsung_tty.c:1261).
    pub fn clear_flags(&mut self) {
        self.mmio.write32(UTRSTAT, UTRSTAT_ALL_FLAGS);
    }

    /// Enables the receive interrupts: threshold, timeout and legacy timeout (`apple_s5l_serial_startup`,
    /// samsung_tty.c:1286-1289).
    pub fn enable_rx_irqs(&mut self) {
        let ucon = self.mmio.read32(UCON);
        self.mmio.write32(
            UCON,
            ucon | UCON_RXTHRESH_ENA | UCON_RXTO_ENA | UCON_RXTO_LEGACY_ENA,
        );
    }

    /// Enables or disables the transmit threshold interrupt (samsung_tty.c:292-293, :403-405).
    pub fn set_tx_irq(&mut self, on: bool) {
        let ucon = self.mmio.read32(UCON);
        let new = if on {
            ucon | UCON_TXTHRESH_ENA
        } else {
            ucon & !UCON_TXTHRESH_ENA
        };
        self.mmio.write32(UCON, new);
    }

    /// Which interrupt causes are pending (samsung_tty.c:957-969).
    pub fn pending(&self) -> Pending {
        let st = self.mmio.read32(UTRSTAT);
        Pending {
            rx: st & (UTRSTAT_RXTHRESH | UTRSTAT_RXTO | UTRSTAT_RXTO_LEGACY) != 0,
            tx: st & UTRSTAT_TXTHRESH != 0,
        }
    }

    /// Acknowledges the receive causes by writing them to `UTRSTAT` (samsung_tty.c:964-966).
    pub fn ack_rx(&mut self) {
        self.mmio.write32(
            UTRSTAT,
            UTRSTAT_RXTHRESH | UTRSTAT_RXTO | UTRSTAT_RXTO_LEGACY,
        );
    }

    /// Acknowledges the transmit cause (samsung_tty.c:970).
    pub fn ack_tx(&mut self) {
        self.mmio.write32(UTRSTAT, UTRSTAT_TXTHRESH);
    }
}

impl<M: Mmio> core::fmt::Write for AppleUart<M> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for &b in s.as_bytes() {
            self.send(b);
        }
        Ok(())
    }
}

/// Whether a device-tree `compatible` string list names this UART (`apple,s5l-uart`, samsung_tty.c:2667, :2878). The list is
/// the property's NUL-separated strings, already split by the caller.
pub fn matches<'a>(mut compatibles: impl Iterator<Item = &'a str>) -> bool {
    compatibles.any(|c| c == COMPATIBLE)
}
