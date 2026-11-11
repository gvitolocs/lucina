# lucina-apple-boot

<!-- SPDX-License-Identifier: GPL-2.0-only -->

What the Mac M4 board (Juno on `macbook-air-m4`) takes from the device tree that m1n1 hands over, as pure logic: the
`simple-framebuffer` node under `/chosen` (`simplefb`) and the spin-table CPU release (`spin_table`). `no_std`, unsafe-free,
host-tested. See the module documentation in `src/` for the rules and the lines they come from.

## Provenance

| | |
| --- | --- |
| Originals (Linux) | `arch/arm64/kernel/smp_spin_table.c` and `Documentation/devicetree/bindings/display/simple-framebuffer.yaml`, AsahiLinux/linux, branch `asahi`, rev `6ba0637d1ed9a2e3639dd3fc9eca6cd0483a6434`; local clone `/home/user/Projects/ReverseEngineering/asahi-m4/linux` (read with `git show HEAD:<path>`) |
| Also read | `arch/arm64/boot/dts/apple/t8132.dtsi` and `t8132-jxxx.dtsi` (SPDX GPL-2.0+ OR MIT) for the M4 nodes. m1n1 `src/kboot.c` and `src/smp.c`, rev `cf37927d6bf4`, as a behaviour reference for what the loader writes and how a parked CPU waits (m1n1 is MIT up to v1.9.9 and GPL-2.0-or-later from v2.0.0, per its README; the files carry `GPL-2.0-or-later`). Nothing copied. |
| Authors | `smp_spin_table.c`: Copyright (C) 2013 ARM Ltd. `simple-framebuffer.yaml`: maintainer Hans de Goede. The m1n1 files carry no copyright line. |
| Licences of the originals | `smp_spin_table.c` GPL-2.0-only; `simple-framebuffer.yaml` GPL-2.0 (version 2 only); m1n1 files GPL-2.0-or-later (SPDX line 1) |
| Licence of this crate | **GPL-2.0-only**, as derived behaviour from the Linux files, whose licence is version 2 only. Every source file carries the SPDX line and the attribution |
| Verdict | **adapt**: the behaviour re-expressed in Rust over traits; no C structure kept |

This is a Rust port of the behaviour of `arch/arm64/kernel/smp_spin_table.c`, the simple-framebuffer binding and m1n1's `kboot.c`/`smp.c` behaviour; the original is the source of truth.
