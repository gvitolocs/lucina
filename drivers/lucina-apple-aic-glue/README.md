# lucina-apple-aic-glue

<!-- SPDX-License-Identifier: GPL-2.0-only -->

The kernel side of the Apple Interrupt Controller for LucinaOS's Mac M4 port (stage 1, item c): what `ostd`'s AIC irq chip
(kernel patch 2031, `ostd/src/arch/arm/irq/chip/aic.rs`) does around [`lucina-apple-aic`](../../drivers/lucina-apple-aic/README.md)
that is not Linux's irq-domain plumbing but still decides behaviour. `no_std` with `alloc`, `forbid(unsafe_code)`,
host-tested over that crate's software AIC (its `sim` feature). The kernel patch holds the MMIO, the system-register `asm`
and the `unsafe`; this crate holds none.

## What it does

| Item | Behaviour, and where it comes from |
| --- | --- |
| `windows` | The core window (`reg` 0) and the event window: in the core window on v1 (irq-apple-aic.c:1007), `reg` 1 on v2/v3 (:1025), as a slice of the core mapping when it lies inside it (the M4: `0x3_81040000 + 0x4000` inside `0x3_81000000 + 0x1cc000`, t8132.dtsi:212-213). |
| `FiqAffinity` | `build_fiq_affinity` (irq-apple-aic.c:928-964): index missing, out of range or already set: ignored; no `cpus`: ignored; a phandle that is no CPU: skipped. Kept as MPIDR affinities, which is what `of_cpu_node_to_id` (:952) resolves the CPU nodes by. |
| `timer_interrupt_index` | The kernel's timer is the EL1 virtual timer: the interrupt named `"virt"` when the node has `interrupt-names`, else the third (arm_arch_timer.c:1174-1180 with :46-51; `ARCH_TIMER_VIRT_PPI` = 2, include/clocksource/arm_arch_timer.h:34-41). On the M4 the third would be the hypervisor timer (t8132.dtsi:458-462). |
| `hyp_timer_interrupt_index` | The same for a kernel at EL2 as a VHE host (LucinaOS patch 2040), whose `cntv_*_el0` is the EL2 virtual timer: `"hyp-virt"` by name, else the fifth (`ARCH_TIMER_HYP_VIRT_PPI` = 4), as `arch_timer_select_ppi` picks in hyp mode (arm_arch_timer.c:1136-1145). None when absent; Linux's fallback to the EL2 physical timer (:1143-1144) is not taken. |
| `LineTable` | hwirq to kernel IRQ line, per FIQ index and per IRQ of each die, plus the core's per-IRQ mask (`irqd_irq_masked`). |
| `map` | `translate`, the chip's trigger check (`aic_irq_set_type` / `aic_fiq_set_type`), then unmasked: an IRQ at the AIC, a FIQ on this CPU if its affinity includes it (`irq_set_percpu_devid_partition`, :647-648). |
| `unmap`, `set_irq_enabled` | Mask, forget; `disable_irq`/`enable_irq` of an IRQ. |
| `complete` | The flow handlers' eoi: `aic_irq_eoi` unless the core masked it (:391-399), `aic_fiq_eoi` (:545-550). |
| `cpu_start` | `aic_init_cpu` (:859-920) as the cpuhp starting state runs it on every CPU (:1094-1096), then the CPU's `enable_percpu_irq` of the FIQs mapped so far. |
| `Dispatch` | The `AicHandler` of the entry points: a FIQ is acked (`aic_fiq_set_mask`, the FIQ chip's `irq_ack`, :624) before delivery, an IRQ is delivered directly (`handle_fasteoi_irq`); an unmapped hwirq is counted and left as it is (an IRQ stays masked by its event read). Virtual IPIs are not used yet: the mux is counted. |

## Provenance

| | |
| --- | --- |
| Originals | `drivers/irqchip/irq-apple-aic.c` and `drivers/clocksource/arm_arch_timer.c`, AsahiLinux/linux, branch `asahi`, rev `6ba0637d1ed9a2e3639dd3fc9eca6cd0483a6434` (local clone `/home/user/Projects/ReverseEngineering/asahi-m4/linux`, read with `git show HEAD:<path>`); `arch/arm64/boot/dts/apple/t8132.dtsi` for the M4's nodes |
| Licences of the originals | irq-apple-aic.c GPL-2.0-or-later, arm_arch_timer.c GPL-2.0-only (SPDX line 1 of each) |
| Licence of this crate | GPL-2.0-only, as derived behaviour from both |
| Verdict | **adapt**: behaviour re-expressed in Rust; no C structure kept |

This is a Rust port of the behaviour of `drivers/irqchip/irq-apple-aic.c` and `drivers/clocksource/arm_arch_timer.c`; the original is the source of truth.
| Also read, reference only | m1n1 (owner decision of 2026-11-11, docs/research/mac-m4/README.md) for the hand-off state; XNU `xnu-12377.121.6` (APSL 2.0) for the timer FIQ handling. Nothing copied from either. |

## Tests

`cargo test -p lucina-apple-aic-glue`: the M4's windows and the other shapes, the affinity walk's edge cases, the timer's
interrupt by name and by position, the line table's bounds, map refusals (EL1 hypervisor timer, trigger types, taken), the
EL1 virtual timer FIQ from map to delivery to eoi (no system register written), the EL2 guest timer masked for the
handler and unmasked by the eoi, unmapped FIQs and IRQs, a fast IPI acknowledged, an IRQ masked by its event read and
unmasked by the eoi unless disabled or unmapped, per-CPU start within the affinity, a v1 WHOAMI report. The geometry numbers
are synthetic, as in `lucina-apple-aic`'s tests.

## Verus

Candidates for the prover (not done here; the builder does not write statements or proofs, docs/ci/PROOF-ROLES.md):
`windows` (the slice lies inside the core window), `LineTable`'s slot bounds (every slot index is below the table's
length), `timer_interrupt_index` and `hyp_timer_interrupt_index` (the index is below `count`).
