use core::fmt;

/// Everything that can go wrong between the driver and the controller or the card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// A register or the card did not reach a state in time; the string names the wait.
    Timeout(&'static str),
    /// The registers read as all ones: nothing answers there.
    NoController,
    /// The controller reports no card in the slot.
    NoCard,
    /// The controller offers no voltage an SD card can start at.
    NoVoltage,
    /// A command failed: its index and the interrupt status.
    Command { cmd: u8, status: u32 },
    /// A data transfer failed: the command, the interrupt status and the ADMA error status.
    Data { cmd: u8, status: u32, adma: u8 },
    /// The card reported errors in its R1 status.
    CardStatus { cmd: u8, status: u32 },
    /// The card did not leave its busy state after ACMD41, or does not take the host's voltage.
    Unusable(&'static str),
    /// A write on a host started read-only, or on a write-protected card.
    ReadOnly,
    /// A buffer the controller cannot address, or a request that does not add up.
    BadBuffer,
    /// A request past the end of the card.
    OutOfRange,
    /// A data-line reset did not finish earlier: the controller may still be doing DMA, so nothing more is sent.
    Wedged,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Timeout(what) => write!(f, "timed out waiting for {what}"),
            Error::NoController => write!(f, "no controller answers (registers read as all ones)"),
            Error::NoCard => write!(f, "no card in the slot"),
            Error::NoVoltage => write!(f, "no usable voltage in the capabilities"),
            Error::Command { cmd, status } => write!(f, "CMD{cmd} failed, status {status:#010x}"),
            Error::Data { cmd, status, adma } => {
                write!(
                    f,
                    "data of CMD{cmd} failed, status {status:#010x}, ADMA error {adma:#04x}"
                )
            }
            Error::CardStatus { cmd, status } => write!(f, "CMD{cmd}: card status {status:#010x}"),
            Error::Unusable(what) => write!(f, "card unusable: {what}"),
            Error::ReadOnly => write!(f, "read-only"),
            Error::BadBuffer => write!(f, "buffer not usable by the controller"),
            Error::OutOfRange => write!(f, "past the end of the card"),
            Error::Wedged => write!(
                f,
                "controller wedged: an earlier data-line reset did not finish"
            ),
        }
    }
}
