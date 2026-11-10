//! i.MX UART console (UART1 at 0x0202_0000 on both the i.MX6SL and the i.MX6Q QEMU models), polled.
//!
//! Register reference: Linux `drivers/tty/serial/imx.c`. The previous kernel or bootloader already set the baud
//! rate; this only makes sure the transmitter is on.

use super::Mmio;
use core::fmt;

const UART1: Mmio = Mmio::at(0x0202_0000);

const UTXD: usize = 0x40;
const UCR1: usize = 0x80;
const UCR2: usize = 0x84;
const UTS: usize = 0xb4;

const UCR1_UARTEN: u32 = 1 << 0;
const UCR2_SRST: u32 = 1 << 0; // active low: 1 = not in reset
const UCR2_RXEN: u32 = 1 << 1;
const UCR2_TXEN: u32 = 1 << 2;
const UCR2_WS: u32 = 1 << 5; // 8 data bits
const UCR2_IRTS: u32 = 1 << 14;
const UTS_TXFULL: u32 = 1 << 4;

pub fn init() {
    if UART1.read(UCR1) & UCR1_UARTEN == 0 {
        UART1.modify(UCR1, 0, UCR1_UARTEN);
    }
    let need = UCR2_SRST | UCR2_RXEN | UCR2_TXEN;
    if UART1.read(UCR2) & need != need {
        UART1.modify(UCR2, 0, need | UCR2_WS | UCR2_IRTS);
    }
}

pub fn putc(c: u8) {
    let mut spins = 0u32;
    while UART1.read(UTS) & UTS_TXFULL != 0 {
        spins += 1;
        if spins > 1_000_000 {
            return; // no clock or no transmitter: drop the byte rather than hang
        }
    }
    UART1.write(UTXD, c as u32);
}

pub struct Console;

impl fmt::Write for Console {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            if b == b'\n' {
                putc(b'\r');
            }
            putc(b);
        }
        Ok(())
    }
}

#[macro_export]
macro_rules! println {
    () => { $crate::print!("\n") };
    ($($arg:tt)*) => {{
        $crate::print!($($arg)*);
        $crate::print!("\n");
    }};
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {{
        use core::fmt::Write as _;
        let _ = write!($crate::arch::serial::Console, $($arg)*);
    }};
}
