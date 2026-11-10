//! UFSHCI registers (JESD223 section 5; names as in Linux `include/ufs/ufshci.h`).

/// Controller capabilities.
pub const CAP: usize = 0x00;
/// UFSHCI version: major in bits 15:8, minor in 7:4, version suffix in 3:0 (`0x0210` = 2.1, `0x0300` = 3.0).
pub const VER: usize = 0x08;
/// Product ID (vendor specific).
pub const HCPID: usize = 0x10;
/// Manufacturer ID.
pub const HCMID: usize = 0x14;
/// Auto-hibernate idle timer.
pub const AHIT: usize = 0x18;
/// Interrupt status (write 1 to clear).
pub const IS: usize = 0x20;
/// Interrupt enable.
pub const IE: usize = 0x24;
/// Host controller status.
pub const HCS: usize = 0x30;
/// Host controller enable.
pub const HCE: usize = 0x34;
/// UIC error codes: PHY adapter, data link, network, transport, DME layers.
pub const UECPA: usize = 0x38;
pub const UECDL: usize = 0x3c;
pub const UECN: usize = 0x40;
pub const UECT: usize = 0x44;
pub const UECDME: usize = 0x48;
/// UTP transfer request interrupt aggregation control.
pub const UTRIACR: usize = 0x4c;
/// UTP transfer request list base (low, high), door bell, clear, run-stop, completion notification (4.0).
pub const UTRLBA: usize = 0x50;
pub const UTRLBAU: usize = 0x54;
pub const UTRLDBR: usize = 0x58;
pub const UTRLCLR: usize = 0x5c;
pub const UTRLRSR: usize = 0x60;
/// UTP task management request list base (low, high), door bell, clear, run-stop.
pub const UTMRLBA: usize = 0x70;
pub const UTMRLBAU: usize = 0x74;
pub const UTMRLDBR: usize = 0x78;
pub const UTMRLCLR: usize = 0x7c;
pub const UTMRLRSR: usize = 0x80;
/// UIC command and its three arguments.
pub const UICCMD: usize = 0x90;
pub const UCMDARG1: usize = 0x94;
pub const UCMDARG2: usize = 0x98;
pub const UCMDARG3: usize = 0x9c;

/// CAP: number of UTP transfer request slots, minus one (bits 4:0).
pub const CAP_NUTRS_MASK: u32 = 0x1f;
/// CAP: number of task management request slots, minus one (bits 18:16).
pub const CAP_NUTMRS_SHIFT: u32 = 16;
pub const CAP_NUTMRS_MASK: u32 = 0x7;
/// CAP: auto-hibernate support.
pub const CAP_AUTOH8: u32 = 1 << 23;
/// CAP: 64-bit addressing support.
pub const CAP_64AS: u32 = 1 << 24;
/// CAP: crypto support.
pub const CAP_CS: u32 = 1 << 28;
/// CAP: multi-circular-queue support (UFSHCI 4.0).
pub const CAP_MCQS: u32 = 1 << 30;

/// HCS: device present.
pub const HCS_DP: u32 = 1 << 0;
/// HCS: UTP transfer request list ready.
pub const HCS_UTRLRDY: u32 = 1 << 1;
/// HCS: UTP task management request list ready.
pub const HCS_UTMRLRDY: u32 = 1 << 2;
/// HCS: UIC command ready.
pub const HCS_UCRDY: u32 = 1 << 3;
/// HCS: UIC power mode change request status (bits 10:8).
pub const HCS_UPMCRS_SHIFT: u32 = 8;
pub const HCS_UPMCRS_MASK: u32 = 0x7;
/// HCS.UPMCRS value Linux requires after a power mode or hibernate command: `PWR_LOCAL`.
pub const HCS_UPMCRS_PWR_LOCAL: u32 = 1;

/// HCE: host controller enable.
pub const HCE_ENABLE: u32 = 1 << 0;

/// IS / IE bits.
pub const IS_UTRCS: u32 = 1 << 0;
pub const IS_UDEPRI: u32 = 1 << 1;
pub const IS_UE: u32 = 1 << 2;
pub const IS_UTMS: u32 = 1 << 3;
pub const IS_UPMS: u32 = 1 << 4;
pub const IS_UHXS: u32 = 1 << 5;
pub const IS_UHES: u32 = 1 << 6;
pub const IS_ULLS: u32 = 1 << 7;
pub const IS_ULSS: u32 = 1 << 8;
pub const IS_UTMRCS: u32 = 1 << 9;
pub const IS_UCCS: u32 = 1 << 10;
pub const IS_DFES: u32 = 1 << 11;
pub const IS_UTPES: u32 = 1 << 12;
pub const IS_HCFES: u32 = 1 << 16;
pub const IS_SBFES: u32 = 1 << 17;
pub const IS_CEFES: u32 = 1 << 18;
/// Errors after which Linux resets the host: `INT_FATAL_ERRORS` (ufshci.h) = DFES | UTPES | HCFES | SBFES | CEFES |
/// ULLS. The UIC error (IS.UE) is not in the mask; it is decided through the UEC registers (see
/// [`UECDL_PA_INIT_ERROR`]).
pub const IS_FATAL: u32 = IS_DFES | IS_UTPES | IS_HCFES | IS_SBFES | IS_CEFES | IS_ULLS;

/// UECDL bit that Linux treats as needing a host reset: `UIC_DATA_LINK_LAYER_ERROR_PA_INIT` (ufshci.h), the only UIC
/// error class `ufshcd_is_saved_err_fatal` counts as fatal.
pub const UECDL_PA_INIT_ERROR: u32 = 0x2000;

/// UIC commands (UICCMD).
pub const UIC_DME_GET: u8 = 0x01;
pub const UIC_DME_SET: u8 = 0x02;
pub const UIC_DME_PEER_GET: u8 = 0x03;
pub const UIC_DME_LINK_STARTUP: u8 = 0x16;
pub const UIC_DME_HIBER_ENTER: u8 = 0x17;
pub const UIC_DME_HIBER_EXIT: u8 = 0x18;
/// UCMDARG2 bits 7:0: result code of the last UIC command; 0 is success.
pub const UIC_RESULT_MASK: u32 = 0xff;

/// `UIC_ARG_MIB(attr)`: the MIB attribute in UCMDARG1 bits 31:16, selector 0.
pub const fn uic_arg_mib(attr: u16) -> u32 {
    (attr as u32) << 16
}

/// `UIC_ARG_MIB_SEL(attr, sel)`: the MIB attribute with a selector (an M-PHY lane).
pub const fn uic_arg_mib_sel(attr: u16, sel: u16) -> u32 {
    (attr as u32) << 16 | sel as u32
}

/// The M-PHY TX state machine of lane 0: how Linux tells whether the link is in hibernate
/// (`ufs_qcom_check_hibern8`, `MPHY_TX_FSM_STATE` with `UIC_ARG_MPHY_TX_GEN_SEL_INDEX(0)`).
pub const MPHY_TX_FSM_STATE: u16 = 0x41;
pub const TX_FSM_HIBERN8: u32 = 1;
/// The M-PHY RX state machine; RX lanes are selected from `PA_MAXDATALANES` up. Ours, from the M-PHY spec, not in
/// Linux (Linux reads only the TX FSM, `MPHY_TX_FSM_STATE` 0x41).
pub const MPHY_RX_FSM_STATE: u16 = 0xc1;
pub const RX_LANE0_SEL: u16 = 4;

/// UniPro / M-PHY attributes the driver reads for its report (Linux `include/ufs/unipro.h`).
pub const PA_ACTIVETXDATALANES: u16 = 0x1560;
pub const PA_CONNECTEDTXDATALANES: u16 = 0x1561;
pub const PA_TXGEAR: u16 = 0x1568;
pub const PA_HSSERIES: u16 = 0x156a;
pub const PA_PWRMODE: u16 = 0x1571;
pub const PA_ACTIVERXDATALANES: u16 = 0x1580;
pub const PA_CONNECTEDRXDATALANES: u16 = 0x1581;
pub const PA_RXGEAR: u16 = 0x1583;
pub const PA_TACTIVATE: u16 = 0x15a8;

/// A UIC command's name, for the log.
pub const fn uic_name(cmd: u8) -> &'static str {
    match cmd {
        UIC_DME_GET => "DME_GET",
        UIC_DME_SET => "DME_SET",
        UIC_DME_PEER_GET => "DME_PEER_GET",
        0x04 => "DME_PEER_SET",
        0x10 => "DME_POWERON",
        0x11 => "DME_POWEROFF",
        0x12 => "DME_ENABLE",
        0x14 => "DME_RESET",
        0x15 => "DME_ENDPOINTRESET",
        UIC_DME_LINK_STARTUP => "DME_LINKSTARTUP",
        UIC_DME_HIBER_ENTER => "DME_HIBER_ENTER",
        UIC_DME_HIBER_EXIT => "DME_HIBER_EXIT",
        0x1a => "DME_TEST_MODE",
        _ => "UIC?",
    }
}

/// A UIC result's name (Linux `UIC_CMD_RESULT_*`). 0x01 means an invalid attribute for the attribute commands
/// and plain failure for the others.
pub const fn uic_result_name(cmd: u8, result: u8) -> &'static str {
    let attribute = matches!(cmd, UIC_DME_GET | UIC_DME_SET | UIC_DME_PEER_GET | 0x04);
    match result {
        0x00 => "SUCCESS",
        0x01 if attribute => "INVALID_ATTR",
        0x01 => "FAILURE",
        0x02 => "INVALID_ATTR_VALUE",
        0x03 => "READ_ONLY_ATTR",
        0x04 => "WRITE_ONLY_ATTR",
        0x05 => "BAD_INDEX",
        0x06 => "LOCKED_ATTR",
        0x07 => "BAD_TEST_FEATURE_INDEX",
        0x08 => "PEER_COMM_FAILURE",
        0x09 => "BUSY",
        0x0a => "DME_FAILURE",
        _ => "?",
    }
}

/// HCS.UPMCRS's name (Linux `enum`: the result of the last power mode or hibernate command).
pub const fn upmcrs_name(hcs: u32) -> &'static str {
    match (hcs >> HCS_UPMCRS_SHIFT) & HCS_UPMCRS_MASK {
        0 => "PWR_OK",
        1 => "PWR_LOCAL",
        2 => "PWR_REMOTE",
        3 => "PWR_BUSY",
        4 => "PWR_ERROR_CAP",
        5 => "PWR_FATAL_ERROR",
        _ => "?",
    }
}

/// The names of the bits set in IS, for the log.
pub struct IsNames(pub u32);

impl core::fmt::Display for IsNames {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        const NAMES: [(u32, &str); 15] = [
            (IS_UTRCS, "UTRCS"),
            (IS_UDEPRI, "UDEPRI"),
            (IS_UE, "UE"),
            (IS_UTMS, "UTMS"),
            (IS_UPMS, "UPMS"),
            (IS_UHXS, "UHXS"),
            (IS_UHES, "UHES"),
            (IS_ULLS, "ULLS"),
            (IS_ULSS, "ULSS"),
            (IS_UTMRCS, "UTMRCS"),
            (IS_UCCS, "UCCS"),
            (IS_DFES, "DFES"),
            (IS_UTPES, "UTPES"),
            (IS_HCFES, "HCFES"),
            (IS_SBFES, "SBFES"),
        ];
        let mut any = false;
        for (bit, name) in NAMES {
            if self.0 & bit != 0 {
                write!(f, "{}{name}", if any { " " } else { "" })?;
                any = true;
            }
        }
        if !any {
            f.write_str("none")?;
        }
        Ok(())
    }
}

/// Decoded CAP register.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    pub raw: u32,
    /// Transfer request slots (1-32).
    pub nutrs: u32,
    /// Task management slots (1-8).
    pub nutmrs: u32,
    pub addr64: bool,
    pub auto_hibernate: bool,
    pub crypto: bool,
    pub mcq: bool,
}

impl Capabilities {
    pub const fn new(raw: u32) -> Self {
        Self {
            raw,
            nutrs: (raw & CAP_NUTRS_MASK) + 1,
            nutmrs: ((raw >> CAP_NUTMRS_SHIFT) & CAP_NUTMRS_MASK) + 1,
            addr64: raw & CAP_64AS != 0,
            auto_hibernate: raw & CAP_AUTOH8 != 0,
            crypto: raw & CAP_CS != 0,
            mcq: raw & CAP_MCQS != 0,
        }
    }
}

/// `(major, minor)` from VER; Linux `ufshci_version()` encodes 2.1 as 0x0210, 3.0 as 0x0300, 4.0 as 0x0400.
pub const fn version(raw: u32) -> (u32, u32) {
    ((raw >> 8) & 0xff, (raw >> 4) & 0xf)
}
