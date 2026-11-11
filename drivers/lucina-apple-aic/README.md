# lucina-apple-aic

<!-- SPDX-License-Identifier: GPL-2.0-or-later -->

The Apple Interrupt Controller (AIC v1, v2, v3) as pure logic for LucinaOS's Mac M4 port (stage 1): the `macbook-air-m4`
board, root compatible `apple,j713`, whose AIC node is `compatible = "apple,t8132-aic3", "apple,t8122-aic3"`
(`linux/arch/arm64/boot/dts/apple/t8132.dtsi:208-215`). `no_std`, `forbid(unsafe_code)`, no dependencies, host-tested. Nothing
here touches hardware: the kernel implements the register traits, the tests implement them over a software model.

Kernel integration: kernel patch 2031 (`ostd/src/arch/arm/irq/chip/aic.rs`) implements the traits over the hardware and
uses this crate through [`lucina-apple-aic-glue`](../../drivers/lucina-apple-aic-glue/README.md); both are in `kernel/inject.txt`
(`ostd/libs/`). The feature `sim` makes `src/sim.rs` public, for that crate's host tests; the kernel never enables it.

## Provenance

| | |
| --- | --- |
| Original | `drivers/irqchip/irq-apple-aic.c` (1136 lines), AsahiLinux/linux, branch `asahi`, rev `6ba0637d1ed9a2e3639dd3fc9eca6cd0483a6434`; local clone `/home/user/Projects/ReverseEngineering/asahi-m4/linux` |
| Also read | `include/dt-bindings/interrupt-controller/apple-aic.h` and `irq.h`, `arch/arm64/include/asm/apple_m1_pmu.h`, `include/clocksource/arm_arch_timer.h`, `arch/arm64/include/asm/{sysreg,cputype}.h`, `arch/arm64/tools/sysreg`, `include/linux/{irq,cpumask,bitops}.h`, `drivers/of/base.c` (match scoring), `kernel/irq/ipi-mux.c` (the IPI multiplexer, for `ipi_mux.rs`), all at the same rev via `git show HEAD:<path>` (the clone is a sparse checkout) |
| Authors | Copyright The Asahi Linux Contributors; based on irq-lpc32xx (Copyright 2015-2016 Vladimir Zapolskiy) and irq-bcm2836 (Copyright 2015 Broadcom). `ipi-mux.c`: The Asahi Linux Contributors, Copyright (c) 2022 Ventana Micro Systems Inc. |
| Licence of the original | GPL-2.0-or-later (SPDX line 1 of both C files); the DT binding header is GPL-2.0+ OR MIT |
| Licence of this crate | GPL-2.0-or-later, as derived behaviour; every source file carries the SPDX line and the attribution |
| Verdict | **adapt**: the behaviour of the C, re-expressed over Rust traits; no C structure (irq domains, static keys, per-CPU macros) kept |

This is a Rust port of the behaviour of `drivers/irqchip/irq-apple-aic.c` and `kernel/irq/ipi-mux.c` (both SPDX GPL-2.0-or-later); the original is the source of truth.
| Not used | m1n1 (its maintainers forbid AI tool use), Aurora's code (see below) |

Every constant and rule carries the `irq-apple-aic.c:<line>` (or header line) it comes from. Hardware numbers the C reads at run
time (`nr_irq`, `max_irq`, die counts) are inputs, never constants.

## What maps to what

| C | Rust |
| --- | --- |
| `struct aic_info`, `aic1*_info`, `aic2_info`, `aic3_info`, `aic_info_match` (:246-331), `IRQCHIP_DECLARE` (:1134-1136) | `info::{AicInfo, AIC*_INFO, AIC_INFO_MATCH, match_compatible, is_declared}` |
| geometry in `aic_of_ic_init` (:995-1049) | `info::{read_caps, derive_geometry, Geometry}` |
| `AIC_IRQ_HWIRQ`, `AIC_FIQ_HWIRQ`, `AIC_HWIRQ_IRQ`, `AIC_HWIRQ_DIE` (:210-216) | `hwirq::*` (Verus-checked) |
| `aic_irq_domain_translate` (:683-749), `aic_irq_get_fwspec_info` (:656-681), `aic_irq_domain_map` (:633-654), `aic_irq_set_type`, `aic_fiq_set_type` | `fwspec::{translate, to_cells, decode, fiq_affinity_index, chip_for, irq_set_type, fiq_set_type}` |
| `aic_irq_mask/unmask/eoi/set_affinity` (:369-463) | `Aic::{irq_mask, irq_unmask, irq_eoi, irq_set_affinity}` |
| `aic_fiq_*` and the `aic_fiq_unmasked` shadow (:349, :495-550) | `Aic::{fiq_set_mask, fiq_clear_mask, fiq_mask, fiq_unmask, fiq_eoi}`, `FiqShadow` |
| `aic_handle_irq` (:401-443), `aic_handle_fiq` (:557-613), `aic_handle_ipi` (:812-836) | `Aic::{handle_irq, handle_fiq, handle_ipi}` over `AicMmio`, `AicSysregs`, `AicHandler` |
| `aic_ipi_send_fast`, `aic_ipi_send_single` (:796-844), `aic_init_smp` (:846-857), `ipi-mux.c` | `Aic::ipi_send_single`, `ipi_mux::IpiMuxCpu` |
| `aic_init_cpu` (:859-920) | `Aic::init_cpu` |
| bulk mask/clear/route and `AIC2_CONFIG` enable (:1073-1090) | `Aic::init_hw` |

Left to the kernel side (they are Linux plumbing, not AIC behaviour): domain creation, `set_handle_irq/fiq`, the cpuhp hook, the
`affinities` DT walk (`build_fiq_affinity`, :928-964), the KVM `vgic_info` registration (:922-926, :1102-1117). The static keys
become `AicInfo::fast_ipi` / `local_fast_ipi`, fixed from the start.

## Verus

Verus-eligible (pure, bounded, bit-level): the hwirq pack/unpack round trip (`src/hwirq.rs`), and the offset derivation
(`info::derive_geometry`, `Aic::bit_reg`).

Done: `src/hwirq.rs` carries its proof in the repo's shipped-code style (`cfg_attr(verus_keep_ghost, verus_spec(...))`, as
`lucina-irqgate` and `lucina-ir3`). `verus/rules.rs` includes it by `#[path]`; `verus/mutants.sh` plants 7 bugs that Verus must
refuse. Run on buildhost from the repo root (2026-11-11, pinned Verus): `16 verified, 0 errors`, mutants `7 of 7 refused`.

```
verify-gate --mem 3G -- /home/user/tools/verus/verus-x86-linux/verus drivers/lucina-apple-aic/verus/rules.rs
verify-gate --mem 3G -- bash drivers/lucina-apple-aic/verus/mutants.sh
```

**Not yet a CI unit.** The CI discovers `verification/<unit>/`, and `lucina-ci inventory --check` refuses a unit whose input the
public snapshot leaves out, so registering it means adding this crate to `tools/ci/public-snapshot.rules`, which publishes a
GPL-2.0-or-later crate to the public repository through the automatic sync. That is a publication decision for the owner, so it
is left out. To promote it once approved: move `verus/` to `verification/mac-aic-hwirq/` (and its `#[path]` back to
`../../drivers/lucina-apple-aic/src/hwirq.rs`, the mutants' paths likewise), add `mac-aic-hwirq` to a group in
`tools/ci/proof-groups.tsv`, add the crate to `tools/ci/public-snapshot.rules`, and run `lucina-ci inventory --write`.

## Tests

`cargo test -p lucina-apple-aic`: geometry (M1's 896 IRQs from irq-apple-aic.c:14; v2/v3 formulas over synthetic register
values that claim nothing about a Mac), compatible matching (including the M4 list), hwirq round trips (exhaustive over
FIQs, sampled over IRQs, random words), translate and its inverse at EL1 and EL2, masking per die, eoi, affinity, the
event drain with held lines, the vGIC maintenance path, the FIQ poll order and its sources, the FIQ mask shadow, fast and
MMIO IPIs, the 32 virtual IPIs through the mux, `init_cpu` and `init_hw` write sequences.

`src/sim.rs` is the software AIC used by the tests. What it takes from the C is in its header; its own assumptions (masks start
clear, die 0 drained before die 1, an IRQ before the v1 "other" IPI, mask registers read back as the mask) only order test
events.

## Not determinable from the C alone

- The T8132 (M4) counts: `nr_irq`, `max_irq`, `nr_die`, `max_die` come from `AIC2_INFO1`/`AIC2_INFO3` at run time (:1014-1020);
  no value is in the C or the DT. Also the M2 and M3 shapes: the C states only the 896 IRQs of :14.
- Read semantics of SW_SET/SW_CLR/MASK_SET/MASK_CLR (the C only writes ones to them) and what HW_STATE holds (the C never reads it).
- Event ordering beyond "lower IRQs = higher priority" (:21): between dies, and between IRQs and v1 IPIs.
- That writing `IPI_SR_PENDING` clears `IPI_SR` (write-one-to-clear): inferred from "Ack the IPI" (:815-822), not stated.
- How the v1 hardware IPI gets masked during delivery: inferred from "Automatic masking on ack" (:23) and the `MASK_CLR` after
  processing (:834-835).
- Who programs `AIC2_IRQ_CFG` routing on v2/v3: its fields are declared (:160-161) and never written, and v2/v3 have no
  `set_affinity`.
- The meaning of every declared-but-unused register (kept in `regs.rs`, marked so).
- What the irq core does when the E-core and P-core PMU specifiers, with different affinities, both translate to the one PMU
  hwirq (:672-679 vs :738-740): that is in kernel/irq, not in this file.
- `aic_irq_set_affinity` with a mask that names no CPU computes `BIT(nr_cpu_ids)` (:454-459); the Rust returns `NoCpu`.

## Taken from Aurora, to confirm on the machine

Nothing. Aurora's AIC delta (`aurora-linux/drivers/irqchip/irq-apple-aic.c`) adds `apple,t8140-aic3` and `apple,t8142-aic3`
infos (lines 330-353 there) and leaves `t8122-aic3` as Asahi has it; it names no t8132-specific behaviour, so nothing was
needed from it for the M4.
