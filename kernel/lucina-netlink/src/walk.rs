//! Walking netlink messages and their attributes, as plain checks on bytes. Verus proves this very file
//! (`verification/juno-netlink/rules.rs`; rustc ignores the attributes). The rules are musl's `NLMSG_OK`, `NLMSG_NEXT`,
//! `NLMSG_RTA`, `NLMSG_RTAOK` and `RTA_NEXT` (`src/network/netlink.h`): a message's length covers its 16-byte header and stays
//! inside what was received, an attribute's covers its 4-byte header and stays inside its message, and both are followed at the
//! next multiple of 4. Netlink is in the machine's byte order: little-endian on AArch64 and x86-64, which are all this reads.
#![allow(unexpected_cfgs)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// `n` rounded up to a multiple of 4 (`NLMSG_ALIGN`, `RTA_ALIGN`), or `None` past `usize`.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r matches Some(a) ==> a >= n && a < n + 4 && a % 4 == 0,
        r is None ==> n + 3 > usize::MAX,
))]
pub fn align4(n: usize) -> Option<usize> {
    if n > usize::MAX - 3 {
        return None;
    }
    let up = n + 3;
    Some(up - up % 4)
}

/// A little-endian `u16` at `pos`.
#[cfg_attr(verus_keep_ghost, verus_spec(
    requires
        pos + 2 <= buf@.len(),
))]
pub fn le16(buf: &[u8], pos: usize) -> u16 {
    (buf[pos] as u16) | ((buf[pos + 1] as u16) << 8u32)
}

/// A little-endian `u32` at `pos`.
#[cfg_attr(verus_keep_ghost, verus_spec(
    requires
        pos + 4 <= buf@.len(),
))]
pub fn le32(buf: &[u8], pos: usize) -> u32 {
    (buf[pos] as u32) | ((buf[pos + 1] as u32) << 8u32) | ((buf[pos + 2] as u32) << 16u32) | ((buf[pos + 3] as u32) << 24u32)
}

/// The message at `pos` of the `len` bytes received (`NLMSG_OK`): its length and type. Its length covers its header and ends
/// inside what was received.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        len <= buf@.len(),
    ensures
        r matches Some((n, _)) ==> 16 <= n && pos + n <= len,
))]
pub fn message(buf: &[u8], pos: usize, len: usize) -> Option<(usize, u16)> {
    if pos > len || len - pos < 16 {
        return None;
    }
    let n = le32(buf, pos) as usize;
    if n < 16 || n > len - pos {
        return None;
    }
    Some((n, le16(buf, pos + 4)))
}

/// The attribute at `pos` of a message that ends at `end` (`NLMSG_RTAOK`): its length and type. Its length covers its 4-byte
/// header and ends inside the message.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        end <= buf@.len(),
    ensures
        r matches Some((n, _)) ==> 4 <= n && pos + n <= end,
))]
pub fn attribute(buf: &[u8], pos: usize, end: usize) -> Option<(usize, u16)> {
    if pos > end || end - pos < 4 {
        return None;
    }
    let n = le16(buf, pos) as usize;
    if n < 4 || n > end - pos {
        return None;
    }
    Some((n, le16(buf, pos + 2)))
}
