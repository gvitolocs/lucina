use core::fmt;

/// Everything that can go wrong reading or editing a DTB. Malformed input only ever produces one of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The blob does not start with the FDT magic 0xd00dfeed.
    BadMagic,
    /// A read ran past the end of a buffer or block.
    Truncated,
    /// `totalsize` is smaller than the header or larger than the buffer.
    BadTotalSize,
    /// Header version is below 17.
    UnsupportedVersion(u32),
    /// `last_comp_version` is above 16: a reader of version 16 could not read it.
    IncompatibleVersion(u32),
    /// A block offset or size is not aligned as the spec requires (struct 4, reservations 8).
    Misaligned,
    /// A block lies (partly) outside `totalsize`.
    OutOfBounds,
    /// Two header/blocks overlap.
    Overlap,
    /// Unknown structure token.
    BadToken(u32),
    /// A name or string is not NUL-terminated or not valid UTF-8.
    BadString,
    /// Tokens are not a single well-nested tree ending in FDT_END.
    BadStructure,
    /// A property value has the wrong length for how it is read.
    BadLength,
    /// Unsupported or invalid `#address-cells` / `#size-cells`, or a value that does not fit them.
    BadCells,
    /// A node path is malformed.
    BadPath,
    /// A node or property does not exist.
    NotFound,
    /// A node or property already exists.
    Exists,
    /// The tree nests deeper than [`crate::MAX_DEPTH`].
    TooDeep,
    /// The serialized tree does not fit the 32-bit offsets of the format.
    TooLarge,
    /// A `--region` specification is malformed.
    BadRegion,
    /// Bad framebuffer parameters.
    BadFramebuffer,
    /// The output buffer is too small for the patched blob.
    BufferTooSmall,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::BadMagic => f.write_str("not a device tree blob (bad magic)"),
            Error::Truncated => f.write_str("device tree is truncated"),
            Error::BadTotalSize => {
                f.write_str("totalsize is smaller than the header or larger than the file")
            }
            Error::UnsupportedVersion(v) => {
                write!(f, "unsupported device tree version {v} (need 17)")
            }
            Error::IncompatibleVersion(v) => write!(f, "last_comp_version {v} is above 16"),
            Error::Misaligned => f.write_str("a block offset or size is misaligned"),
            Error::OutOfBounds => f.write_str("a block lies outside totalsize"),
            Error::Overlap => f.write_str("header and blocks overlap"),
            Error::BadToken(t) => write!(f, "unknown structure token {t:#x}"),
            Error::BadString => f.write_str("a name or string is not NUL-terminated UTF-8"),
            Error::BadStructure => f.write_str("structure block is not one well-nested tree"),
            Error::BadLength => f.write_str("property value has the wrong length"),
            Error::BadCells => f.write_str("unsupported or mismatching #address-cells/#size-cells"),
            Error::BadPath => f.write_str("malformed node path"),
            Error::NotFound => f.write_str("node or property not found"),
            Error::Exists => f.write_str("node or property already exists"),
            Error::TooDeep => f.write_str("device tree nests too deeply"),
            Error::TooLarge => f.write_str("device tree is too large to serialize"),
            Error::BadFramebuffer => f.write_str("bad framebuffer parameters"),
            Error::BufferTooSmall => f.write_str("output buffer too small for the device tree"),
            Error::BadRegion => {
                f.write_str("bad region, expected <name>@<hex addr>+<hex size>[,no-map]")
            }
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for Error {}
