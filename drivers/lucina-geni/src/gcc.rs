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
/// Polls of a branch whose halt check is looked at: at least as long as Linux's 200 x 1 us at some 100 ns per
/// register read (`clk_branch_wait`'s `count = 200`, `:80-86`), and none of them sleeps.
const BRANCH_POLLS: u32 = 10_000;
/// `clk_branch_wait`'s one fixed wait (`:75-76`): `udelay(10)`, for a `BRANCH_HALT_DELAY` in either direction and for
/// clearing a voted branch's bit. Reported to the caller, which owns the clock.
const BRANCH_DELAY_US: u64 = 10;

/// A branch's `halt_check` in `gcc-sc7180.c`, as `clk-branch.h:30-36` spells the values and `clk_branch_wait`
/// (`clk-branch.c:62-92`) acts on them. `gcc-sc7180.c` declares `BRANCH_HALT`, `BRANCH_HALT_VOTED`,
/// `BRANCH_HALT_DELAY` and `BRANCH_HALT_SKIP`; `BRANCH_HALT_ENABLE` is here because the wait polls it like the
/// checked types (`clk-branch.c:77`) with the halt bit read the other way (`:47`), and no sc7180 clock declares it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Halt {
    /// `BRANCH_HALT` (`clk-branch.h:31`, pol: 1 = halt): not voted, so `CLK_OFF` is polled in both directions.
    Plain,
    /// `BRANCH_HALT_VOTED` (`:32`): polled after enabling, a fixed 10 µs after clearing the vote, never both.
    Voted,
    /// `BRANCH_HALT_ENABLE` (`:33`, pol: 0 = halt): polled in both directions with `CLK_OFF` read the other way.
    Enable,
    /// `BRANCH_HALT_DELAY` (`:35`): no bit to check; a fixed 10 µs in either direction.
    Delay,
    /// `BRANCH_HALT_SKIP` (`:36`): the bit means nothing, do not look.
    Skip,
}

impl Halt {
    /// `voted` of `clk_branch_wait` (`:65`): the type's `BRANCH_VOTED` bit.
    fn voted(self) -> bool {
        self == Halt::Voted
    }

    /// `invert` of `clk_branch2_check_halt` (`:47`): `BRANCH_HALT_ENABLE` alone reads the halt bit the other way.
    fn invert(self) -> bool {
        self == Halt::Enable
    }

    /// Whether enabling this type looks at the halt register: `:77-79` polls `BRANCH_HALT`, `BRANCH_HALT_ENABLE`
    /// and a voted clock alike, while `:72-76` answers a skipped or delayed one before looking. On the way down a
    /// voted clock joins the delay instead, which is why [`disable_branch`] does not ask.
    fn polls(self) -> bool {
        matches!(self, Halt::Plain | Halt::Enable | Halt::Voted)
    }

    /// The fixed wait `clk_branch_wait` owes this type (`:75-76`): the 10 µs of a `BRANCH_HALT_DELAY` in either
    /// direction and of a voted clock going down, and nothing else - a skipped or hardware gated branch is answered
    /// before the line is reached (`:72-73`), and a checked one is polled instead. Public because the caller owns
    /// the clock and waits: the helpers report this, they do not sleep.
    pub fn wait_us(self, enabling: bool) -> u64 {
        if self == Halt::Delay || (!enabling && self.voted()) {
            BRANCH_DELAY_US
        } else {
            0
        }
    }
}

/// A branch clock, voted or not. Addresses are in whatever the [`Regs`] passed use, and `vote` is the register
/// Linux's `.enable_reg` names: the shared APCS vote register for a voted branch, its own CBCR for a branch that
/// is not one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Branch {
    pub cbcr: usize,
    pub vote: usize,
    pub vote_bit: u32,
    /// The `halt_check` the branch declares in `gcc-sc7180.c`.
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
    /// `BRANCH_HALT_SKIP`, or hardware gated mode: `clk_branch_wait` returns before it looks at the halt register
    /// (`:72-73`), so the branch is taken as on and `CLK_OFF` says nothing.
    NotCheckable,
    /// `BRANCH_HALT_DELAY`: the same, with Linux's fixed 10 µs owed before anything touches the block (`:75-76`).
    Delayed { wait_us: u64 },
    /// Still off after its vote, which is `clk_branch_wait`'s `WARN` and `-EBUSY` (`:87-89`).
    StaysOff,
}

impl BranchState {
    pub fn is_on(self) -> bool {
        self != BranchState::StaysOff
    }

    /// What the caller still owes before it reaches the block: the 10 µs of a `BRANCH_HALT_DELAY` enable, and
    /// nothing else, since a poll has already been run here and the rest of Linux's answers wait for nothing.
    pub fn wait_us(self) -> u64 {
        match self {
            BranchState::Delayed { wait_us } => wait_us,
            _ => 0,
        }
    }
}

/// `clk_branch2_check_halt` for enabling (`clk-branch.c:54-58`): running is `CLK_OFF` at the level Linux's
/// `invert` puts it (`:56`, `CBCR_CLK_OFF` for `BRANCH_HALT_ENABLE`), or the NOC FSM saying on. All ones is a read
/// that failed, whatever the fields would say.
fn branch_runs(cbcr: u32, invert: bool) -> bool {
    cbcr != u32::MAX
        && (cbcr & CBCR_CLK_OFF == if invert { CBCR_CLK_OFF } else { 0 }
            || (cbcr >> CBCR_FSM_SHIFT) & CBCR_FSM_MASK == FSM_STATUS_ON)
}

/// `clk_branch2_check_halt` for disabling (`clk-branch.c:59`): `CLK_OFF` at the level opposite the enable test, and
/// no FSM - a branch on its way down has no transaction left to gate.
fn branch_halted(cbcr: u32, invert: bool) -> bool {
    cbcr != u32::MAX && cbcr & CBCR_CLK_OFF == if invert { 0 } else { CBCR_CLK_OFF }
}

/// Whether the branch reads as in hardware gated mode, which is `clk_branch_in_hwcg_mode` (`:17-27`) for the
/// branches whose `hwcg_reg` is their own CBCR: there the answer is bit 1 of the value [`enable_branch`] and
/// [`disable_branch`] already have.
fn in_hwcg_mode(branch: &Branch, cbcr: u32) -> bool {
    branch
        .hwcg_bit
        .is_some_and(|bit| cbcr != u32::MAX && cbcr & (1 << bit) != 0)
}

/// Enables a branch as `clk_branch_toggle` and `clk_branch_wait` do. The vote bit is always set first, as
/// `clk_enable_regmap` does (`:100-103`), even when the CBCR already reads running; nothing is written when it is
/// already set. Returns what it found, with the CBCR and the vote register as they are afterwards.
pub fn enable_branch(r: &mut impl Regs, branch: &Branch) -> (BranchState, u32, u32) {
    let cbcr = r.read32(branch.cbcr);
    let vote = r.read32(branch.vote);
    if vote & (1 << branch.vote_bit) == 0 {
        r.write32(branch.vote, vote | (1 << branch.vote_bit));
    }
    // A branch that already reads running is answered as running before `clk_branch_wait`'s first test: the wait
    // returns success without looking for a skipped or hardware gated branch (`:72-73`), and for a check that does
    // look the first poll passes on the value read here (`:82-84`). Both answer on, and what the register says is
    // the more useful half of that answer.
    if branch.halt.polls() && branch_runs(cbcr, branch.halt.invert()) {
        return (BranchState::Running, cbcr, r.read32(branch.vote));
    }
    let state = if branch.halt == Halt::Skip || in_hwcg_mode(branch, cbcr) {
        BranchState::NotCheckable
    } else if branch.halt == Halt::Delay {
        BranchState::Delayed {
            wait_us: BRANCH_DELAY_US,
        }
    } else if (0..BRANCH_POLLS).any(|_| branch_runs(r.read32(branch.cbcr), branch.halt.invert())) {
        BranchState::VotedOn
    } else {
        BranchState::StaysOff
    };
    (state, r.read32(branch.cbcr), r.read32(branch.vote))
}

/// What [`disable_branch`] did, and what it leaves for the caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BranchOff {
    /// The bit was set and is clear now. `wait_us`: what Linux waits after it - the 10 µs of a voted branch
    /// (`clk_branch_wait`'s `!enabling && voted`, `clk-branch.c:75-76`) or of a `BRANCH_HALT_DELAY` one, 0 when the
    /// branch is hardware gated, its halt check skipped (`:72-73`) or its halt check polled instead (`:77-88`).
    /// `stuck`: that poll ran out its reads and `CLK_OFF` never appeared, which is Linux's `WARN` and `-EBUSY`
    /// (`:87-89`) that `clk_branch2_disable` ignores (`:133-136`); false wherever nothing is polled.
    Cleared { wait_us: u64, stuck: bool },
    /// The bit was already clear: nothing was written, as the clock's own refcount never reached this caller.
    WasOff,
}

/// `clk_branch2_disable` for one branch, as `clk_branch_toggle(hw, false)` does it: `clk_disable_regmap` clears
/// only this clock's bits of the vote register, which every other QUP clock shares (`clk-branch.c:104-106`,
/// `clk-regmap.c:72-85`). The wait it earns is reported, not slept: the caller has the clock.
///
/// The caller's bookkeeping is the clock framework's: this writes for a clock whose last user is going, so the vote
/// bit is cleared once, not once per user (Linux's `clk core` counts enables, `drivers/clk/clk.c:1046-1060`).
pub fn disable_branch(r: &mut impl Regs, branch: &Branch) -> (BranchOff, u32, u32) {
    let vote = r.read32(branch.vote);
    if vote & (1 << branch.vote_bit) == 0 {
        return (BranchOff::WasOff, r.read32(branch.cbcr), vote);
    }
    r.write32(branch.vote, vote & !(1 << branch.vote_bit));
    let cbcr = r.read32(branch.cbcr);
    let off = if branch.halt == Halt::Skip || in_hwcg_mode(branch, cbcr) {
        BranchOff::Cleared {
            wait_us: 0,
            stuck: false,
        }
    } else if branch.halt == Halt::Delay || branch.halt.voted() {
        BranchOff::Cleared {
            wait_us: BRANCH_DELAY_US,
            stuck: false,
        }
    } else {
        // A `BRANCH_HALT` or `BRANCH_HALT_ENABLE` check is polled in both directions (`clk-branch.c:77-79`), so on
        // the way down this waits for `CLK_OFF` to appear rather than for it to go, and never delays (`:75` needs
        // `voted`, which neither type has).
        BranchOff::Cleared {
            wait_us: 0,
            stuck: !(0..BRANCH_POLLS)
                .any(|_| branch_halted(r.read32(branch.cbcr), branch.halt.invert())),
        }
    };
    (off, r.read32(branch.cbcr), r.read32(branch.vote))
}
