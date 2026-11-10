// SPDX-License-Identifier: GPL-2.0-only
//! Register offsets. GPU and GMU offsets are dwords from `gpu@5000000`, as in Linux's `a6xx.xml` and `a6xx_gmu.xml`; on
//! this SoC the GMU's window starts at dword 0x1a800 of the same range. Everything but the PDC lives in one window,
//! [`WINDOW_BASE`], because the GMU's window also covers GPU_CC.

/// 0x5000000: the GPU, the GPU's SMMU (+0x40000), the GMU (+0x6a000), RSCC (+0x8d000) and GPU_CC (+0x90000).
pub const WINDOW_BASE: usize = 0x500_0000;
pub const WINDOW_SIZE: usize = 0xa_0000;
/// Byte offset of the GPU's SMMU in the window.
pub const SMMU_OFFSET: u32 = 0x4_0000;
pub const SMMU_SIZE: u32 = 0x1_0000;
/// Byte offset of RSCC in the window: `gmu->rscc = gmu->mmio + 0x23000`.
pub const RSCC_OFFSET: u32 = 0x8_d000;
/// Byte offset of GPU_CC in the window.
pub const GPU_CC_OFFSET: u32 = 0x9_0000;

// GMU, dwords from the GPU's base.
pub const GMU_GX_SPTPRAC_POWER_CONTROL: u32 = 0x1a881;
pub const GMU_CM3_ITCM_START: u32 = 0x1b400;
pub const GMU_CM3_DTCM_START: u32 = 0x1c400;
pub const GMU_BOOT_SLUMBER_OPTION: u32 = 0x1cbf8;
pub const GMU_GX_VOTE_IDX: u32 = 0x1cbf9;
pub const GMU_MX_VOTE_IDX: u32 = 0x1cbfa;
pub const GMU_DCVS_ACK_OPTION: u32 = 0x1cbfc;
pub const GMU_DCVS_PERF_SETTING: u32 = 0x1cbfd;
pub const GMU_DCVS_BW_SETTING: u32 = 0x1cbfe;
pub const GMU_DCVS_RETURN: u32 = 0x1cbff;
pub const GMU_ICACHE_CONFIG: u32 = 0x1f400;
pub const GMU_DCACHE_CONFIG: u32 = 0x1f401;
pub const GMU_SYS_BUS_CONFIG: u32 = 0x1f40f;
pub const GMU_CM3_SYSRESET: u32 = 0x1f800;
pub const GMU_CM3_BOOT_CONFIG: u32 = 0x1f801;
pub const GMU_CM3_FW_INIT_RESULT: u32 = 0x1f81c;
pub const GMU_CM3_CFG: u32 = 0x1f82d;
pub const GMU_POWER_COUNTER_SELECT_0: u32 = 0x1f841;
pub const GMU_POWER_COUNTER_ENABLE: u32 = 0x1f840;
pub const GMU_PWR_COL_INTER_FRAME_CTRL: u32 = 0x1f8c0;
pub const GMU_PWR_COL_KEEPALIVE: u32 = 0x1f8c3;
pub const GMU_SPTPRAC_PWR_CLK_STATUS: u32 = 0x1f8d0;
pub const GMU_RPMH_CTRL: u32 = 0x1f8e8;
/// `GPU_GMU_CX_GMU_RPMH_POWER_STATE`: 0 active, 0xf slumber.
pub const GMU_RPMH_POWER_STATE: u32 = 0x1f8ec;
pub const GMU_PWR_COL_CP_MSG: u32 = 0x1f900;
pub const GMU_PWR_COL_CP_RESP: u32 = 0x1f901;
pub const GMU_HFI_CTRL_STATUS: u32 = 0x1f980;
pub const GMU_HFI_SFR_ADDR: u32 = 0x1f982;
pub const GMU_HFI_QTBL_INFO: u32 = 0x1f984;
pub const GMU_HFI_QTBL_ADDR: u32 = 0x1f985;
pub const GMU_HFI_CTRL_INIT: u32 = 0x1f986;
pub const GMU_GMU2HOST_INTR_CLR: u32 = 0x1f991;
pub const GMU_GMU2HOST_INTR_INFO: u32 = 0x1f992;
pub const GMU_GMU2HOST_INTR_MASK: u32 = 0x1f993;
pub const GMU_HOST2GMU_INTR_SET: u32 = 0x1f994;
pub const GMU_GENERAL_7: u32 = 0x1f9cc;
pub const GMU_AO_HOST_INTERRUPT_STATUS: u32 = 0x23b05;
pub const GMU_AO_HOST_INTERRUPT_CLR: u32 = 0x23b04;
pub const GMU_AO_HOST_INTERRUPT_MASK: u32 = 0x23b06;
pub const GMU_RSCC_CONTROL_REQ: u32 = 0x23b07;
pub const GMU_RSCC_CONTROL_ACK: u32 = 0x23b08;
pub const GMU_AO_GPU_CX_BUSY_STATUS: u32 = 0x23b0c;
pub const GMU_AO_GPU_CX_BUSY_STATUS2: u32 = 0x23b0d;
pub const GMU_AO_GPU_CX_BUSY_MASK: u32 = 0x23b0e;
pub const GMU_AO_AHB_FENCE_CTRL: u32 = 0x23b10;
pub const GMU_AHB_FENCE_RANGE_0: u32 = 0x23b11;
pub const GMU_AHB_FENCE_STATUS: u32 = 0x23b13;
pub const GMU_AHB_FENCE_STATUS_CLR: u32 = 0x23b14;
/// GPU_CC's `gx_gdscr` as the GMU's register map names it (`REG_A6XX_GPU_CC_GX_GDSCR`).
pub const GPU_CC_GX_GDSCR: u32 = 0x24403;

// RSCC, dwords from its own start.
pub const RSCC_RSC_STATUS0_DRV0: u32 = 0x004;
pub const RSCC_PDC_SEQ_START_ADDR: u32 = 0x008;
pub const RSCC_PDC_MATCH_VALUE_LO: u32 = 0x009;
pub const RSCC_PDC_MATCH_VALUE_HI: u32 = 0x00a;
pub const RSCC_PDC_SLAVE_ID_DRV0: u32 = 0x00b;
pub const RSCC_HIDDEN_TCS_CMD0_ADDR: u32 = 0x00d;
pub const RSCC_HIDDEN_TCS_CMD0_DATA: u32 = 0x00e;
pub const RSCC_OVERRIDE_START_ADDR: u32 = 0x100;
pub const RSCC_SEQ_BUSY_DRV0: u32 = 0x101;
pub const RSCC_SEQ_MEM_0_DRV0: u32 = 0x180;
pub const RSCC_TCS0_DRV0_STATUS: u32 = 0x346;
pub const RSCC_TCS1_DRV0_STATUS: u32 = 0x3ee;
pub const RSCC_TCS2_DRV0_STATUS: u32 = 0x496;
pub const RSCC_TCS3_DRV0_STATUS: u32 = 0x53e;

// PDC, dwords in its own windows.
pub const PDC_GPU_SEQ_MEM_0: u32 = 0x0000;
pub const PDC_GPU_ENABLE_PDC: u32 = 0x1140;
pub const PDC_GPU_SEQ_START_ADDR: u32 = 0x1148;
pub const PDC_GPU_TCS1_CONTROL: u32 = 0x1572;
pub const PDC_GPU_TCS1_CMD_ENABLE_BANK: u32 = 0x1573;
pub const PDC_GPU_TCS1_CMD_WAIT_FOR_CMPL_BANK: u32 = 0x1574;
pub const PDC_GPU_TCS1_CMD0_MSGID: u32 = 0x1575;
pub const PDC_GPU_TCS1_CMD0_ADDR: u32 = 0x1576;
pub const PDC_GPU_TCS1_CMD0_DATA: u32 = 0x1577;
pub const PDC_GPU_TCS3_CONTROL: u32 = 0x15d6;
pub const PDC_GPU_TCS3_CMD_ENABLE_BANK: u32 = 0x15d7;
pub const PDC_GPU_TCS3_CMD_WAIT_FOR_CMPL_BANK: u32 = 0x15d8;
pub const PDC_GPU_TCS3_CMD0_MSGID: u32 = 0x15d9;
pub const PDC_GPU_TCS3_CMD0_ADDR: u32 = 0x15da;
pub const PDC_GPU_TCS3_CMD0_DATA: u32 = 0x15db;

// GPU, dwords.
pub const RBBM_GBIF_HALT: u32 = 0x0016;
pub const RBBM_GBIF_HALT_ACK: u32 = 0x0017;
pub const RBBM_SW_RESET_CMD: u32 = 0x0043;
pub const RBBM_VBIF_CLIENT_QOS_CNTL: u32 = 0x0010;
pub const RBBM_INTERFACE_HANG_INT_CNTL: u32 = 0x001f;
pub const RBBM_INT_CLEAR_CMD: u32 = 0x0037;
pub const RBBM_INT_0_MASK: u32 = 0x0038;
pub const RBBM_INT_0_STATUS: u32 = 0x0201;
pub const RBBM_STATUS: u32 = 0x0210;
pub const RBBM_STATUS3: u32 = 0x0213;
pub const RBBM_PERFCTR_CNTL: u32 = 0x0500;
pub const RBBM_PERFCTR_GPU_BUSY_MASKED: u32 = 0x050b;
pub const CP_RB_BASE: u32 = 0x0800;
pub const CP_RB_CNTL: u32 = 0x0802;
pub const CP_RB_RPTR: u32 = 0x0806;
pub const CP_RB_WPTR: u32 = 0x0807;
pub const CP_SQE_CNTL: u32 = 0x0808;
pub const CP_HW_FAULT: u32 = 0x0821;
pub const CP_INTERRUPT_STATUS: u32 = 0x0823;
pub const CP_PROTECT_STATUS: u32 = 0x0824;
pub const CP_SQE_INSTR_BASE: u32 = 0x0830;
pub const CP_ADDR_MODE_CNTL: u32 = 0x0842;
pub const CP_PROTECT_CNTL: u32 = 0x084f;
pub const CP_PROTECT_BASE: u32 = 0x0850;
pub const CP_SCRATCH_2: u32 = 0x0885;
pub const CP_ROQ_THRESHOLDS_1: u32 = 0x08c1;
pub const CP_ROQ_THRESHOLDS_2: u32 = 0x08c2;
pub const CP_MEM_POOL_SIZE: u32 = 0x08c3;
pub const CP_PERFCTR_CP_SEL_0: u32 = 0x08d0;
pub const CP_SQE_STAT_ADDR: u32 = 0x0908;
pub const CP_SQE_STAT_DATA: u32 = 0x0909;
pub const CP_IB1_BASE: u32 = 0x0928;
pub const CP_IB1_REM_SIZE: u32 = 0x092a;
pub const CP_AHB_CNTL: u32 = 0x098d;
pub const VSC_ADDR_MODE_CNTL: u32 = 0x0c01;
pub const UCHE_ADDR_MODE_CNTL: u32 = 0x0e00;
pub const UCHE_WRITE_RANGE_MAX: u32 = 0x0e05;
pub const UCHE_WRITE_THRU_BASE: u32 = 0x0e07;
pub const UCHE_TRAP_BASE: u32 = 0x0e09;
pub const UCHE_GMEM_RANGE_MIN: u32 = 0x0e0b;
pub const UCHE_GMEM_RANGE_MAX: u32 = 0x0e0d;
pub const UCHE_CACHE_WAYS: u32 = 0x0e17;
pub const UCHE_FILTER_CNTL: u32 = 0x0e18;
pub const UCHE_CLIENT_PF: u32 = 0x0e19;
pub const GBIF_HALT: u32 = 0x3c45;
pub const GBIF_HALT_ACK: u32 = 0x3c46;
pub const GRAS_ADDR_MODE_CNTL: u32 = 0x8601;
pub const RB_ADDR_MODE_CNTL: u32 = 0x8e05;
pub const VPC_ADDR_MODE_CNTL: u32 = 0x9601;
pub const PC_ADDR_MODE_CNTL: u32 = 0x9e01;
pub const PC_DBG_ECO_CNTL: u32 = 0x9e00;
pub const VFD_ADDR_MODE_CNTL: u32 = 0xa601;
pub const SP_ADDR_MODE_CNTL: u32 = 0xae01;
pub const TPL1_ADDR_MODE_CNTL: u32 = 0xb601;
pub const HLSQ_ADDR_MODE_CNTL: u32 = 0xbe05;
pub const RBBM_SECVID_TRUST_CNTL: u32 = 0xf400;
pub const RBBM_SECVID_TSB_TRUSTED_BASE: u32 = 0xf800;
pub const RBBM_SECVID_TSB_TRUSTED_SIZE: u32 = 0xf802;
pub const RBBM_SECVID_TSB_CNTL: u32 = 0xf803;
pub const RBBM_SECVID_TSB_ADDR_MODE_CNTL: u32 = 0xf810;
// UBWC / NC-mode registers of `a6xx_set_ubwc_config`.
pub const RBBM_NC_MODE_CNTL: u32 = 0x0534;
pub const UCHE_MODE_CNTL: u32 = 0x0e01;
pub const RB_NC_MODE_CNTL: u32 = 0x8e08;
pub const SP_NC_MODE_CNTL: u32 = 0xae02;
pub const TPL1_NC_MODE_CNTL: u32 = 0xb604;
