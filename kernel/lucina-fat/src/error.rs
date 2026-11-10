//! Errors of the FAT logic. The kernel maps each one to an errno.

/// What went wrong.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    /// The block device failed.
    Io,
    /// The on-disk structures contradict themselves (bad chain, loop, broken entry).
    Corrupt,
    /// The caller asked for something that cannot be done (bad name, bad argument).
    Invalid,
    /// No such entry.
    NotFound,
    /// The name is already taken.
    Exists,
    /// The directory is not empty.
    NotEmpty,
    /// No free cluster or directory slot.
    NoSpace,
    /// A name longer than 255 UTF-16 units.
    NameTooLong,
    /// The operation needs a file but got a directory.
    IsDir,
    /// The operation needs a directory but got a file.
    NotDir,
    /// The volume is mounted read-only.
    ReadOnly,
    /// The file would be larger than FAT can hold (4 GiB - 1).
    TooBig,
    /// Not a FAT volume, or one this code does not handle.
    Unsupported,
}

/// Result of a FAT operation.
pub type Result<T> = core::result::Result<T, Error>;

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = match self {
            Error::Io => "I/O error",
            Error::Corrupt => "corrupt file system",
            Error::Invalid => "invalid argument",
            Error::NotFound => "no such file or directory",
            Error::Exists => "file exists",
            Error::NotEmpty => "directory not empty",
            Error::NoSpace => "no space left on device",
            Error::NameTooLong => "file name too long",
            Error::IsDir => "is a directory",
            Error::NotDir => "not a directory",
            Error::ReadOnly => "read-only file system",
            Error::TooBig => "file too large",
            Error::Unsupported => "unsupported file system",
        };
        f.write_str(s)
    }
}
