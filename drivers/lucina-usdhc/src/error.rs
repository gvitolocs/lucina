use core::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Waiting for this took too long.
    Timeout(&'static str),
    /// The controller reported an error for a command.
    Command { cmd: u8, status: u32 },
    /// The controller reported an error while moving a command's data.
    Data { cmd: u8, status: u32 },
    /// The card's status after a command has an error bit set.
    CardStatus { cmd: u8, status: u32 },
    /// This driver does not send this.
    Refused { cmd: u8, arg: u32 },
    /// The card cannot be used, and why.
    Unusable(&'static str),
    /// The card is reading from a boot, RPMB or general purpose partition.
    OtherPartition(u8),
    /// A buffer is not what the controller can move.
    BadBuffer,
    /// Past the end of the card.
    OutOfRange,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Error::Timeout(what) => write!(f, "timed out waiting for {what}"),
            Error::Command { cmd, status } => write!(f, "CMD{cmd} failed, status {status:#010x}"),
            Error::Data { cmd, status } => write!(f, "data of CMD{cmd} failed, status {status:#010x}"),
            Error::CardStatus { cmd, status } => write!(f, "CMD{cmd}: card status {status:#010x}"),
            Error::Refused { cmd, arg } => write!(f, "CMD{cmd} with {arg:#010x} is not sent here"),
            Error::Unusable(why) => write!(f, "the card cannot be used: {why}"),
            Error::OtherPartition(n) => write!(f, "the card has partition {n} selected, not the user area"),
            Error::BadBuffer => write!(f, "a buffer the controller cannot move"),
            Error::OutOfRange => write!(f, "past the end of the card"),
        }
    }
}
