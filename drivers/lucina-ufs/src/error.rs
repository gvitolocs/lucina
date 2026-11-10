use core::fmt;

use crate::scsi::Sense;

/// Everything that can go wrong between the driver and the controller or the device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The controller did not reach a state in time; the string names the wait.
    Timeout(&'static str),
    /// The host controller is not enabled and the reset policy forbids enabling it.
    NotEnabled,
    /// No device on the link (HCS.DP clear) after link startup or on an inherited link.
    NoDevice,
    /// A UIC command failed: command and result code from UCMDARG2.
    Uic { cmd: u8, result: u8 },
    /// The controller reported a fatal error (IS.HCFES/SBFES/UTPES/DFES, or a UIC error).
    Fatal(u32),
    /// The transfer request completed with an Overall Command Status other than SUCCESS.
    Ocs(u8),
    /// The response UPIU has an unexpected transaction type.
    BadResponse { expected: u8, got: u8 },
    /// The device answered with a non-zero response code (UPIU byte 6).
    Response(u8),
    /// A query request failed with this query response code.
    Query(u8),
    /// SCSI CHECK CONDITION (or another non-GOOD status without sense) for a command.
    Scsi { status: u8, sense: Option<Sense> },
    /// The policy refuses this command for this unit (writes to anything but the main user LUN, unknown opcodes).
    Refused { lun: u8, opcode: u8 },
    /// A data buffer the controller cannot address (above 4 GiB without 64-bit addressing, not dword-aligned,
    /// odd length) or too many segments for the PRDT.
    BadBuffer,
    /// A descriptor or SCSI payload too short or inconsistent.
    BadData(&'static str),
    /// The LUN is not one of the units found at probe time.
    NoUnit(u8),
    /// An earlier timeout left a request the controller would not give back: the host is unusable.
    Dead,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Timeout(what) => write!(f, "timed out waiting for {what}"),
            Error::NotEnabled => write!(
                f,
                "host controller not enabled and the reset policy forbids enabling it"
            ),
            Error::NoDevice => write!(f, "no device present on the link"),
            Error::Uic { cmd, result } => write!(
                f,
                "{} ({cmd:#04x}): result {result:#04x} {}",
                crate::regs::uic_name(*cmd),
                crate::regs::uic_result_name(*cmd, *result)
            ),
            Error::Fatal(is) => write!(f, "controller error, IS={is:#010x}"),
            Error::Ocs(ocs) => write!(f, "overall command status {ocs:#04x}"),
            Error::BadResponse { expected, got } => {
                write!(f, "response UPIU type {got:#04x}, expected {expected:#04x}")
            }
            Error::Response(r) => write!(f, "UPIU response {r:#04x}"),
            Error::Query(r) => write!(f, "query response {r:#04x}"),
            Error::Scsi {
                status,
                sense: Some(s),
            } => write!(
                f,
                "SCSI status {status:#04x}, sense key {:#x} asc {:#04x} ascq {:#04x}",
                s.key, s.asc, s.ascq
            ),
            Error::Scsi {
                status,
                sense: None,
            } => write!(f, "SCSI status {status:#04x}"),
            Error::Refused { lun, opcode } => {
                write!(
                    f,
                    "command {opcode:#04x} refused for LUN {lun:#04x} (read-only unit)"
                )
            }
            Error::BadBuffer => write!(f, "data buffer not usable by the controller"),
            Error::BadData(what) => write!(f, "malformed {what}"),
            Error::NoUnit(lun) => write!(f, "no logical unit {lun}"),
            Error::Dead => write!(f, "host controller left unusable by an earlier timeout"),
        }
    }
}
