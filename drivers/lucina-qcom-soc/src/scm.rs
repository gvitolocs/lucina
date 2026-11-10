//! Secure monitor calls, Linux 7.1 `drivers/firmware/qcom/qcom_scm.c` and `qcom_scm-smc.c` (GPL-2.0-only). Adapted from
//! narf `drivers/soc/qcom/src/scm.rs` (GPL-2.0-or-later, not run on silicon), made synchronous.
//!
//! Correction from the recheck against Linux: when the convention probe gets no answer on a `qcom,scm-sc7180` node,
//! Linux forces the ARM64 convention, since "some SC7180 firmwares didn't implement `IS_CALL_AVAIL`". narf would have
//! fallen to ARM32. The probe here takes that compatible as an argument and [`Convention::Forced64`] says it happened.
//!
//! A call: `x0` = `ARM_SMCCC_CALL_VAL(fast or std, 64 or 32, SIP, svc << 8 | cmd)`, `x1` = arginfo, `x2..x5` = the
//! arguments, `x6` = 0 at first and then what the monitor returned (the QCOM A6 quirk), results in `x1..x3`.
//!
//! [`Scm::probe_convention`], [`Scm::is_call_available`] and [`Scm::pas_supported`] change nothing in the secure
//! world; Linux makes them at every boot, and they are what `lucina.wifi=report` calls. The other PAS calls start or
//! stop the modem and are never used in that mode.

use alloc::vec::Vec;

use crate::io::Delay;

pub const OWNER_SIP: u32 = 2;
pub const SVC_PIL: u32 = 0x02;
pub const PIL_PAS_INIT_IMAGE: u32 = 0x01;
pub const PIL_PAS_MEM_SETUP: u32 = 0x02;
pub const PIL_PAS_AUTH_AND_RESET: u32 = 0x05;
pub const PIL_PAS_SHUTDOWN: u32 = 0x06;
pub const PIL_PAS_IS_SUPPORTED: u32 = 0x07;
pub const SVC_INFO: u32 = 0x06;
pub const INFO_IS_CALL_AVAIL: u32 = 0x01;
pub const SVC_BOOT: u32 = 0x01;
pub const BOOT_SET_REMOTE_STATE: u32 = 0x0a;
pub const SVC_MP: u32 = 0x0c;
pub const MP_VIDEO_VAR: u32 = 0x08;
pub const MP_ASSIGN: u32 = 0x16;
/// `QCOM_SCM_VMID_HLOS` and `QCOM_SCM_PERM_RW`.
pub const VMID_HLOS: u32 = 3;
pub const PERM_RW: u32 = 6;

const SCM_V2_EBUSY: i64 = -12;
const SCM_ENOMEM: i64 = -5;
const SCM_EOPNOTSUPP: i64 = -4;
const SCM_EINVAL_ADDR: i64 = -3;
const SCM_EINVAL_ARG: i64 = -2;
const SCM_ERROR: i64 = -1;
const SCM_INTERRUPTED: i64 = 1;
const SCM_WAITQ_SLEEP: i64 = 2;

const EBUSY_WAIT_MS: u32 = 30;
const EBUSY_MAX_RETRY: u32 = 20;
/// Linux loops without bound; a stuck monitor must not hang us silently. Authenticating the modem image keeps the
/// monitor busy for seconds and every timer tick returns here, so the limit is far above what a real call needs.
const INTERRUPTED_MAX: u32 = 1 << 24;

/// `QCOM_SCM_ARGS` argument kinds.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum ArgType {
    Val = 0,
    Ro = 1,
    Rw = 2,
    BufVal = 3,
}

/// `QCOM_SCM_ARGS(num, types...)`.
pub const fn arginfo(num: u32, types: &[ArgType]) -> u32 {
    let mut v = num & 0xf;
    let mut i = 0;
    while i < types.len() && i < 10 {
        v |= (types[i] as u32 & 0x3) << (4 + 2 * i);
        i += 1;
    }
    v
}

/// `SCM_SMC_FNID(svc, cmd)`.
pub const fn smc_fnid(svc: u32, cmd: u32) -> u32 {
    ((svc & 0xff) << 8) | (cmd & 0xff)
}

/// `ARM_SMCCC_CALL_VAL`.
pub const fn call_val(fast: bool, smc64: bool, owner: u32, fnid: u32) -> u64 {
    ((if fast { 1u32 << 31 } else { 0 })
        | (if smc64 { 1 << 30 } else { 0 })
        | ((owner & 0x3f) << 24)
        | (fnid & 0xffff)) as u64
}

/// The `smc #0` trap: `regs` are `x0..x7` in and out.
pub trait SmcConduit {
    fn smc(&self, regs: &mut [u64; 8]);
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Convention {
    /// Answered `IS_CALL_AVAIL` on the 64-bit convention.
    Arm64,
    /// Did not answer, and the node is `qcom,scm-sc7180`: as Linux, assume 64-bit.
    Forced64,
    Arm32,
    Legacy,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ScmError {
    Io,
    Invalid,
    NotSupported,
    NoMem,
    Busy,
    /// The call succeeded with a non-zero `result[0]` (Linux returns that as the error).
    Result(u64),
    LegacyConvention,
    /// `QCOM_SCM_WAITQ_SLEEP`: firmware wait queues, which newer SoCs use, are an error here.
    WaitQueue,
    Interrupted,
}

const fn remap_error(code: i64) -> ScmError {
    match code {
        SCM_ERROR => ScmError::Io,
        SCM_EINVAL_ADDR | SCM_EINVAL_ARG => ScmError::Invalid,
        SCM_EOPNOTSUPP => ScmError::NotSupported,
        SCM_ENOMEM => ScmError::NoMem,
        SCM_V2_EBUSY => ScmError::Busy,
        _ => ScmError::Invalid,
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Desc {
    pub svc: u32,
    pub cmd: u32,
    pub arginfo: u32,
    pub args: [u64; 4],
}

/// One SCM user. Calls take `&mut self`: as with `qcom_scm_lock`, the monitor sees one call at a time.
pub struct Scm<C, D> {
    conduit: C,
    delay: D,
    convention: Option<Convention>,
    last_regs: [u64; 8],
}

impl<C: SmcConduit, D: Delay> Scm<C, D> {
    pub const fn new(conduit: C, delay: D) -> Self {
        Self {
            conduit,
            delay,
            convention: None,
            last_regs: [0; 8],
        }
    }

    /// `__scm_smc_do_quirk`.
    fn do_quirk(&mut self, regs: &[u64; 8]) -> Result<[u64; 8], ScmError> {
        let mut r = *regs;
        r[6] = 0;
        for _ in 0..INTERRUPTED_MAX {
            let mut io = r;
            self.conduit.smc(&mut io);
            if io[0] as i64 != SCM_INTERRUPTED {
                return Ok(io);
            }
            r[0] = io[0];
            r[6] = io[6];
        }
        Err(ScmError::Interrupted)
    }

    fn regs_for(desc: &Desc, fast: bool, smc64: bool) -> [u64; 8] {
        let mut r = [0u64; 8];
        r[0] = call_val(fast, smc64, OWNER_SIP, smc_fnid(desc.svc, desc.cmd));
        r[1] = desc.arginfo as u64;
        r[2..6].copy_from_slice(&desc.args);
        r
    }

    /// `__get_convention`: 64-bit, then forced 64-bit on SC7180, then 32-bit, then legacy. Atomic calls.
    pub fn probe_convention(&mut self, is_sc7180: bool) -> Convention {
        if let Some(c) = self.convention {
            return c;
        }
        let avail = Desc {
            svc: SVC_INFO,
            cmd: INFO_IS_CALL_AVAIL,
            arginfo: arginfo(1, &[]),
            args: [
                (smc_fnid(SVC_INFO, INFO_IS_CALL_AVAIL) | (OWNER_SIP << 24)) as u64,
                0,
                0,
                0,
            ],
        };
        let answered = |r: Result<[u64; 8], ScmError>| matches!(r, Ok(v) if v[0] == 0 && v[1] == 1);
        let found = if answered(self.do_quirk(&Self::regs_for(&avail, true, true))) {
            Convention::Arm64
        } else if is_sc7180 {
            Convention::Forced64
        } else if answered(self.do_quirk(&Self::regs_for(&avail, true, false))) {
            Convention::Arm32
        } else {
            Convention::Legacy
        };
        self.convention = Some(found);
        found
    }

    /// What the monitor left in `x0`-`x7` after the last yielding call, for a log when it refuses something.
    pub fn last_regs(&self) -> [u64; 8] {
        self.last_regs
    }

    /// `qcom_scm_call`, the yielding form. Returns `result[0..3]`. Needs [`Scm::probe_convention`] first.
    pub fn call(&mut self, desc: &Desc) -> Result<[u64; 3], ScmError> {
        let smc64 = match self.convention {
            Some(Convention::Arm64 | Convention::Forced64) => true,
            Some(Convention::Arm32) => false,
            Some(Convention::Legacy) | None => return Err(ScmError::LegacyConvention),
        };
        let regs = Self::regs_for(desc, false, smc64);
        let mut retry = 0;
        loop {
            let r = self.do_quirk(&regs)?;
            self.last_regs = r;
            match r[0] as i64 {
                0 => return Ok([r[1], r[2], r[3]]),
                SCM_WAITQ_SLEEP => return Err(ScmError::WaitQueue),
                SCM_V2_EBUSY => {
                    // `retry_count++ > QCOM_SCM_EBUSY_MAX_RETRY` of `__scm_smc_do`.
                    if retry > EBUSY_MAX_RETRY {
                        return Err(ScmError::Busy);
                    }
                    retry += 1;
                    self.delay.sleep_ms(EBUSY_WAIT_MS);
                }
                status => return Err(remap_error(status)),
            }
        }
    }

    /// `ret ?: res.result[0]`, the PAS calls' return shape.
    fn call_result0(&mut self, desc: &Desc) -> Result<(), ScmError> {
        match self.call(desc)? {
            [0, ..] => Ok(()),
            [r, ..] => Err(ScmError::Result(r)),
        }
    }

    /// `__qcom_scm_is_call_available`.
    pub fn is_call_available(&mut self, svc: u32, cmd: u32) -> bool {
        let desc = Desc {
            svc: SVC_INFO,
            cmd: INFO_IS_CALL_AVAIL,
            arginfo: arginfo(1, &[]),
            args: [(smc_fnid(svc, cmd) | (OWNER_SIP << 24)) as u64, 0, 0, 0],
        };
        matches!(self.call(&desc), Ok(r) if r[0] != 0)
    }

    /// `qcom_scm_pas_supported`.
    pub fn pas_supported(&mut self, pas_id: u32) -> bool {
        if !self.is_call_available(SVC_PIL, PIL_PAS_IS_SUPPORTED) {
            return false;
        }
        let desc = Desc {
            svc: SVC_PIL,
            cmd: PIL_PAS_IS_SUPPORTED,
            arginfo: arginfo(1, &[]),
            args: [pas_id as u64, 0, 0, 0],
        };
        matches!(self.call(&desc), Ok(r) if r[0] != 0)
    }

    /// `qcom_scm_pas_init_image`, the metadata already in physically contiguous memory cleaned to the point of
    /// coherency, which stays allocated until after [`Scm::pas_auth_and_reset`].
    pub fn pas_init_image(&mut self, pas_id: u32, metadata_phys: u64) -> Result<(), ScmError> {
        self.call_result0(&Desc {
            svc: SVC_PIL,
            cmd: PIL_PAS_INIT_IMAGE,
            arginfo: arginfo(2, &[ArgType::Val, ArgType::Rw]),
            args: [pas_id as u64, metadata_phys, 0, 0],
        })
    }

    /// `qcom_scm_pas_mem_setup`.
    pub fn pas_mem_setup(&mut self, pas_id: u32, addr: u64, size: u64) -> Result<(), ScmError> {
        self.call_result0(&Desc {
            svc: SVC_PIL,
            cmd: PIL_PAS_MEM_SETUP,
            arginfo: arginfo(3, &[]),
            args: [pas_id as u64, addr, size, 0],
        })
    }

    /// `qcom_scm_pas_auth_and_reset`.
    pub fn pas_auth_and_reset(&mut self, pas_id: u32) -> Result<(), ScmError> {
        self.call_result0(&Desc {
            svc: SVC_PIL,
            cmd: PIL_PAS_AUTH_AND_RESET,
            arginfo: arginfo(1, &[]),
            args: [pas_id as u64, 0, 0, 0],
        })
    }

    /// `qcom_scm_pas_shutdown`.
    pub fn pas_shutdown(&mut self, pas_id: u32) -> Result<(), ScmError> {
        self.call_result0(&Desc {
            svc: SVC_PIL,
            cmd: PIL_PAS_SHUTDOWN,
            arginfo: arginfo(1, &[]),
            args: [pas_id as u64, 0, 0, 0],
        })
    }

    /// `qcom_scm_mem_protect_video_var`: the video firmware's content-protected ranges, all four 32-bit (`cp_start`, `cp_size`,
    /// `cp_nonpixel_start`, `cp_nonpixel_size`).
    pub fn mem_protect_video_var(&mut self, cp_start: u32, cp_size: u32, nonpixel_start: u32, nonpixel_size: u32) -> Result<(), ScmError> {
        self.call_result0(&Desc {
            svc: SVC_MP,
            cmd: MP_VIDEO_VAR,
            arginfo: arginfo(4, &[ArgType::Val, ArgType::Val, ArgType::Val, ArgType::Val]),
            args: [cp_start as u64, cp_size as u64, nonpixel_start as u64, nonpixel_size as u64],
        })
    }

    /// `qcom_scm_set_remote_state`: 1 resumes a remote processor the secure world started, 0 suspends it (Venus's
    /// `venus_set_hw_state`).
    pub fn set_remote_state(&mut self, state: u32, id: u32) -> Result<(), ScmError> {
        self.call_result0(&Desc {
            svc: SVC_BOOT,
            cmd: BOOT_SET_REMOTE_STATE,
            arginfo: arginfo(2, &[]),
            args: [state as u64, id as u64, 0, 0],
        })
    }
    /// `__qcom_scm_assign_mem`: `blob` is [`AssignMem::bytes`], written to physically contiguous memory at `phys`, cleaned
    /// to the point of coherency. Seven arguments do not fit the registers: the fourth register holds the address of the
    /// rest (`__scm_smc_call`), which the blob carries as its last part.
    pub fn assign_mem(&mut self, phys: u64, blob: &AssignMem) -> Result<(), ScmError> {
        use ArgType::{Ro, Val};
        self.call_result0(&Desc {
            svc: SVC_MP,
            cmd: MP_ASSIGN,
            arginfo: arginfo(7, &[Ro, Val, Ro, Val, Ro, Val, Val]),
            args: [
                phys + blob.map_at,
                MEM_MAP_INFO_SIZE,
                phys + blob.src_at,
                phys + blob.ext_at,
            ],
        })
    }
}

/// `sizeof(struct qcom_scm_mem_map_info)`.
const MEM_MAP_INFO_SIZE: u64 = 16;
/// `sizeof(struct qcom_scm_current_perm_info)`.
const PERM_INFO_SIZE: usize = 24;
/// `SCM_SMC_N_EXT_ARGS`: `MAX_QCOM_SCM_ARGS - SCM_SMC_N_REG_ARGS + 1`.
const N_EXT_ARGS: usize = 7;

const fn align64(n: usize) -> usize {
    (n + 63) & !63
}

/// The tables `qcom_scm_assign_mem` builds for one region: the source VMIDs, the region, the destination VMIDs with their
/// permissions, each 64-byte aligned, and then the arguments that do not fit the registers.
pub struct AssignMem {
    src_at: u64,
    map_at: u64,
    ext_at: u64,
    src: Vec<u32>,
    mem: (u64, u64),
    dest: Vec<(u32, u32)>,
}

impl AssignMem {
    pub fn new(src: &[u32], mem_addr: u64, mem_size: u64, dest: &[(u32, u32)]) -> Self {
        let src_size = src.len() * 4;
        let map_at = align64(src_size);
        let dest_at = map_at + align64(MEM_MAP_INFO_SIZE as usize);
        let ext_at = dest_at + align64(dest.len() * PERM_INFO_SIZE);
        Self {
            src_at: 0,
            map_at: map_at as u64,
            ext_at: ext_at as u64,
            src: src.to_vec(),
            mem: (mem_addr, mem_size),
            dest: dest.to_vec(),
        }
    }

    /// How many bytes [`AssignMem::bytes`] returns.
    pub fn byte_len(&self) -> usize {
        self.ext_at as usize + N_EXT_ARGS * 8
    }

    /// The tables for memory at physical address `phys`.
    pub fn bytes(&self, phys: u64) -> Vec<u8> {
        let mut out = alloc::vec![0u8; self.byte_len()];
        for (i, vmid) in self.src.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&vmid.to_le_bytes());
        }
        let map = self.map_at as usize;
        out[map..map + 8].copy_from_slice(&self.mem.0.to_le_bytes());
        out[map + 8..map + 16].copy_from_slice(&self.mem.1.to_le_bytes());
        let dest = map + align64(MEM_MAP_INFO_SIZE as usize);
        for (i, (vmid, perm)) in self.dest.iter().enumerate() {
            let at = dest + i * PERM_INFO_SIZE;
            out[at..at + 4].copy_from_slice(&vmid.to_le_bytes());
            out[at + 4..at + 8].copy_from_slice(&perm.to_le_bytes());
        }
        // args[3..]: src_sz, dest, dest_sz, 0, and zeros.
        let ext = self.ext_at as usize;
        let args = [
            (self.src.len() * 4) as u64,
            phys + dest as u64,
            (self.dest.len() * PERM_INFO_SIZE) as u64,
        ];
        for (i, a) in args.iter().enumerate() {
            out[ext + i * 8..ext + i * 8 + 8].copy_from_slice(&a.to_le_bytes());
        }
        out
    }
}
