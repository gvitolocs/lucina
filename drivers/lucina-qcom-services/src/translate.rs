// SPDX-License-Identifier: BSD-3-Clause
//
// From userspace/crates/lucina-qcom/src/translate.rs, after linux-msm tqftpserv's translate.c (Copyright (c) 2018,
// Linaro Ltd.).

//! Where a TFTP path lives (tqftpserv's `translate.c`).
//!
//! * `/readonly/firmware/image/<f>` (and the `modem_pr`, vendor and `firmware_mnt` variants): a file beside the
//!   modem's firmware, read through [`crate::Files`] by its name relative to that directory.
//! * `/readwrite/<f>`: the modem's scratch files, in RAM here ([`crate::tqftp::RamStore`]).
//!
//! The search of remoteproc directories, `updates/` and `firmware_class.path` is the kernel's [`crate::Files`] now.

use alloc::string::{String, ToString};

const READONLY_PATH: &str = "/readonly/firmware/image/";
const READONLY_FW_BASE: &str = "/readonly/firmware/";
const READONLY_MODEM_PATH: &str = "/readonly/firmware/modem_pr";
const READWRITE_PATH: &str = "/readwrite/";
const READONLY_VENDOR_PATH: &str = "/readonly/vendor/firmware/";
const READONLY_VENDOR_MNT_PATH: &str = "/readonly/vendor/firmware_mnt/image/";

/// tqftpserv's `sanitize_path()`: no `..` component and no empty one (a `//` would make the rest absolute); one
/// leading slash is expected.
pub fn sanitize(path: &str) -> bool {
    let rest = path.strip_prefix('/').unwrap_or(path);
    if rest.is_empty() {
        return true;
    }
    let last = rest.split('/').count() - 1;
    // an empty last component is a trailing slash, which the C check allows
    rest.split('/')
        .enumerate()
        .all(|(i, c)| c != ".." && (!c.is_empty() || i == last))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// Relative to the firmware directory: a name for [`crate::Files`].
    ReadOnly(String),
    /// In the RAM store.
    ReadWrite(String),
}

/// Which namespace a requested path is in, in `translate_open()`'s order.
pub fn classify(path: &str) -> Option<Target> {
    if !sanitize(path) {
        return None;
    }
    if let Some(f) = path.strip_prefix(READONLY_PATH) {
        Some(Target::ReadOnly(f.to_string()))
    } else if path.starts_with(READONLY_MODEM_PATH) {
        path.get(READONLY_FW_BASE.len()..)
            .map(|f| Target::ReadOnly(f.to_string()))
    } else if let Some(f) = path.strip_prefix(READWRITE_PATH) {
        Some(Target::ReadWrite(f.to_string()))
    } else if let Some(f) = path.strip_prefix(READONLY_VENDOR_MNT_PATH) {
        Some(Target::ReadOnly(f.to_string()))
    } else {
        path.strip_prefix(READONLY_VENDOR_PATH)
            .map(|f| Target::ReadOnly(f.to_string()))
    }
}
