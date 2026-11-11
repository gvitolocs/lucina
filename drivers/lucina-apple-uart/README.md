# lucina-apple-uart

<!-- SPDX-License-Identifier: GPL-2.0-only -->

The Apple S5L UART (`apple,s5l-uart`) as polled logic for LucinaOS's Mac M4 port (stage 1): the `macbook-air-m4` board,
root compatible `apple,j713`, whose console is `serial0 = serial@3ad200000` (`reg-io-width = <4>`,
`linux/arch/arm64/boot/dts/apple/t8132.dtsi:442-453`; `stdout-path = "serial0"`, `t8132-jxxx.dtsi:14`, `:22`). `no_std`,
`forbid(unsafe_code)`, no dependencies, host-tested. Nothing here touches hardware: the kernel implements the one register
trait ([`Mmio`], `ostd/src/arch/arm/serial.rs`, patch 2020), the tests implement it over a software register file.

## Provenance

| | |
| --- | --- |
| Original | `drivers/tty/serial/samsung_tty.c` (the `TYPE_APPLE_S5L` paths) and `include/linux/serial_s3c.h`, AsahiLinux/linux, branch `asahi`, rev `6ba0637d1ed9a2e3639dd3fc9eca6cd0483a6434`; local clone `/home/user/Projects/ReverseEngineering/asahi-m4/linux` (sparse: read with `git show HEAD:<path>`) |
| Also read | `arch/arm64/boot/dts/apple/t8132.dtsi`, `t8132-jxxx.dtsi`; m1n1 `src/uart_regs.h`, `src/uart.c`, `src/start.S` (rev `cf37927d6bf4`) as a behaviour reference for what the loader leaves the UART doing; nothing copied |
| Authors | `samsung_tty.c`: Ben Dooks, Copyright (c) 2003-2008 Simtec Electronics. `serial_s3c.h`: Copyright (C) 2002 Shane Nay; additional defines Copyright 2003 Simtec Electronics |
| Licence of the original | `samsung_tty.c`: **GPL-2.0** (SPDX line 1, `MODULE_LICENSE("GPL v2")`: version 2 only). `serial_s3c.h`: GPL-2.0+ |
| Licence of this crate | **GPL-2.0-only**, because the behaviour comes from `samsung_tty.c`, which is version 2 only. The brief for this crate said GPL-2.0-or-later; the file's own header does not allow that, so this follows the header. Every source file carries the SPDX line and the attribution |
| Verdict | **adapt**: the polled paths and the S5L bits re-expressed over a Rust trait; no C structure (uart_port, clk, DMA, termios) kept |

This is a Rust port of the behaviour of `drivers/tty/serial/samsung_tty.c` and `include/linux/serial_s3c.h`; the original is the source of truth.

Every constant and rule cites `samsung_tty.c:<line>` or `serial_s3c.h:<line>` at the pinned revision.

## What maps to what

| C | Rust |
| --- | --- |
| register offsets (`serial_s3c.h:19-29`) | `regs::{ULCON, UCON, UFCON, UMCON, UTRSTAT, UERSTAT, UFSTAT, UMSTAT, UTXH, URXH, UBRDIV}` |
| `s5l_serial_drv_data` (`samsung_tty.c:2564-2587`): UFSTAT layout is S3C2410's, FIFO 16, MMIO32 | `regs::{UFSTAT_*, FIFO_SIZE, REG_WIDTH_BYTES}` |
| `APPLE_S5L_UCON_*`, `APPLE_S5L_UTRSTAT_*` (`serial_s3c.h:249-270`) | `regs::{UCON_*, UTRSTAT_*}` |
| `samsung_early_putc`, `samsung_early_busyuart(_fifo)` (`samsung_tty.c:2741-2764`) | `AppleUart::{tx_ready(TxRule::Early), try_send, send}` |
| `s3c24xx_serial_console_txrdy` (`samsung_tty.c:2208-2225`) | `AppleUart::tx_ready(TxRule::Console)` |
| `s3c24xx_serial_rx_fifocnt`, `s3c24xx_serial_get_poll_char`, `samsung_early_read` | `AppleUart::{rx_count, recv}` |
| `s3c24xx_port_configured` (`samsung_tty.c:2228-2232`) | `AppleUart::configured` |
| `apple_s5l_serial_startup/shutdown`, `apple_serial_handle_irq` (`:955-975`, `:1182-1196`, `:1254-1295`) | `AppleUart::{mask_all_irqs, clear_flags, enable_rx_irqs, set_tx_irq, pending, ack_rx, ack_tx}` |
| `OF_EARLYCON_DECLARE(s5l, "apple,s5l-uart", ...)` (`:2878`) | `uart::matches`, `regs::COMPATIBLE` |

## What is not settled from the originals

- Whether the loader (iBoot/m1n1) leaves the M4's UART in FIFO mode (`UFCON` bit 0). m1n1's own early putc waits for `UTRSTAT`
  bit 1 only (`m1n1/src/start.S:6-7, 234-237`), whatever the mode; Linux's early console picks its test from `UFCON`. This crate follows
  Linux. If the M4's UART is in FIFO mode with `UFSTAT.TXFULL` not meaningful, transmit could stall; only hardware tells.
- The baud rate and clock state the loader leaves: not read, never written here (`UBRDIV` is not touched).
- Nothing about `UCON`/`UFCON` initial values on the M4.
