//! Qualcomm SoC services that a remote processor needs, for the Galaxy Book Go's modem DSP (SC7180), which hosts the
//! WCN3990's Wi-Fi firmware. Nothing here has run on Qualcomm silicon. The crate is tested on the host; the kernel
//! (`device::qcom_wifi`, patch 0620) uses its read-only parts in `lucina.wifi=report`.
//!
//! * [`smem`]: the shared memory items (Linux `drivers/soc/qcom/smem.c`). [`smem::Smem::probe`] and
//!   [`smem::Smem::get`] only read, and only what Linux reads: the header, the partition table, and the partitions
//!   the application processor shares. Another processor's partition may be protected from us.
//! * [`hwlock`]: the TCSR mutex that guards SMEM allocation (`qcom_hwspinlock.c`).
//! * [`smp2p`]: the single-bit signalling edge to the modem (`smp2p.c`), through which `ready` and `handover` come.
//! * [`cmd_db`]: the read-only table of RPMh resources (`cmd-db.c`).
//! * [`qmp`]: the mailbox to the always-on processor (`qcom_aoss.c`), used for the modem's `load_state` message.
//! * [`scm`]: secure monitor calls (`qcom_scm.c`, `qcom_scm-smc.c`): the convention probe and the PAS calls.
//! * [`mdt`]: the modem firmware image (`mdt_loader.c`), checked against the real `qcmpss7180_nm.mbn` when present.
//! * [`pas`]: the order of a PAS cold start (`qcom_q6v5_pas.c`, `qcom_q6v5.c`) over hooks the caller provides.
//!
//! `smem`, `hwlock`, `smp2p`, `mdt` and `scm` began as `drivers/soc/qcom/src` of hodgesds/narf (GPL-2.0-or-later,
//! itself not run on silicon) and `cmd_db` as `drivers/soc/qcom-cmd-db` of petitstrawberry/scarlet-project-chromebook
//! (GPL-2.0-only). All were rechecked against Linux 7.1 (GPL-2.0-only), which decides behaviour. The rechecks found
//! what those sources got wrong for this SoC; each module's comment lists them. `docs/research/integration-decisions.md`
//! records this.
//!
//! Every value read from shared memory comes from another processor: it is bounds-checked, and nothing panics or
//! loops without bound on a bad one. All access is by aligned 32-bit words ([`io::RegIo`]): the kernel maps these
//! regions as device memory, and write-back mapping the command DB is known to crash the machine (the XPU sees the
//! eviction of a clean line as a write; Linux commit f9bb896eab22).

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod cmd_db;
pub mod hwlock;
pub mod io;
pub mod mdt;
pub mod pas;
pub mod qmp;
pub mod scm;
pub mod smem;
pub mod smp2p;
