//! What the GCC says about a serial engine's source clock (SC7180's `gcc_qupv3_wrap*_s*_clk_src`), after Linux's
//! `clk-rcg2.c` and `gcc-sc7180.c`.
//!
//! * [`RcgState::read`] reads a root clock generator and works out its real rate: from its `CFG` and M/N registers,
//!   or, with DFS on, from the perf-level table that the serial engine's `CLK_SEL` indexes.
//! * [`start_root`] starts a root that the firmware left stopped: TCXO, divider 1, as the driver's last resort when a
//!   branch stays off after its vote.
//!
//! Register offsets are from the `cmd_rcgr` the caller names. All of this is on the GCC, which is always clocked; none
//! of it touches the serial engine.

use core::fmt;

use crate::Regs;

/// `BI_TCXO`, source 0 of `gcc_parent_map_1`, the only source whose rate Linux's I2C timing table is for.
pub const TCXO_HZ: u32 = 19_200_000;
const CFG: usize = 0x4;
const M: usize = 0x8;
const N: usize = 0xc;
/// `SE_CMD_DFSR_OFFSET`: bit 0 `SE_CMD_DFS_EN`, bits 4:1 the current perf level.
const DFSR: usize = 0x14;
const fn perf_cfg(level: usize) -> usize {
    0x1c + 4 * level
}
const fn perf_m(level: usize) -> usize {
    0x5c + 4 * level
}
const fn perf_n(level: usize) -> usize {
    0x9c + 4 * level
}
const CMD_UPDATE: u32 = 1 << 0;
const CMD_ROOT_EN: u32 = 1 << 1;
/// `MAX_PERF_LEVEL` of `clk-rcg2.c`.
pub const PERF_LEVELS: usize = 8;
/// `hid_width` 5.
const CFG_DIV_MASK: u32 = 0x1f;
const CFG_SRC_SHIFT: u32 = 8;
const CFG_SRC_MASK: u32 = 7 << CFG_SRC_SHIFT;
const CFG_MODE_SHIFT: u32 = 12;
const CFG_MODE_MASK: u32 = 3 << CFG_MODE_SHIFT;
const CFG_HW_CLK_CTRL: u32 = 1 << 20;
/// `mnd_width` 16.
const MND_MASK: u32 = 0xffff;
/// `F(19200000, P_BI_TCXO, 1, 0, 0)`: source 0, `2 * 1 - 1` in the half-integer divider field, MN off.
const CFG_TCXO_DIV_1: u32 = 1;
/// Polls of `CMD.UPDATE`: at least as long as Linux's 500 x 1 us at some 100 ns per register read.
const UPDATE_POLLS: u32 = 10_000;

/// `gcc_parent_map_1`, with the rates `ftbl_gcc_qupv3_wrap0_s0_clk_src` implies (GPLL6: 51.2 MHz is 384 MHz / 7.5).
fn parent_hz(source: u32) -> Option<u64> {
    match source {
        0 => Some(u64::from(TCXO_HZ)),
        1 => Some(600_000_000),
        2 => Some(384_000_000),
        6 => Some(300_000_000),
        _ => None,
    }
}

/// `calc_rate` of `clk-rcg2.c`, from a value in `CFG`'s format and the raw M and N registers.
fn rate(cfg: u32, m: u32, n: u32) -> Option<u32> {
    let mut hz = parent_hz((cfg & CFG_SRC_MASK) >> CFG_SRC_SHIFT)?;
    let div = cfg & CFG_DIV_MASK;
    if div != 0 {
        hz = hz * 2 / u64::from(div + 1);
    }
    if cfg & CFG_MODE_MASK != 0 {
        let m = m & MND_MASK;
        let n = (!n & MND_MASK) + m;
        if n == 0 {
            return None;
        }
        hz = hz * u64::from(m) / u64::from(n);
    }
    u32::try_from(hz).ok()
}

/// A root clock generator as read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RcgState {
    pub cmd: u32,
    pub cfg: u32,
    /// `SE_CMD_DFS_EN`: hardware takes the rate from `levels`, by the serial engine's `CLK_SEL`.
    pub dfs: bool,
    /// The perf level the RCG reports as current (only meaningful with DFS on).
    pub dfs_level: u8,
    /// The rate `CFG` gives, with DFS off.
    pub cfg_hz: Option<u32>,
    /// The rate of each perf level, with DFS on. `None`: an unknown source.
    pub levels: [Option<u32>; PERF_LEVELS],
}

impl RcgState {
    pub fn read(r: &mut impl Regs, cmd_rcgr: usize) -> Self {
        let cfg = r.read32(cmd_rcgr + CFG);
        let dfsr = r.read32(cmd_rcgr + DFSR);
        let dfs = dfsr & 1 != 0;
        let mut levels = [None; PERF_LEVELS];
        if dfs {
            for (level, hz) in levels.iter_mut().enumerate() {
                *hz = rate(
                    r.read32(cmd_rcgr + perf_cfg(level)),
                    r.read32(cmd_rcgr + perf_m(level)),
                    r.read32(cmd_rcgr + perf_n(level)),
                );
            }
        }
        RcgState {
            cmd: r.read32(cmd_rcgr),
            cfg,
            dfs,
            dfs_level: ((dfsr >> 1) & 0xf) as u8,
            cfg_hz: rate(cfg, r.read32(cmd_rcgr + M), r.read32(cmd_rcgr + N)),
            levels,
        }
    }

    /// The engine's serial clock with this `CLK_SEL`, if it can be known.
    pub fn rate_hz(&self, clk_sel: u32) -> Option<u32> {
        if self.dfs {
            *self.levels.get(clk_sel as usize)?
        } else {
            self.cfg_hz
        }
    }

    /// The `CLK_SEL` that gives exactly `hz`, as Linux's `geni_se_clk_freq_match(exact)` finds it.
    pub fn clk_sel_for(&self, hz: u32) -> Option<u32> {
        if self.dfs {
            self.levels
                .iter()
                .position(|l| *l == Some(hz))
                .map(|l| l as u32)
        } else {
            (self.cfg_hz == Some(hz)).then_some(0)
        }
    }
}

impl fmt::Display for RcgState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CMD {:#x} CFG {:#x}, ", self.cmd, self.cfg)?;
        if self.dfs {
            write!(f, "DFS on, level {}, Hz:", self.dfs_level)?;
            for hz in self.levels {
                match hz {
                    Some(hz) => write!(f, " {hz}")?,
                    None => f.write_str(" ?")?,
                }
            }
            Ok(())
        } else {
            f.write_str("DFS off, ")?;
            match self.cfg_hz {
                Some(hz) => write!(f, "{hz} Hz"),
                None => f.write_str("unknown rate"),
            }
        }
    }
}

/// Starts a stopped root: `ROOT_EN` and `UPDATE`, polling until hardware takes the update. With DFS off it first
/// selects TCXO with divider 1 as `__clk_rcg2_configure` does (only the fields Linux's mask names, MN left off). With
/// DFS on `CFG` is not what the root runs from, so it stays as it is. Returns whether the update finished.
pub fn start_root(r: &mut impl Regs, cmd_rcgr: usize) -> bool {
    if r.read32(cmd_rcgr + DFSR) & 1 == 0 {
        let mask = CFG_DIV_MASK | CFG_SRC_MASK | CFG_MODE_MASK | CFG_HW_CLK_CTRL;
        let cfg = r.read32(cmd_rcgr + CFG);
        r.write32(cmd_rcgr + CFG, (cfg & !mask) | CFG_TCXO_DIV_1);
    }
    let cmd = r.read32(cmd_rcgr);
    r.write32(cmd_rcgr, cmd | CMD_UPDATE | CMD_ROOT_EN);
    (0..UPDATE_POLLS).any(|_| r.read32(cmd_rcgr) & CMD_UPDATE == 0)
}

/// `CBCR_CLK_OFF`: the branch reads as stopped.
const CBCR_CLK_OFF: u32 = 1 << 31;
/// `CBCR_NOC_FSM_STATUS`, and the value that says the branch runs whatever `CLK_OFF` says (`clk_branch2_check_halt`).
const CBCR_FSM_SHIFT: u32 = 28;
const CBCR_FSM_MASK: u32 = 7;
const FSM_STATUS_ON: u32 = 2;
/// Polls of a voted branch: at least as long as Linux's 200 x 1 us at some 100 ns per register read.
const BRANCH_POLLS: u32 = 10_000;

/// A branch's `halt_check` in `gcc-sc7180.c`, of those a voted clock can have.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Halt {
    /// `BRANCH_HALT_VOTED`: poll `CLK_OFF` after enabling.
    Voted,
    /// `BRANCH_HALT_SKIP`: the bit means nothing, do not look.
    Skip,
}

/// A voted branch clock. Addresses are in whatever the [`Regs`] passed use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Branch {
    pub cbcr: usize,
    pub vote: usize,
    pub vote_bit: u32,
    pub halt: Halt,
    /// `hwcg_bit`, for the branches with `hwcg_reg` = their own CBCR: set, hardware gates the clock when idle.
    pub hwcg_bit: Option<u32>,
}

/// What [`enable_branch`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BranchState {
    /// It runs; the vote bit was set first if it was not.
    Running,
    /// It read as off and runs since its vote.
    VotedOn,
    /// In hardware gated mode (or `BRANCH_HALT_SKIP`) `CLK_OFF` says nothing: voted if it read as off, and taken as on,
    /// as `clk_branch_wait` does.
    NotCheckable,
    /// Still off after its vote.
    StaysOff,
}

impl BranchState {
    pub fn is_on(self) -> bool {
        self != BranchState::StaysOff
    }
}

/// `clk_branch2_check_halt` for enabling. All ones is a read that failed, whatever the fields would say.
fn branch_runs(cbcr: u32) -> bool {
    cbcr != u32::MAX
        && (cbcr & CBCR_CLK_OFF == 0 || (cbcr >> CBCR_FSM_SHIFT) & CBCR_FSM_MASK == FSM_STATUS_ON)
}

/// Enables a voted branch as `clk_branch_toggle` and `clk_branch_wait` do. The vote bit is always set first, as
/// `clk_enable_regmap` does, even when the CBCR already reads running; nothing is written when it is already set.
/// Returns what it found, with the CBCR and the vote register as they are afterwards.
pub fn enable_branch(r: &mut impl Regs, branch: &Branch) -> (BranchState, u32, u32) {
    let cbcr = r.read32(branch.cbcr);
    let vote = r.read32(branch.vote);
    if vote & (1 << branch.vote_bit) == 0 {
        r.write32(branch.vote, vote | (1 << branch.vote_bit));
    }
    if branch.halt == Halt::Voted && branch_runs(cbcr) {
        return (BranchState::Running, cbcr, r.read32(branch.vote));
    }
    let gated = branch
        .hwcg_bit
        .is_some_and(|bit| cbcr != u32::MAX && cbcr & (1 << bit) != 0);
    let state = if branch.halt == Halt::Skip || gated {
        BranchState::NotCheckable
    } else if (0..BRANCH_POLLS).any(|_| branch_runs(r.read32(branch.cbcr))) {
        BranchState::VotedOn
    } else {
        BranchState::StaysOff
    };
    (state, r.read32(branch.cbcr), r.read32(branch.vote))
}
