//! Qualcomm SDCC v5 additions to SDHCI: vendor registers in the same window as the standard ones.
//!
//! Offsets and sequences from Linux `sdhci-msm.c` (`sdhci_msm_v5_offset`, `sdhci_msm_probe`,
//! `sdhci_msm_handle_pwr_irq`, `sdhci_msm_set_uhs_signaling`).

pub const CORE_DLL_CONFIG: usize = 0x200;
pub const CORE_DLL_PDN: u32 = 1 << 29;
pub const CORE_DLL_RST: u32 = 1 << 30;
pub const CORE_VENDOR_SPEC: usize = 0x20c;
pub const CORE_VENDOR_SPEC_POR_VAL: u32 = 0x0a9c;
pub const CORE_VENDOR_SPEC_CAPABILITIES0: usize = 0x21c;
pub const CORE_PWRCTL_STATUS: usize = 0x240;
pub const CORE_PWRCTL_MASK: usize = 0x244;
pub const CORE_PWRCTL_CLEAR: usize = 0x248;
pub const CORE_PWRCTL_CTL: usize = 0x24c;
pub const CORE_VENDOR_SPEC3: usize = 0x250;
pub const CORE_PWRSAVE_DLL: u32 = 1 << 3;
pub const CORE_MCI_VERSION: usize = 0x318;
/// One past the highest register touched here; a `reg` smaller than this is not this block.
pub const REQUIRED_WINDOW: usize = 0x31c;

pub const PWRCTL_BUS_OFF: u32 = 1 << 0;
pub const PWRCTL_BUS_ON: u32 = 1 << 1;
pub const PWRCTL_IO_LOW: u32 = 1 << 2;
pub const PWRCTL_IO_HIGH: u32 = 1 << 3;
pub const PWRCTL_REQUESTS: u32 = 0xf;
pub const PWRCTL_ACK_BUS_SUCCESS: u32 = 1 << 0;
pub const PWRCTL_ACK_IO_SUCCESS: u32 = 1 << 2;

/// What to write to `CORE_PWRCTL_CTL` for these requests. The rails are set before the driver powers the bus and stay
/// as they are, so each request is already satisfied; Linux switches its regulators at this point.
pub fn pwrctl_ack(requests: u32) -> u32 {
    let mut ack = 0;
    if requests & (PWRCTL_BUS_ON | PWRCTL_BUS_OFF) != 0 {
        ack |= PWRCTL_ACK_BUS_SUCCESS;
    }
    if requests & (PWRCTL_BUS_ON | PWRCTL_BUS_OFF | PWRCTL_IO_LOW | PWRCTL_IO_HIGH) != 0 {
        ack |= PWRCTL_ACK_IO_SUCCESS;
    }
    ack
}

/// Whether the core version wants the capabilities written back (`sdhci_msm_probe`).
pub fn core_wants_capabilities(version: u32) -> bool {
    let major = version >> 28;
    let minor = version & 0xff;
    major >= 1 && minor != 0x11 && minor != 0x12
}
