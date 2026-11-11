// SPDX-License-Identifier: GPL-2.0-only
// Derived from drivers/tty/serial/samsung_tty.c (SPDX GPL-2.0; Ben Dooks, Copyright (c) 2003-2008 Simtec Electronics) and
// include/linux/serial_s3c.h (SPDX GPL-2.0+; Copyright (C) 2002 Shane Nay, additional defines Copyright 2003 Simtec
// Electronics) of AsahiLinux/linux (branch asahi, rev 6ba0637d1ed9). Re-expressed in Rust for LucinaOS; see the crate README.
// This is a Rust port of the behaviour of drivers/tty/serial/samsung_tty.c and include/linux/serial_s3c.h; the original is the source of truth.
// Host tests of the S5L UART logic over the software register file in sim.rs.

use crate::regs::*;
use crate::sim::SimUart;
use crate::{AppleUart, TxRule, matches};
use core::fmt::Write;

fn uart() -> AppleUart<SimUart> {
    AppleUart::new(SimUart::new())
}

#[test]
fn register_offsets_are_those_of_serial_s3c_h() {
    assert_eq!(
        [
            ULCON, UCON, UFCON, UMCON, UTRSTAT, UERSTAT, UFSTAT, UMSTAT, UTXH, URXH, UBRDIV
        ],
        [
            0x00, 0x04, 0x08, 0x0C, 0x10, 0x14, 0x18, 0x1C, 0x20, 0x24, 0x28
        ]
    );
}

#[test]
fn apple_bits_are_those_of_serial_s3c_h() {
    // APPLE_S5L_UCON_* are bit numbers 9, 11, 12, 13 (serial_s3c.h:249-252).
    assert_eq!(UCON_RXTO_ENA, 0x200);
    assert_eq!(UCON_RXTO_LEGACY_ENA, 0x800);
    assert_eq!(UCON_RXTHRESH_ENA, 0x1000);
    assert_eq!(UCON_TXTHRESH_ENA, 0x2000);
    assert_eq!(UCON_IRQ_MASK, 0x3A00);
    // UTRSTAT: bits 3, 4, 5, 9 and GENMASK(9, 3) (serial_s3c.h:266-270).
    assert_eq!(UTRSTAT_RXTO_LEGACY, 8);
    assert_eq!(UTRSTAT_RXTHRESH, 0x10);
    assert_eq!(UTRSTAT_TXTHRESH, 0x20);
    assert_eq!(UTRSTAT_RXTO, 0x200);
    assert_eq!(
        UTRSTAT_ALL_FLAGS,
        (0..=9).filter(|b| *b >= 3).fold(0u32, |a, b| a | 1 << b)
    );
    // S3C2410 UFSTAT layout (serial_s3c.h:155-160).
    assert_eq!(UFSTAT_TXFULL, 0x200);
    assert_eq!(UFSTAT_RXFULL, 0x100);
    assert_eq!(UFSTAT_TXMASK, 0xF0);
    assert_eq!(UFSTAT_RXMASK, 0xF);
}

#[test]
fn compatible_match() {
    assert!(matches(["apple,s5l-uart"].into_iter()));
    assert!(matches(["foo,bar", "apple,s5l-uart"].into_iter()));
    assert!(!matches(["arm,pl011", "arm,primecell"].into_iter()));
    assert!(!matches(core::iter::empty()));
    assert!(!matches(["apple,s5l-uart-v2"].into_iter()));
}

#[test]
fn fifo_mode_tx_ready_follows_txfull() {
    let mut u = uart();
    u.mmio.write32_for_test(UFCON, UFCON_FIFOMODE);
    assert!(u.fifo_mode());
    for rule in [TxRule::Early, TxRule::Console] {
        assert!(u.tx_ready(rule));
    }
    for _ in 0..16 {
        assert!(u.try_send(b'x', TxRule::Early));
    }
    // Full: nothing more is accepted, nothing written.
    for rule in [TxRule::Early, TxRule::Console] {
        assert!(!u.tx_ready(rule));
    }
    let writes_before = u.mmio().writes.len();
    assert!(!u.try_send(b'y', TxRule::Early));
    assert_eq!(u.mmio().writes.len(), writes_before);
}

#[test]
fn non_fifo_early_rule_is_txfe_and_console_rule_is_txe() {
    let mut u = uart();
    assert!(!u.fifo_mode());
    assert!(u.tx_ready(TxRule::Early));
    assert!(u.tx_ready(TxRule::Console));
    assert!(u.try_send(b'a', TxRule::Early));
    // One byte in the buffer: neither TXFE nor TXE.
    assert!(!u.tx_ready(TxRule::Early));
    assert!(!u.tx_ready(TxRule::Console));
    u.mmio.drain(1);
    // Buffer empty but the shift register still busy: TXFE yes, TXE no.
    u.mmio.shifting = true;
    assert!(u.tx_ready(TxRule::Early));
    assert!(!u.tx_ready(TxRule::Console));
    u.mmio.shifting = false;
    assert!(u.tx_ready(TxRule::Console));
}

#[test]
fn send_writes_the_byte_to_utxh_only_when_ready() {
    let mut u = uart();
    u.send(b'L');
    assert_eq!(u.mmio().writes, [(UTXH, u32::from(b'L'))]);
    assert_eq!(
        u.mmio()
            .tx_fifo
            .iter()
            .copied()
            .collect::<std::vec::Vec<_>>(),
        b"L"
    );
}

#[test]
fn fmt_write_sends_every_byte_in_order() {
    let mut u = uart();
    u.mmio.ufcon = UFCON_FIFOMODE;
    writeln!(u, "hi {}", 42).unwrap();
    assert_eq!(
        u.mmio()
            .tx_fifo
            .iter()
            .copied()
            .collect::<std::vec::Vec<_>>(),
        b"hi 42\n"
    );
}

#[test]
fn configured_needs_a_nonzero_mode_field() {
    let mut u = uart();
    assert!(!u.configured());
    u.mmio.ucon = UCON_RXTHRESH_ENA; // an interrupt enable is not a mode
    assert!(!u.configured());
    u.mmio.ucon = 1;
    assert!(u.configured());
    u.mmio.ucon = 4;
    assert!(u.configured());
}

#[test]
fn recv_returns_none_on_an_empty_fifo_and_bytes_in_order() {
    let mut u = uart();
    assert_eq!(u.rx_count(), 0);
    assert_eq!(u.recv(), None);
    u.mmio.rx_fifo.borrow_mut().extend(*b"ab");
    assert_eq!(u.rx_count(), 2);
    assert_eq!(u.recv(), Some(b'a'));
    assert_eq!(u.recv(), Some(b'b'));
    assert_eq!(u.recv(), None);
}

#[test]
fn rx_count_of_a_full_fifo_is_the_fifo_size() {
    let mut u = uart();
    u.mmio.rx_fifo.borrow_mut().extend([7u8; 16]);
    // The model reports RXFULL with an all-zero count field, as the full flag exists to say.
    assert_eq!(u.mmio().read32_for_test(UFSTAT) & UFSTAT_RXMASK, 0);
    assert_eq!(u.rx_count(), FIFO_SIZE);
    assert_eq!(u.recv(), Some(7));
    assert_eq!(u.rx_count(), 15);
}

#[test]
fn mask_all_irqs_clears_only_the_four_enables_and_clears_flags() {
    let mut u = uart();
    // A configured port with all four enables and some unrelated bits.
    u.mmio.ucon = UCON_IRQ_MASK | 0x5 | (1 << 8);
    u.mmio.flags = UTRSTAT_ALL_FLAGS;
    u.mask_all_irqs();
    assert_eq!(u.mmio().ucon, 0x5 | (1 << 8));
    assert_eq!(u.mmio().flags, 0);
    assert!(u.mmio().writes.contains(&(UTRSTAT, UTRSTAT_ALL_FLAGS)));
}

#[test]
fn enable_rx_irqs_sets_exactly_three_bits() {
    let mut u = uart();
    u.mmio.ucon = 0x5;
    u.enable_rx_irqs();
    assert_eq!(
        u.mmio().ucon,
        0x5 | UCON_RXTHRESH_ENA | UCON_RXTO_ENA | UCON_RXTO_LEGACY_ENA
    );
    assert_eq!(u.mmio().ucon & UCON_TXTHRESH_ENA, 0);
}

#[test]
fn tx_irq_toggles_one_bit() {
    let mut u = uart();
    u.mmio.ucon = 0x5;
    u.set_tx_irq(true);
    assert_eq!(u.mmio().ucon, 0x5 | UCON_TXTHRESH_ENA);
    u.set_tx_irq(false);
    assert_eq!(u.mmio().ucon, 0x5);
}

#[test]
fn pending_and_ack_follow_the_apple_irq_handler() {
    let mut u = uart();
    assert_eq!(u.pending(), crate::Pending::default());
    for bit in [UTRSTAT_RXTHRESH, UTRSTAT_RXTO, UTRSTAT_RXTO_LEGACY] {
        u.mmio.flags = bit;
        let p = u.pending();
        assert!(p.rx && !p.tx);
        u.ack_rx();
        assert_eq!(u.mmio().flags, 0);
    }
    u.mmio.flags = UTRSTAT_TXTHRESH;
    let p = u.pending();
    assert!(!p.rx && p.tx);
    u.ack_tx();
    assert_eq!(u.mmio().flags, 0);
    // The acknowledge writes only the cause it names (samsung_tty.c:964-966, :970).
    u.mmio.flags = UTRSTAT_RXTHRESH | UTRSTAT_TXTHRESH;
    u.ack_tx();
    assert_eq!(u.mmio().flags, UTRSTAT_RXTHRESH);
}
