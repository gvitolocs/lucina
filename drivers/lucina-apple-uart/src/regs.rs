// SPDX-License-Identifier: GPL-2.0-only
// Derived from drivers/tty/serial/samsung_tty.c (SPDX GPL-2.0; Ben Dooks, Copyright (c) 2003-2008 Simtec Electronics) and
// include/linux/serial_s3c.h (SPDX GPL-2.0+; Copyright (C) 2002 Shane Nay, additional defines Copyright 2003 Simtec
// Electronics) of AsahiLinux/linux (branch asahi, rev 6ba0637d1ed9). Re-expressed in Rust for LucinaOS; see the crate README.
// This is a Rust port of the behaviour of drivers/tty/serial/samsung_tty.c and include/linux/serial_s3c.h; the original is the source of truth.

//! Register offsets, bits and masks of the Apple S5L UART (Linux `TYPE_APPLE_S5L`, DT `apple,s5l-uart`).
//!
//! Every line cites `serial_s3c.h:<line>` (S3C) or `samsung_tty.c:<line>` (TTY) at rev 6ba0637d1ed9.

/// `apple,s5l-uart`: the compatible the driver matches (samsung_tty.c:2667, early console samsung_tty.c:2878).
pub const COMPATIBLE: &str = "apple,s5l-uart";

/// Registers are 32 bits wide and must be accessed as 32-bit MMIO ("Apple A7-A11 requires MMIO32 register accesses",
/// samsung_tty.c:2864-2865; `.iotype = UPIO_MEM32`, samsung_tty.c:2569).
pub const REG_WIDTH_BYTES: usize = 4;

/// ULCON, line control (serial_s3c.h:21).
pub const ULCON: usize = 0x00;
/// UCON, control (serial_s3c.h:22).
pub const UCON: usize = 0x04;
/// UFCON, FIFO control (serial_s3c.h:23).
pub const UFCON: usize = 0x08;
/// UMCON, modem control (serial_s3c.h:24).
pub const UMCON: usize = 0x0C;
/// UTRSTAT, transmit/receive status (serial_s3c.h:26).
pub const UTRSTAT: usize = 0x10;
/// UERSTAT, error status (serial_s3c.h:27).
pub const UERSTAT: usize = 0x14;
/// UFSTAT, FIFO status (serial_s3c.h:28).
pub const UFSTAT: usize = 0x18;
/// UMSTAT, modem status (serial_s3c.h:29).
pub const UMSTAT: usize = 0x1C;
/// UTXH, transmit holding register (serial_s3c.h:20).
pub const UTXH: usize = 0x20;
/// URXH, receive holding register (serial_s3c.h:19).
pub const URXH: usize = 0x24;
/// UBRDIV, baud rate divisor (serial_s3c.h:25). The driver never writes it for an early console; the boot loader's setting
/// stays.
pub const UBRDIV: usize = 0x28;

/// FIFO size of the S5L UART: `.fifosize = 16` (samsung_tty.c:2570).
pub const FIFO_SIZE: u32 = 16;

// UFCON
/// FIFO enable (`S3C2410_UFCON_FIFOMODE`, serial_s3c.h:119). The early console picks its transmit test from it
/// (samsung_tty.c:2757-2760).
pub const UFCON_FIFOMODE: u32 = 1 << 0;
/// Reset the receive FIFO (`S3C2410_UFCON_RESETRX`, serial_s3c.h:137).
pub const UFCON_RESETRX: u32 = 1 << 1;
/// Reset the transmit FIFO (`S3C2410_UFCON_RESETTX`, serial_s3c.h:136).
pub const UFCON_RESETTX: u32 = 1 << 2;

// UFSTAT: the S5L takes the S3C2410 layout (`.rx_fifomask = S3C2410_UFSTAT_RXMASK` ..., samsung_tty.c:2571-2576).
/// Transmit FIFO full (`S3C2410_UFSTAT_TXFULL`, serial_s3c.h:155).
pub const UFSTAT_TXFULL: u32 = 1 << 9;
/// Receive FIFO full (`S3C2410_UFSTAT_RXFULL`, serial_s3c.h:156).
pub const UFSTAT_RXFULL: u32 = 1 << 8;
/// Transmit FIFO count mask (`S3C2410_UFSTAT_TXMASK` = 15 << 4, serial_s3c.h:157).
pub const UFSTAT_TXMASK: u32 = 15 << 4;
/// Transmit FIFO count shift (`S3C2410_UFSTAT_TXSHIFT`, serial_s3c.h:158).
pub const UFSTAT_TXSHIFT: u32 = 4;
/// Receive FIFO count mask (`S3C2410_UFSTAT_RXMASK` = 15, serial_s3c.h:159).
pub const UFSTAT_RXMASK: u32 = 15;
/// Receive FIFO count shift (`S3C2410_UFSTAT_RXSHIFT`, serial_s3c.h:160).
pub const UFSTAT_RXSHIFT: u32 = 0;

// UTRSTAT, the S3C2410 bits (serial_s3c.h:170-173) ...
/// Receive buffer data ready (`S3C2410_UTRSTAT_RXDR`, serial_s3c.h:173).
pub const UTRSTAT_RXDR: u32 = 1 << 0;
/// Transmit buffer (FIFO) empty (`S3C2410_UTRSTAT_TXFE`, serial_s3c.h:172). The early console's non-FIFO transmit test
/// (samsung_tty.c:2743).
pub const UTRSTAT_TXFE: u32 = 1 << 1;
/// Transmitter empty, shift register too (`S3C2410_UTRSTAT_TXE`, serial_s3c.h:171). The runtime console's non-FIFO transmit
/// test (samsung_tty.c:2224).
pub const UTRSTAT_TXE: u32 = 1 << 2;
// ... and the Apple ones, which are interrupt flags (serial_s3c.h:266-270).
/// Legacy receive timeout (`APPLE_S5L_UTRSTAT_RXTO_LEGACY`, serial_s3c.h:266).
pub const UTRSTAT_RXTO_LEGACY: u32 = 1 << 3;
/// Receive threshold reached (`APPLE_S5L_UTRSTAT_RXTHRESH`, serial_s3c.h:267).
pub const UTRSTAT_RXTHRESH: u32 = 1 << 4;
/// Transmit threshold reached (`APPLE_S5L_UTRSTAT_TXTHRESH`, serial_s3c.h:268).
pub const UTRSTAT_TXTHRESH: u32 = 1 << 5;
/// Receive timeout (`APPLE_S5L_UTRSTAT_RXTO`, serial_s3c.h:269).
pub const UTRSTAT_RXTO: u32 = 1 << 9;
/// All Apple flags, `GENMASK(9, 3)` (`APPLE_S5L_UTRSTAT_ALL_FLAGS`, serial_s3c.h:270): bits 3 to 9 inclusive. Written back to
/// UTRSTAT to clear them (samsung_tty.c:1195, :1261, :1924).
pub const UTRSTAT_ALL_FLAGS: u32 = 0x3F8;

// UCON
/// Receive mode field (`S3C64XX_UCON_RXMODE_MASK`, serial_s3c.h:117).
pub const UCON_RXMODE_MASK: u32 = 3;
/// Transmit mode field (`S3C64XX_UCON_TXMODE_MASK`, serial_s3c.h:114).
pub const UCON_TXMODE_MASK: u32 = 3 << 2;
/// Receive timeout enable (`APPLE_S5L_UCON_RXTO_ENA_MSK`, bit 9; serial_s3c.h:249, :253).
pub const UCON_RXTO_ENA: u32 = 1 << 9;
/// Legacy receive timeout enable (`APPLE_S5L_UCON_RXTO_LEGACY_ENA_MSK`, bit 11; serial_s3c.h:250, :254).
pub const UCON_RXTO_LEGACY_ENA: u32 = 1 << 11;
/// Receive threshold interrupt enable (`APPLE_S5L_UCON_RXTHRESH_ENA_MSK`, bit 12; serial_s3c.h:251, :255).
pub const UCON_RXTHRESH_ENA: u32 = 1 << 12;
/// Transmit threshold interrupt enable (`APPLE_S5L_UCON_TXTHRESH_ENA_MSK`, bit 13; serial_s3c.h:252, :256).
pub const UCON_TXTHRESH_ENA: u32 = 1 << 13;
/// The four Apple interrupt-enable bits (`APPLE_S5L_UCON_MASK`, serial_s3c.h:261-264).
pub const UCON_IRQ_MASK: u32 =
    UCON_RXTO_ENA | UCON_RXTO_LEGACY_ENA | UCON_RXTHRESH_ENA | UCON_TXTHRESH_ENA;
/// "Configured" test: the transmit or receive mode field is non-zero (`s3c24xx_port_configured`, samsung_tty.c:2228-2232).
pub const UCON_CONFIGURED_MASK: u32 = 0xF;
