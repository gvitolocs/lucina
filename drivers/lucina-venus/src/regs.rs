// SPDX-License-Identifier: GPL-2.0-only
//! Register offsets of the video block, from the start of its window (`reg` of `video-codec@aa00000`, 0xff000 long), for Venus 4xx
//! (`hfi_venus_io.h` and `core.c` of Linux: the non-V6 bases). Only what the decode path uses.

/// `VBIF_BASE`.
pub const VBIF: u32 = 0x8_0000;
pub const VBIF_AXI_HALT_CTRL0: u32 = VBIF + 0x208;
pub const VBIF_AXI_HALT_CTRL1: u32 = VBIF + 0x20c;
pub const VBIF_AXI_HALT_CTRL0_HALT_REQ: u32 = 1 << 0;
pub const VBIF_AXI_HALT_CTRL1_HALT_ACK: u32 = 1 << 0;
/// `VBIF_AXI_HALT_ACK_TIMEOUT_US`.
pub const VBIF_AXI_HALT_ACK_TIMEOUT_US: u64 = 500_000;

/// `CPU_CS_BASE`: the control and status registers of the block's own processor.
pub const CPU_CS: u32 = 0xc_0000 + 0x1_2000;
pub const CPU_CS_A2HSOFTINTCLR: u32 = CPU_CS + 0x1c;
pub const VIDC_CTRL_INIT: u32 = CPU_CS + 0x48;
pub const VIDC_CTRL_INIT_CTRL: u32 = 1 << 0;
pub const CPU_CS_SCIACMDARG0: u32 = CPU_CS + 0x4c;
/// `CPU_CS_SCIACMDARG0_ERROR_STATUS_MASK`: 4 in these bits is "invalid setting for UC_REGION".
pub const SCIACMDARG0_ERROR_STATUS_MASK: u32 = 0xfe;
pub const SCIACMDARG0_INIT_STATUS: u32 = 1 << 0;
pub const SCIACMDARG0_PC_READY: u32 = 1 << 8;
pub const SCIACMDARG0_INIT_IDLE_MSG: u32 = 1 << 30;
pub const CPU_CS_SCIACMDARG1: u32 = CPU_CS + 0x50;
pub const CPU_CS_SCIACMDARG2: u32 = CPU_CS + 0x54;
pub const CPU_CS_SCIACMDARG3: u32 = CPU_CS + 0x58;
pub const SFR_ADDR: u32 = CPU_CS + 0x5c;
pub const UC_REGION_ADDR: u32 = CPU_CS + 0x64;
pub const UC_REGION_SIZE: u32 = CPU_CS + 0x68;

/// `CPU_IC_BASE`: the interrupt controller of the block's processor.
pub const CPU_IC: u32 = 0xc_0000 + 0x1_f000;
pub const CPU_IC_SOFTINT: u32 = CPU_IC + 0x18;
/// `BIT(CPU_IC_SOFTINT_H2A_SHIFT)`: host to processor.
pub const CPU_IC_SOFTINT_H2A: u32 = 1 << 15;

/// `WRAPPER_BASE`.
pub const WRAPPER: u32 = 0xe_0000;
pub const WRAPPER_HW_VERSION: u32 = WRAPPER;
pub const WRAPPER_INTR_STATUS: u32 = WRAPPER + 0x0c;
pub const WRAPPER_INTR_STATUS_A2HWD: u32 = 0x10;
pub const WRAPPER_INTR_STATUS_A2H: u32 = 0x4;
pub const WRAPPER_INTR_MASK: u32 = WRAPPER + 0x10;
/// `WRAPPER_INTR_MASK_A2HVCODEC_MASK`: what `venus_boot_core` writes on everything but IRIS2: the codec's interrupt masked, the
/// processor's and the watchdog's not.
pub const WRAPPER_INTR_MASK_A2HVCODEC: u32 = 0x8;
pub const WRAPPER_INTR_CLEAR: u32 = WRAPPER + 0x14;
pub const WRAPPER_POWER_STATUS: u32 = WRAPPER + 0x44;
pub const WRAPPER_CPU_CLOCK_CONFIG: u32 = WRAPPER + 0x2000;
pub const WRAPPER_CPU_AXI_HALT: u32 = WRAPPER + 0x2008;
pub const WRAPPER_CPU_AXI_HALT_STATUS: u32 = WRAPPER + 0x200c;
pub const WRAPPER_CPU_CGC_DIS: u32 = WRAPPER + 0x2010;
pub const WRAPPER_CPU_STATUS: u32 = WRAPPER + 0x2014;
pub const WRAPPER_CPU_STATUS_WFI: u32 = 1 << 0;
pub const WRAPPER_VCODEC0_MMCC_POWER_STATUS: u32 = WRAPPER + 0x90;
pub const WRAPPER_VCODEC0_MMCC_POWER_CONTROL: u32 = WRAPPER + 0x94;
