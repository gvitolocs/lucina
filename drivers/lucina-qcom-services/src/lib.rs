// SPDX-License-Identifier: BSD-3-Clause
//
// Derived, through userspace/crates/lucina-qcom, from linux-msm pd-mapper and tqftpserv (Copyright (c) 2018,
// Linaro Ltd.), rmtfs (Copyright (c) 2016, Linaro Ltd) and qrtr's libqrtr (Copyright (c) 2015, Sony Mobile
// Communications Inc.). Sockets, threads and files are replaced by the traits below; the logic is the C's.

//! The three services the modem expects from its host, as state machines for the kernel thread that owns a
//! [`lucina_qrtr::router::Router`]. There is no AF_QIPCRTR: the thread passes router events in and the services
//! send through the router.
//!
//! * [`servreg`]: the service-registry locator ("pd-mapper", service 64): `wlan/fw` is `msm/modem/wlan_pd`,
//!   instance 180. Maps come from the `.jsn` files through [`Files`].
//! * [`tqftp`] with [`tftp`] and [`translate`]: TFTP on QRTR ("tqftpserv", service 4096): the modem reads
//!   `wlanmdsp.mbn` block by block through [`Files`]. Each transfer has its own router port. `/readwrite` is a
//!   small store in RAM: nothing a modem writes ever reaches a disk.
//! * [`rmtfs`]: the remote file system (service 14), always read only (`rmtfs -r -P`): [`Partitions`] can only
//!   read, modem writes stay in RAM. Sectors move through the `qcom,rmtfs-mem` window behind [`RmtfsMem`].
//!
//! # The kernel side
//!
//! ```ignore
//! let mut services = Services::new(files, partitions, window);
//! services.start(&mut router)?; // before the modem starts: FIRMWARE.md's 40 s race
//! // in the thread's loop, after feeding channel messages to router.handle_rx():
//! while let Some(event) = router.poll_event() {
//!     if let Some(event) = services.on_event(&mut router, event) {
//!         // not for these services: the WLFW client's port, its lookups
//!     }
//! }
//! while let Some(line) = services.pop_log() { /* console */ }
//! while let Some(msg) = router.poll_tx() { /* put on the IPCRTR channel */ }
//! ```
//!
//! Blocks are read one at a time, and only when the router's flow control lets one out, so a 3.8 MB file never
//! sits in memory. Nothing keeps time: as in the C, a transfer waits for its client or for
//! `DEL_CLIENT`/`BYE`.
//!
//! Every message was checked against bytes from the C originals' encoders (`tests/fixtures/c-oracle.txt`, made
//! by `userspace/crates/lucina-qcom/oracle/`, pins in its README). The modem in the tests is a model on a second
//! router, written from reading the C. Nothing here has met a real modem.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod json;
mod log;
pub mod qmi;
pub mod rmtfs;
mod services;
pub mod servreg;
pub mod tftp;
pub mod tqftp;
pub mod translate;

pub use log::Log;
pub use services::{OUTBOX_LIMIT, Ports, Services, StartError};

use alloc::vec::Vec;

/// A read that failed for a reason other than the end of the data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IoError;

/// Where the services find blobs, by the name relative to the firmware directory (`wlanmdsp.mbn`, `modemuw.jsn`,
/// `modem_pr/so/901_0_0.mbn`). Read only: there is no way to write through this. The name may come from the
/// modem; it passed [`translate::sanitize`] and has no `..` or empty component.
pub trait Files {
    /// The file's length, or `None` if there is no such file.
    fn file_len(&mut self, name: &str) -> Option<u64>;
    /// Like `pread(2)`: fill `buf` from `offset`, and say how much. 0 means the end of the file, also for an
    /// `offset` past it.
    fn read_at(&mut self, name: &str, offset: u64, buf: &mut [u8]) -> Result<usize, IoError>;
}

/// The modem's EFS partitions by GPT name (`modemst1`, `modemst2`, `fsc`, `fsg`, and the others in
/// [`rmtfs::PARTITIONS`]). Read only by construction: rmtfs here cannot write to a disk.
pub trait Partitions {
    /// The partition's length in bytes, or `None` if it is absent, which rmtfs answers with an error.
    fn partition_len(&mut self, name: &str) -> Option<u64>;
    /// Read exactly `buf.len()` bytes at `offset`; rmtfs never reads past [`Self::partition_len`].
    fn read_at(&mut self, name: &str, offset: u64, buf: &mut [u8]) -> Result<(), IoError>;
}

/// The `qcom,rmtfs-mem` window (device tree: client-id 1, vmid 15). Offsets are from its start, and rmtfs checks
/// them against [`Self::size`] before calling.
pub trait RmtfsMem {
    /// The physical address alloc-buf answers with.
    fn phys(&self) -> u64 {
        rmtfs::RMTFS_MEM_PHYS
    }
    fn size(&self) -> u64 {
        rmtfs::RMTFS_MEM_SIZE
    }
    fn read(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), IoError>;
    fn write(&mut self, offset: u64, buf: &[u8]) -> Result<(), IoError>;
}

/// An answer to send from the service's own port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub dst_node: u32,
    pub dst_port: u32,
    pub payload: Vec<u8>,
}

/// What a service publishes, as libqrtr's `qrtr_publish()` takes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Publish {
    pub service: u32,
    pub version: u16,
    pub instance: u16,
}

impl Publish {
    /// The word [`lucina_qrtr::router::Router::publish`] takes: `version | instance << 8`.
    pub const fn instance_word(&self) -> u32 {
        (self.version as u32) | ((self.instance as u32) << 8)
    }
}

/// pd-mapper: service 64, version 0x101, instance 0.
pub const PUBLISH_PD_MAPPER: Publish = Publish {
    service: servreg::SERVICE,
    version: servreg::VERSION,
    instance: servreg::INSTANCE,
};
/// tqftpserv: service 4096, version 1, instance 0.
pub const PUBLISH_TQFTP: Publish = Publish {
    service: tqftp::SERVICE,
    version: tqftp::VERSION,
    instance: tqftp::INSTANCE,
};
/// rmtfs: service 14, version 1, instance 0.
pub const PUBLISH_RMTFS: Publish = Publish {
    service: rmtfs::SERVICE,
    version: rmtfs::VERSION,
    instance: rmtfs::INSTANCE,
};
