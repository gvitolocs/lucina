// SPDX-License-Identifier: MPL-2.0

//! The display's clock branches gated for a sleep and back again, without their registers: what the DPU, the DSI host and
//! the MDSS each hold in the Qualcomm clock controllers, in the order Linux's runtime-suspend path lets them go.
//!
//! The Galaxy Book Go's display pipeline is programmed by the firmware, so this kernel holds no clock framework of its own
//! for it: it has no refcounts, only what the enable bits say. What this module adds is exactly those refcounts, as the
//! three device tree consumers name the clocks (`sc7180.dtsi:3172-3175`, `:3202-3209`, `:3275-3286`) and as Linux's bulk
//! helpers walk them (`clk-bulk.c:161-166` and `:211-217` for the disable, `include/linux/clk.h:1162-1174` for the enable).
//! A branch is written off when the last consumer that names it lets go, and written on again when the first one comes
//! back - which is why the MDP core clock, the DPU's "core" and the MDSS's both, goes only with the MDSS, after the DSI
//! host is down: the rule patch 1354 wrote into [`crate::scanout`]'s vote (`dispcc-sc7180.c:501-517`, `msm_mdss.c:485-492`,
//! `:346-359`).
//!
//! One operation per branch, the branch's own enable bit, because that is all of Linux's `clk_disable` and `clk_enable` for
//! these clocks: `clk_disable_regmap` clears `enable_mask` (`clk-regmap.c:72-84`, reached from `clk_branch_disable` →
//! `clk_branch_toggle`, `clk-branch.c:94-109`) and `clk_enable_regmap` sets it (`:48-60`), then `clk_branch_wait` looks at
//! the hardware (`:62-92`). What Linux does *around* that is not this kernel's to do, and is not done here:
//!
//! * the source's parking. Four of the display sources are shared and take `clk_rcg2_shared_ops` - AHB, MDP, ROT and VSYNC
//!   (`dispcc-sc7180.c:157`, `:266`, `:294`, `:308`, on `cmd_rcgr` `0x22bc` `:147`, `0x20c8` `:257`, `0x20e0` `:285`,
//!   `0x20f8` `:299`) - and that ops table's `.disable` is `clk_rcg2_shared_disable` (`clk-rcg2.c:1341`, body
//!   `:1241-1267`): it stores the CFG word (`:1249`), writes the safe source into it (`:1261-1262`) and updates the mux
//!   (`:1264`) under a force enable (`:1259`, `:1266`). The framework reaches it from the branch's own disable, because a
//!   disable runs the clock's ops once its count drops (`clk.c:1207`, `:1212-1213`) and then disables the parent with it
//!   (`:1217`): in Linux the sleep that gates the last child of the MDP source parks that source and lets `disp_cc_pll0`
//!   go behind it. `clk_rcg2_shared_init` calls the same function at registration (`:1333`), which is one more caller and
//!   not the reason the parking exists - it is there so a source that an RCG is left stuck on cannot wedge a GDSC behind it
//!   (`:1321-1331`). **Not done, on purpose:** this kernel writes branch bits only, so the sources stay configured as the
//!   firmware set them and `disp_cc_pll0` runs through the sleep. Nothing is unprepared into a parent that stops, and the
//!   RCG root still gates by its children's hardware feedback (`:1315-1318`), which is why the wedge of `:1321-1331` cannot
//!   happen here. No rate is ever set, so no mux needs re-parking either.
//! * the performance state and the interconnect votes: a device's own, and they go through `lucina-modem-rpmh`'s
//!   per-consumer API at the caller's point, not from here.
//! * the DSI PHY and its regulators: not clock branches, and not this kernel's. It is a *consumer* of one branch, though,
//!   which is what keeps that branch out of every walk: see [`NEVER_GATED`] and the note on `disp_cc_mdss_ahb_clk`.
//!
//! The order of the three consumers is Linux's device tree: children before parent on the way down, parent before children
//! on the way back (`dpu_kms.c:1397-1412`, `dsi_host.c:323-336` and `:500-508`, `msm_mdss.c:485-492`, `:346-359`), each
//! consumer's own clocks in the order its driver walks them. The host and the DPU are devices the MDSS driver populates
//! (`of_platform_populate`, `msm_mdss.c:544`), and a device with an active child is not runtime-idle
//! (`drivers/base/power/runtime.c:269`), which is what puts the MDSS last down and first back.
//!
//! What the kernel walks is [`User::Dpu`] alone. The host's four link clocks belong to its power-off, not to its runtime
//! suspend: `msm_dsi_host_power_off` writes 0 to `DSI_CTRL` first (`dsi_ctrl_disable`, `dsi_host.c:2441` and `:714-717`),
//! puts the lanes to their sleep state (`pinctrl_pm_select_sleep_state`, `:2443`) and only then lets the link clocks go
//! (`:2445`), and the way back is `msm_dsi_host_power_on` (`:2365-2428`), which sets their rates and enables them
//! (`:2394-2396`) before it reprograms the controller: timing (`dsi_timing_setup`, `:2410`), a software reset (`:2411`), the
//! enable (`:2412`). A host this kernel leaves enabled and in video mode is a host Linux would not touch, so its byte,
//! byte_intf, pixel and esc clocks keep running; `msm_dsi_host_xfer_restore` (`:2077-2093`, the disable at `:2091`) is the
//! only other place they go, and it is a power-off path too. The MDSS follows by the rule above: with the host still an
//! active child of it, `msm_mdss_disable` (`:346-359`) is not reached, and `disp_cc_mdss_mdp_clk` - the DPU's "core" and the
//! MDSS's - keeps its rate with it. So the DPU's walk gates `vsync`, `rot` and `lut`, the three branches no other consumer
//! names, and leaves `core` and `bus` to the counts of the two consumers that stay up. [`Gating`] and its walks keep the
//! whole Linux order for the patch that powers the host off and back on the way `:2430-2460` and `:2365-2428` do.

/// A branch's CBCR is in the display clock controller, `clock-controller@af00000` (`sc7180.dtsi:3432-3434`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Controller {
    DispCc,
    /// `clock-controller@100000` (`sc7180.dtsi:790-792`), for the one global branch this path gates.
    Gcc,
}

/// `CBCR_CLK_OFF` (`clk-branch.h:59`): set in a branch's own register once the hardware has gated it, which is what a
/// disable waits for (`clk_branch2_check_halt`'s answer to "not enabling", `clk-branch.c:43-60`, at `:59`).
pub const CLK_OFF: u32 = 1 << 31;
/// `CBCR_NOC_FSM_STATUS` (`clk-branch.h:60`) and `FSM_STATUS_ON` (`:61`, `BIT(1)`): a branch whose NOC finite state machine
/// says on runs whatever `CLK_OFF` says, which is what an enable accepts (`clk-branch.c:54-58`).
pub const FSM_SHIFT: u32 = 28;
pub const FSM_MASK: u32 = 7;
pub const FSM_ON: u32 = 2;
/// `enable_mask` of every branch listed here: `BIT(0)`, at each of its own lines in `dispcc-sc7180.c` and
/// `gcc-sc7180.c`. A branch runs while it is set (`clk_is_enabled_regmap`, `clk-regmap.c:22-37`).
pub const ENABLE: u32 = 1 << 0;
/// How long a branch gets to answer, and how the wait gives the CPU away: Linux's `clk_branch_wait` polls 200 times with
/// `udelay(1)` between the reads, then warns and gives up (`clk-branch.c:80-89`).
pub const WAIT_LIMIT_US: u64 = 200;
pub const POLL_US: u64 = 1;
/// A branch whose `halt_check` carries `BRANCH_VOTED` is not polled when it goes off - it gets 10 µs and that is all
/// (`clk-branch.c:65`, `:75-76`); coming *up* it is polled like any other (`:79`).
pub const VOTED_PAUSE_US: u64 = 10;

/// The DSI host's own registers, for the readback that says the picture came back: `dsi@ae94000`'s `dsi_ctrl` region
/// (`sc7180.dtsi:3269-3270`), 0x400 long. Only their meaning is here: nothing in this module writes them. Linux's
/// `dsi_ctrl_disable` puts 0 in `CTRL` (`dsi_host.c:714-717`), which on this machine would take the firmware's pipeline
/// away.
pub const DSI_CTRL: u32 = 0x000;
/// `DSI_CTRL_ENABLE` and `DSI_CTRL_VID_MODE_EN` (`dsi.xml:94-96`, `pos="0"` and `pos="1"`; the names Linux ORs and clears
/// are at `dsi_host.c:1081-1092`).
pub const CTRL_ENABLE: u32 = 1 << 0;
pub const CTRL_VID_MODE_EN: u32 = 1 << 1;
/// `DSI_STATUS0` (`dsi.xml:107`) with `VIDEO_MODE_ENGINE_BUSY` (`:111`, `pos="3"`), the bit Linux reads to see whether the
/// video engine is still moving pixels (`dsi_host.c:1137-1144`).
pub const DSI_STATUS0: u32 = 0x004;
pub const STATUS0_VID_BUSY: u32 = 1 << 3;

/// One branch clock: its name as Linux gives it, the controller and register its CBCR sits in, and whether its
/// `halt_check` is `BRANCH_HALT_VOTED` (`clk-branch.h:31-36`). `enable_reg` and `halt_reg` are the same word for every
/// branch listed, so one address is enough.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Branch {
    pub name: &'static str,
    pub at: Controller,
    pub reg: u32,
    pub voted: bool,
}

pub const MDP: usize = 0;
/// The DPU's "iface", the DSI host's "iface" and the MDSS's "ahb" - and the DSI PHY's "iface" too, which is why it is in
/// [`NEVER_GATED`].
pub const AHB: usize = 1;
pub const ROT: usize = 2;
pub const LUT: usize = 3;
pub const VSYNC: usize = 4;
pub const BYTE: usize = 5;
pub const BYTE_INTF: usize = 6;
pub const PIXEL: usize = 7;
pub const ESC: usize = 8;
pub const BUS: usize = 9;
/// The MDSS's "iface", which Linux never gates: see [`NEVER_GATED`].
pub const MDSS_IFACE: usize = 10;

/// Every branch the display path can name, each with the lines it comes from. They are all `clk_branch2_ops`
/// (`clk-branch.h:108`), and none has a `hwcg_reg`, so `clk_branch_in_hwcg_mode` is false and the wait happens
/// (`clk-branch.c:17-27`, `:72-73`).
pub const BRANCHES: [Branch; 11] = [
    // `disp_cc_mdss_mdp_clk`: dispcc+0x200c bit 0, `BRANCH_HALT` (`dispcc-sc7180.c:501-517`). The DPU's "core"
    // (`sc7180.dtsi:3206`) and the MDSS's (`:3174`), from `disp_cc_mdss_mdp_clk_src` (`:256-268`).
    Branch {
        name: "disp_cc_mdss_mdp_clk",
        at: Controller::DispCc,
        reg: 0x200c,
        voted: false,
    },
    // `disp_cc_mdss_ahb_clk`: dispcc+0x2080 bit 0, `BRANCH_HALT` (`:312-328`). The DPU's "iface" (`sc7180.dtsi:3203`), the
    // MDSS's "ahb" (`:3173`), the DSI host's "iface" (`:3279`) and the DSI PHY's "iface" (`:3351-3353`): the register path of
    // the whole block, and a branch no walk here carries. The PHY holds it as long as it is powered, which this kernel never
    // changes - see [`NEVER_GATED`].
    Branch {
        name: "disp_cc_mdss_ahb_clk",
        at: Controller::DispCc,
        reg: 0x2080,
        voted: false,
    },
    // `disp_cc_mdss_rot_clk`: dispcc+0x2014 bit 0, `BRANCH_HALT` (`:572-588`). The DPU's "rot" (`sc7180.dtsi:3204`).
    Branch {
        name: "disp_cc_mdss_rot_clk",
        at: Controller::DispCc,
        reg: 0x2014,
        voted: false,
    },
    // `disp_cc_mdss_mdp_lut_clk`: dispcc+0x201c bit 0, `BRANCH_VOTED` (`:519-534`). The DPU's "lut" (`sc7180.dtsi:3205`).
    Branch {
        name: "disp_cc_mdss_mdp_lut_clk",
        at: Controller::DispCc,
        reg: 0x201c,
        voted: true,
    },
    // `disp_cc_mdss_vsync_clk`: dispcc+0x2024 bit 0, `BRANCH_HALT` (`:608-624`). The DPU's "vsync" (`sc7180.dtsi:3207`).
    Branch {
        name: "disp_cc_mdss_vsync_clk",
        at: Controller::DispCc,
        reg: 0x2024,
        voted: false,
    },
    // `disp_cc_mdss_byte0_clk`: dispcc+0x2028 bit 0, `BRANCH_HALT` (`:330-346`). The DSI host's "byte" (`:3275`).
    Branch {
        name: "disp_cc_mdss_byte0_clk",
        at: Controller::DispCc,
        reg: 0x2028,
        voted: false,
    },
    // `disp_cc_mdss_byte0_intf_clk`: dispcc+0x202c bit 0, `BRANCH_HALT` (`:376-392`). The DSI host's "byte_intf" (`:3276`).
    Branch {
        name: "disp_cc_mdss_byte0_intf_clk",
        at: Controller::DispCc,
        reg: 0x202c,
        voted: false,
    },
    // `disp_cc_mdss_pclk0_clk`: dispcc+0x2004 bit 0, `BRANCH_HALT` (`:554-570`). The DSI host's "pixel" (`:3277`).
    Branch {
        name: "disp_cc_mdss_pclk0_clk",
        at: Controller::DispCc,
        reg: 0x2004,
        voted: false,
    },
    // `disp_cc_mdss_esc0_clk`: dispcc+0x2038 bit 0, `BRANCH_HALT` (`:483-499`). The DSI host's "core", which is its escape
    // clock (`:3278`, and `dsi_host.c:308-315` takes the name "core" into `esc_clk`).
    Branch {
        name: "disp_cc_mdss_esc0_clk",
        at: Controller::DispCc,
        reg: 0x2038,
        voted: false,
    },
    // `gcc_disp_hf_axi_clk`: gcc+0xb024 bit 0, `BRANCH_HALT` (`gcc-sc7180.c:1043-1054`). The DPU's "bus"
    // (`sc7180.dtsi:3202`) and the DSI host's ("bus", `:3280`): the display's path through the NOC.
    Branch {
        name: "gcc_disp_hf_axi_clk",
        at: Controller::Gcc,
        reg: 0x0b024,
        voted: false,
    },
    // `gcc_disp_ahb_clk`: gcc+0x0b00c. The MDSS's "iface" (`sc7180.dtsi:3172`), and not a branch this kernel may gate:
    // the GCC driver switches it on by hand with `qcom_branch_set_clk_en` (`clk-branch.h:102-105`) among the clocks that
    // stay on ("Keep some clocks always-on", `gcc-sc7180.c:2446-2454`, the line for this one at `:2450`), has no `clk_branch`
    // for it at all and no entry in its registered clocks (`:2246-2372`): the provider answers that index with a null hw
    // (`qcom_cc_clk_hw_get`, `common.c:251`), and a null clock prepares and disables as 0 (`clk.c:1186-1187`,
    // `:1408-1409`), so the MDSS's bulk disable has no gate to turn for it. It is listed for its address and is in none of
    // the walks below.
    Branch {
        name: "gcc_disp_ahb_clk",
        at: Controller::Gcc,
        reg: 0x0b00c,
        voted: false,
    },
];

/// How many branches the table holds, and so how many counters [`Gating`] keeps.
pub const COUNT: usize = BRANCHES.len();

/// The branches a display device names and no walk of this patch turns off.
///
/// [`MDSS_IFACE`] because Linux never had a gate for it: the GCC driver switches it on by hand and registers no clock the
/// framework could disable, so its bulk disable has nothing to write (the note on it in [`BRANCHES`]).
///
/// [`AHB`] because a consumer of it stays up while this kernel sleeps: the DSI PHY names `DISP_CC_MDSS_AHB_CLK` as its
/// "iface" (`sc7180.dtsi:3351-3353`, the clock the PHY driver takes at `msm_clk_get(pdev, "iface")`, `dsi_phy.c:693`) and
/// holds it from `dsi_phy_enable_resource`'s `clk_prepare_enable` (`dsi_phy.c:523`, reached from `msm_dsi_phy_enable`
/// `:759`) until `dsi_phy_disable_resource`'s `clk_disable_unprepare` (`:534`, reached from `msm_dsi_phy_disable` `:818`).
/// This kernel leaves the PHY exactly as the firmware left it, powered and feeding the panel's bridge - an enabled PHY in
/// Linux's terms - so Linux would hold that count and never gate the branch. A third consumer, the DP controller
/// (`sc7180.dtsi:3358`, its `DISP_CC_MDSS_AHB_CLK` at `:3371`), is another reason: `status = "disabled"` in the SoC file
/// (`:3360`), and a board that enables DP is a board this walk would stop the register path of.
pub const NEVER_GATED: [usize; 2] = [MDSS_IFACE, AHB];

/// Which device names which clocks, and in the order its driver walks them. All three are counted by [`Gating::new`]; of
/// the three walks, the kernel of patch 1357 calls [`User::Dpu`] only - see the note above.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum User {
    /// `qcom,sc7180-dpu`, `display-controller@ae01000`: `dpu_runtime_suspend`'s bulk disable (`dpu_kms.c:1406`) of
    /// `clocks = bus, iface, rot, lut, core, vsync` (`sc7180.dtsi:3202-3209`), and `dpu_runtime_resume`'s bulk enable of
    /// the same list (`:1425`). The iface entry is [`NEVER_GATED`], so the walk carries five.
    Dpu,
    /// `qcom,sc7180-dsi-ctrl`, `dsi@ae94000`: the four link clocks of a host that is being powered off
    /// (`dsi_link_clk_disable_6g`, `dsi_host.c:504-507`, called from `msm_dsi_host_power_off` `:2445` and
    /// `msm_dsi_host_xfer_restore` `:2091`; the runtime suspend `:323-336` drops `iface, bus` alone, at `:333`); back,
    /// `msm_dsi_runtime_resume`'s bulk enable (`:348`) then the link clocks (`:388-411`). The iface entry is
    /// [`NEVER_GATED`] too.
    Dsi,
    /// `qcom,sc7180-mdss`, `display-subsystem@ae00000`: `msm_mdss_disable`'s bulk disable of `iface, ahb, core`
    /// (`msm_mdss.c:350`, `sc7180.dtsi:3172-3175`) and `msm_mdss_enable`'s bulk enable (`:298`). Both the iface and the ahb
    /// entry are [`NEVER_GATED`], so the walk carries the core clock alone.
    Mdss,
}

impl User {
    /// The branches this device lets go, in that order. `clk_bulk_disable_unprepare` disables from the end of the list and
    /// unprepares the same way back (`clk-bulk.c:211-217`, `:161-166`, `include/linux/clk.h:1176-1181`).
    pub fn down_order(self) -> &'static [usize] {
        match self {
            // `bus, iface, rot, lut, core, vsync` from the end: vsync, core, lut, rot, (iface), bus.
            User::Dpu => &[VSYNC, MDP, LUT, ROT, BUS],
            // `dsi_link_clk_disable_6g`: esc, pixel, byte_intf, byte (`dsi_host.c:504-507`); then the bus clocks from the
            // end of `iface, bus`: bus, (iface) (`dsi_host.c:333`).
            User::Dsi => &[ESC, PIXEL, BYTE_INTF, BYTE, BUS],
            // `iface, ahb, core` from the end, both of the first two never gated: core.
            User::Mdss => &[MDP],
        }
    }

    /// The branches this device takes back, in that order: `clk_bulk_prepare_enable` walks its list forwards
    /// (`include/linux/clk.h:1162-1174`), and so does `dsi_link_clk_enable_6g` (`dsi_host.c:388-411`).
    pub fn up_order(self) -> &'static [usize] {
        match self {
            User::Dpu => &[BUS, ROT, LUT, MDP, VSYNC],
            // The bus clock (`dsi_host.c:348`), then the link clocks (`:388-411`).
            User::Dsi => &[BUS, ESC, BYTE, PIXEL, BYTE_INTF],
            User::Mdss => &[MDP],
        }
    }

    /// The three devices in the order their clocks go: children before parent, which is Linux's device tree - `dsi.c:204`
    /// and `dpu_kms.c:1441` force the child devices' runtime suspend, and the MDSS's own `mdss_pm_suspend` does the same
    /// work for itself last (`msm_mdss.c:503-510`).
    pub fn down_all() -> &'static [User] {
        &[User::Dpu, User::Dsi, User::Mdss]
    }

    /// Back: the parent before its children, the same list reversed.
    pub fn up_all() -> &'static [User] {
        &[User::Mdss, User::Dsi, User::Dpu]
    }
}

/// A branch's register, read and written as a word. A read that cannot happen is all ones, which is a clock that neither
/// runs nor stops: the wait then says "stuck" and Linux warns (`clk-branch.c:87-89`).
pub trait Words {
    fn read(&mut self, at: Controller, offset: u32) -> u32;
    fn write(&mut self, at: Controller, offset: u32, value: u32);
}

/// Time, so that a test counts the polls of a wait instead of sitting through them: the same seam the scanout's frame wait
/// uses ([`crate::scanout::Pace]).
pub use crate::scanout::Pace;

/// What one device's step did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Step {
    /// The step had nothing to do: this device was already down (`down`) or already up (`up`), the way Linux's
    /// `mdss_pm_suspend` returns at once when its device is runtime-suspended already (`msm_mdss.c:503-510`).
    pub nothing: bool,
    /// Branches written off, or on.
    pub wrote: u8,
    /// Branches another consumer still holds, so their bit was left as it was.
    pub shared: u8,
    /// Branches found in the state wanted already: nothing written for them.
    pub already: u8,
    /// The branch that did not answer its wait, so it is not known to be in the state asked for. Linux warns and, for a
    /// disable, goes on regardless (`clk-branch.c:87-89`; `clk_branch_disable` returns nothing, `:116-119`); the caller
    /// keeps the vote that this state was a condition for.
    pub stuck: Option<&'static str>,
}

impl Step {
    const NOTHING: Step = Step {
        nothing: true,
        wrote: 0,
        shared: 0,
        already: 0,
        stuck: None,
    };
}

/// The three devices' clock lists, with the refcounts and the state each device is in.
pub struct Gating<W: Words, P: Pace> {
    words: W,
    pace: P,
    /// How many of the three consumers hold each branch.
    refs: [u8; COUNT],
    /// Whether the firmware left it running when this started, the only reason a branch may be written at all.
    boot: [bool; COUNT],
    /// Whether the branch is known gated, the state the votes are judged against: see [`Gating::is_gated`].
    gated: [bool; COUNT],
    down: [bool; 3],
}

impl<W: Words, P: Pace> Gating<W, P> {
    /// Read every branch once, and count each consumer's clocks from what the firmware left running. A branch that reads
    /// off is held by no one: not gated on the way down, not enabled on the way back.
    pub fn new(mut words: W, pace: P) -> Self {
        let mut boot = [false; COUNT];
        let mut gated = [false; COUNT];
        for (index, branch) in BRANCHES.iter().enumerate() {
            let word = words.read(branch.at, branch.reg);
            boot[index] = running(word);
            // A branch whose enable bit reads clear is off and held by no one. All ones is a read that did not answer,
            // which says nothing: such a clock keeps its vote.
            gated[index] = word != u32::MAX && word & ENABLE == 0;
        }
        let mut refs = [0u8; COUNT];
        for user in User::down_all() {
            for &clk in user.up_order() {
                if boot[clk] {
                    refs[clk] += 1;
                }
            }
        }
        Self {
            words,
            pace,
            refs,
            boot,
            gated,
            down: [false; 3],
        }
    }

    /// Whether this branch is known to be gated: it was turned off and the hardware said so, or it was found off and no
    /// one wrote it since. A wait that never answered is NOT gated, and neither is a read that failed — which is the
    /// condition 1354 set: no rpmh clock vote is let go while the clock it is for may still run.
    pub fn is_gated(&self, branch: usize) -> bool {
        self.gated[branch]
    }

    pub fn is_down(&self, user: User) -> bool {
        self.down[index(user)]
    }

    /// Let this device's clocks go, in Linux's order, gating a branch only when this was its last consumer.
    pub fn down(&mut self, user: User) -> Step {
        if self.is_down(user) {
            return Step::NOTHING;
        }
        self.down[index(user)] = true;
        let mut step = Step {
            nothing: false,
            ..Step::NOTHING
        };
        for &clk in user.down_order() {
            let b = BRANCHES[clk];
            if !self.boot[clk] {
                step.already += 1;
                continue;
            }
            if self.refs[clk] > 1 {
                // Another consumer of the same clock is still up: the bit stays set, which is what Linux's refcount does.
                self.refs[clk] -= 1;
                self.gated[clk] = false;
                step.shared += 1;
                continue;
            }
            self.refs[clk] = 0;
            let word = self.words.read(b.at, b.reg);
            if word & ENABLE == 0 {
                step.already += 1;
                self.gated[clk] = word != u32::MAX;
                continue;
            }
            // `clk_disable_regmap` (`clk-regmap.c:72-84`), then `clk_branch_wait` (`clk-branch.c:62-92`).
            self.words.write(b.at, b.reg, word & !ENABLE);
            step.wrote += 1;
            let answered = self.wait(b, false);
            if !answered {
                step.stuck = Some(b.name);
            }
            self.gated[clk] = answered;
        }
        step
    }

    /// Take this device's clocks back, enabling a branch as soon as it has a consumer again.
    pub fn up(&mut self, user: User) -> Step {
        if !self.is_down(user) {
            return Step::NOTHING;
        }
        self.down[index(user)] = false;
        let mut step = Step {
            nothing: false,
            ..Step::NOTHING
        };
        for &clk in user.up_order() {
            let b = BRANCHES[clk];
            if !self.boot[clk] {
                step.already += 1;
                continue;
            }
            self.refs[clk] += 1;
            self.gated[clk] = false;
            let word = self.words.read(b.at, b.reg);
            if word & ENABLE != 0 {
                // Already running: `clk_enable_regmap` writes nothing when the bit is set (`clk-regmap.c:48-60`).
                step.already += 1;
                continue;
            }
            self.words.write(b.at, b.reg, word | ENABLE);
            step.wrote += 1;
            if !self.wait(b, true) {
                step.stuck = Some(b.name);
            }
        }
        step
    }

    /// `clk_branch_wait` for one branch: a voted branch going off gets its 10 µs and no look
    /// (`clk-branch.c:65`, `:75-76`); anything else is polled until the hardware answers, 200 µs of it (`:77-89`).
    fn wait(&mut self, branch: Branch, enabling: bool) -> bool {
        if branch.voted && !enabling {
            self.pace.rest(VOTED_PAUSE_US);
            return true;
        }
        let started = self.pace.now_us();
        loop {
            let word = self.words.read(branch.at, branch.reg);
            if answering(word, enabling) {
                return true;
            }
            if self.pace.now_us() - started >= WAIT_LIMIT_US {
                return false;
            }
            self.pace.rest(POLL_US);
        }
    }
}

/// A branch that reads running: its enable bit set with its halt flag clear, or its NOC machine saying on whatever
/// `CLK_OFF` says (`clk_branch2_check_halt` for enabling, `clk-branch.c:54-58`; `clk_is_enabled_regmap` on the bit alone,
/// `clk-regmap.c:22-37`). All ones is a read that failed, and is not running.
fn running(word: u32) -> bool {
    word != u32::MAX
        && word & ENABLE != 0
        && (word & CLK_OFF == 0 || (word >> FSM_SHIFT) & FSM_MASK == FSM_ON)
}

/// What the wait looks for: `CLK_OFF` set for a disable, the clock running for an enable (`clk-branch.c:43-60`).
fn answering(word: u32, enabling: bool) -> bool {
    if word == u32::MAX {
        return false;
    }
    if enabling {
        word & ENABLE != 0 && running(word)
    } else {
        word & CLK_OFF != 0
    }
}

/// Whether the DSI host still streams a picture: its control register enabled and in video mode, with the status saying
/// the video engine is busy (`dsi.xml:94-114`, read the way `dsi_host.c:1137-1144` reads it).
pub fn dsi_streaming(ctrl: u32, status0: u32) -> bool {
    ctrl != u32::MAX
        && ctrl & CTRL_ENABLE != 0
        && ctrl & CTRL_VID_MODE_EN != 0
        && status0 & STATUS0_VID_BUSY != 0
}

/// The name a branch is logged under.
pub fn name(branch: usize) -> &'static str {
    BRANCHES[branch].name
}

/// The branch of a controller register, for a read that comes back by address.
pub fn branch_at(at: Controller, reg: u32) -> Option<usize> {
    BRANCHES.iter().position(|b| b.at == at && b.reg == reg)
}

fn index(user: User) -> usize {
    match user {
        User::Dpu => 0,
        User::Dsi => 1,
        User::Mdss => 2,
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use std::vec::Vec;

    use super::*;

    /// The two controller pages, as the hardware answers. Every branch gates straight away - its `CLK_OFF` follows the
    /// enable bit - except the ones in `stuck`, which never report `CLK_OFF` at all.
    struct Hw {
        enable: [bool; COUNT],
        stuck: Vec<usize>,
        writes: Vec<(usize, u32)>,
    }

    impl Hw {
        /// Every branch running, which is what the firmware leaves on a machine whose screen is lit.
        fn lit() -> Self {
            Self {
                enable: [true; COUNT],
                stuck: Vec::new(),
                writes: Vec::new(),
            }
        }

        /// A branch whose halt flag never answers a disable.
        fn never_stopping(mut self, branch: usize) -> Self {
            self.stuck.push(branch);
            self
        }

        /// A branch the firmware did not enable.
        fn left_off(mut self, branch: usize) -> Self {
            self.enable[branch] = false;
            self
        }

        fn gating(self) -> Gating<Self, Time> {
            Gating::new(self, Time::default())
        }

        /// The steps this model was written to, as (branch, value).
        fn written(&self, branch: usize) -> Vec<u32> {
            self.writes
                .iter()
                .filter(|w| w.0 == branch)
                .map(|w| w.1)
                .collect()
        }

        fn walk(&self) -> Vec<usize> {
            self.writes.iter().map(|w| w.0).collect()
        }
    }

    impl Words for Hw {
        fn read(&mut self, at: Controller, offset: u32) -> u32 {
            let branch = branch_at(at, offset).expect("a read outside the branches");
            let enable = self.enable[branch];
            // `CLK_OFF` is read-only hardware: a stuck branch keeps saying the clock runs whatever its enable bit
            // reads now.
            let stopped = !enable && !self.stuck.contains(&branch);
            u32::from(enable) | if stopped { CLK_OFF } else { 0 }
        }

        fn write(&mut self, at: Controller, offset: u32, value: u32) {
            let branch = branch_at(at, offset).expect("a write outside the branches");
            self.writes.push((branch, value));
            self.enable[branch] = value & ENABLE != 0;
        }
    }

    /// A clock that only moves when the wait does, so a wait is counted in rests taken.
    #[derive(Default)]
    struct Time {
        now: u64,
        rests: Vec<u64>,
    }

    impl Pace for Time {
        fn now_us(&mut self) -> u64 {
            self.now
        }

        fn rest(&mut self, us: u64) {
            self.rests.push(us);
            self.now += us;
        }
    }

    impl Time {
        /// How many times a wait looked at the hardware, not counting the pause a voted branch gets instead.
        fn polls(&self) -> usize {
            self.rests.iter().filter(|&&us| us == POLL_US).count()
        }
    }

    /// Every branch off, in the order the three walks wrote them, with nothing left running.
    fn all_the_way_down(g: &mut Gating<Hw, Time>) {
        for user in User::down_all() {
            g.down(*user);
        }
    }

    #[test]
    fn a_shared_clock_goes_only_with_its_last_consumer() {
        let mut g = Hw::lit().gating();
        // The MDP core clock is the DPU's and the MDSS's; the bus clock is the DPU's and the DSI host's.
        g.down(User::Dpu);
        assert!(!g.is_gated(MDP), "the MDSS still names the MDP clock");
        assert!(!g.is_gated(BUS), "the DSI host still names the bus clock");
        g.down(User::Dsi);
        assert!(!g.is_gated(MDP), "the MDSS still names it");
        assert!(g.is_gated(BUS), "the host was its last consumer");
        let step = g.down(User::Mdss);
        assert!(g.is_gated(MDP), "the last consumer of it is the MDSS");
        assert!(g.is_gated(ESC));
        assert_eq!(step.shared, 0, "by now nothing is held by another consumer");
    }

    #[test]
    fn the_ahb_clock_is_in_no_walk_at_all() {
        // A fourth consumer of `disp_cc_mdss_ahb_clk` - the DSI PHY (`sc7180.dtsi:3351-3353`, `dsi_phy.c:693`, held from
        // `:523` to `:534`) - stays powered through this kernel's sleep, so no consumer of ours may gate the branch and
        // none is even asked to. Same for the MDSS's iface, which Linux has no gate for (`common.c:251`).
        for user in User::down_all() {
            for branch in NEVER_GATED {
                assert!(
                    !user.down_order().contains(&branch),
                    "{} down",
                    name(branch)
                );
                assert!(!user.up_order().contains(&branch), "{} up", name(branch));
            }
        }
        assert_eq!(NEVER_GATED, [MDSS_IFACE, AHB]);
        let mut g = Hw::lit().gating();
        all_the_way_down(&mut g);
        for user in User::up_all() {
            g.up(*user);
        }
        for branch in NEVER_GATED {
            assert!(g.words.written(branch).is_empty(), "{}", name(branch));
            assert!(!g.is_gated(branch), "{} stays running", name(branch));
        }
    }

    #[test]
    fn the_dpu_walk_alone_gates_only_what_no_other_consumer_names() {
        // What the kernel of 1357 does: the host and the MDSS stay up, so the DPU's walk leaves their clocks alone.
        let mut g = Hw::lit().gating();
        let step = g.down(User::Dpu);
        assert_eq!(g.words.walk(), [VSYNC, LUT, ROT]);
        assert_eq!(step.wrote, 3);
        assert_eq!(step.shared, 2);
        assert!(
            !g.is_gated(MDP),
            "the MDSS's count holds it, so 1354's vote stays"
        );
        assert!(!g.is_gated(BUS), "the host's count holds it");
        g.words.writes.clear();
        let back = g.up(User::Dpu);
        assert_eq!(g.words.walk(), [ROT, LUT, VSYNC]);
        assert_eq!(back.wrote, 3);
        assert_eq!(back.already, 2, "the two shared clocks never went");
        for branch in 0..COUNT {
            assert!(!g.is_gated(branch), "{} is running again", name(branch));
        }
    }

    #[test]
    fn the_down_walk_is_linux_s() {
        let mut g = Hw::lit().gating();
        all_the_way_down(&mut g);
        // DPU: vsync, (mdp shared), lut, rot, (bus shared). DSI: esc, pixel, byte_intf, byte, then bus. MDSS: mdp.
        // The iface and the ahb branch are in no walk (`NEVER_GATED`). Every write is a clear of the enable bit.
        assert_eq!(
            g.words.walk(),
            [VSYNC, LUT, ROT, ESC, PIXEL, BYTE_INTF, BYTE, BUS, MDP]
        );
        assert!(g.words.writes.iter().all(|w| w.1 & ENABLE == 0));
        for branch in 0..COUNT {
            if NEVER_GATED.contains(&branch) {
                assert!(
                    g.words.enable[branch],
                    "{} stays on, as Linux leaves it",
                    name(branch)
                );
                assert!(g.words.written(branch).is_empty());
                assert!(!g.is_gated(branch), "{}", name(branch));
            } else {
                assert!(!g.words.enable[branch], "{}", name(branch));
                assert!(g.is_gated(branch), "{}", name(branch));
            }
        }
    }

    #[test]
    fn the_way_up_is_the_same_walk_backwards() {
        let mut g = Hw::lit().gating();
        all_the_way_down(&mut g);
        g.words.writes.clear();
        for user in User::up_all() {
            g.up(*user);
        }
        // The MDSS takes its core clock back; the DSI host then its bus clock and the link clocks; the DPU only the
        // clocks nobody else had brought back. Shared clocks are enabled once, by whoever gets there first.
        assert_eq!(
            g.words.walk(),
            [MDP, BUS, ESC, BYTE, PIXEL, BYTE_INTF, ROT, LUT, VSYNC]
        );
        assert!(g.words.writes.iter().all(|w| w.1 & ENABLE != 0));
        for branch in 0..COUNT {
            assert!(g.words.enable[branch], "{}", name(branch));
            assert!(!g.is_gated(branch), "{}", name(branch));
        }
    }

    #[test]
    fn a_branch_that_does_not_answer_is_named_and_the_rest_goes_on() {
        let mut g = Hw::lit().never_stopping(MDP).gating();
        g.down(User::Dpu);
        g.down(User::Dsi);
        let step = g.down(User::Mdss);
        // Linux warns and carries on with the disable (`clk-branch.c:87-89`, `clk_branch_disable` returns nothing).
        assert_eq!(step.stuck, Some("disp_cc_mdss_mdp_clk"));
        // Not proven off, so the vote that waits for it is not let go: this is what `mdp_clock_on` reads.
        assert!(!g.is_gated(MDP), "the hardware never said it stopped");
        assert!(
            g.is_gated(ROT),
            "the branches that went before it are gated"
        );
        assert!(g.is_gated(BUS));
        // The wait is the 200 µs of it, polled once a microsecond.
        assert_eq!(g.pace.polls() as u64, WAIT_LIMIT_US / POLL_US);
    }

    #[test]
    fn a_voted_branch_that_goes_is_paused_not_polled() {
        let mut g = Hw::lit().gating();
        g.down(User::Dpu);
        // `disp_cc_mdss_mdp_lut_clk` is the only voted branch (`dispcc-sc7180.c:521`); the others answer the first read of
        // the wait, which costs no rest at all.
        assert_eq!(g.pace.rests, [VOTED_PAUSE_US]);
        assert!(g.is_gated(LUT));
        // Coming back it is polled like any other (`clk-branch.c:79`), so the pause is not repeated.
        g.words.writes.clear();
        g.pace.rests.clear();
        g.up(User::Dpu);
        assert!(g.pace.rests.is_empty());
        assert!(!g.is_gated(LUT));
    }

    #[test]
    fn a_device_already_down_is_written_to_once() {
        let mut g = Hw::lit().gating();
        let first = g.down(User::Dsi);
        let again = g.down(User::Dsi);
        assert!(again.nothing);
        // esc, pixel, byte_intf, byte and the shared bus clock; the host's iface is the DPU's and the MDSS's and its
        // ahb branch is in no walk at all.
        assert_eq!(first.wrote, 4);
        assert_eq!(first.shared, 1);
        let writes = g.words.writes.len();
        assert!(g.up(User::Dpu).nothing, "the DPU never went down here");
        let back = g.up(User::Dsi);
        assert_eq!(back.wrote, 4);
        assert_eq!(back.already, 1, "the bus clock never went");
        assert_eq!(g.words.writes.len(), writes + 4);
    }

    #[test]
    fn a_branch_the_firmware_left_off_is_never_touched() {
        let mut g = Hw::lit().left_off(BYTE).gating();
        all_the_way_down(&mut g);
        assert!(
            g.words.written(BYTE).is_empty(),
            "nothing to gate that was never on"
        );
        for user in User::up_all() {
            g.up(*user);
        }
        assert!(g.words.written(BYTE).is_empty(), "nor to enable");
        assert!(!g.words.enable[BYTE]);
        assert!(g.is_gated(BYTE), "found off, and off it stays");
    }

    #[test]
    fn every_write_is_to_a_branch_a_device_names() {
        let allowed: Vec<usize> = User::down_all()
            .iter()
            .flat_map(|u| u.down_order().iter().copied())
            .collect();
        let mut g = Hw::lit().gating();
        all_the_way_down(&mut g);
        for user in User::up_all() {
            g.up(*user);
        }
        for (branch, _) in &g.words.writes {
            assert!(
                allowed.contains(branch),
                "{} is not a clock of these three",
                name(*branch)
            );
        }
        for branch in NEVER_GATED {
            assert!(!allowed.contains(&branch), "{}", name(branch));
            assert!(g.words.written(branch).is_empty(), "{}", name(branch));
        }
    }

    #[test]
    fn the_halt_answer_is_the_same_test_linux_uses() {
        // `clk_branch2_check_halt`: running with `CLK_OFF` clear, or with the NOC machine on whatever `CLK_OFF` says.
        assert!(running(ENABLE));
        assert!(running(ENABLE | (FSM_ON << FSM_SHIFT)));
        assert!(running(ENABLE | (FSM_ON << FSM_SHIFT) | CLK_OFF));
        assert!(!running(ENABLE | CLK_OFF));
        assert!(!running(0));
        assert!(!running(u32::MAX), "a failed read is not a clock that runs");
        // A disable is answered by `CLK_OFF` alone; an enable needs the bit it just wrote.
        assert!(answering(ENABLE | CLK_OFF, false));
        assert!(answering(CLK_OFF, false));
        assert!(!answering(ENABLE, false));
        assert!(answering(ENABLE, true));
        assert!(!answering(ENABLE | CLK_OFF, true));
    }

    #[test]
    fn nothing_is_written_twice_to_the_same_register() {
        // One cycle: each branch the walks carry is cleared once and set once, and the two they never carry never.
        let mut g = Hw::lit().gating();
        all_the_way_down(&mut g);
        for user in User::up_all() {
            g.up(*user);
        }
        for branch in 0..COUNT {
            let writes = g.words.written(branch);
            if NEVER_GATED.contains(&branch) {
                assert!(writes.is_empty(), "{}", name(branch));
            } else {
                assert_eq!(writes.len(), 2, "{}: off then on", name(branch));
                assert_eq!(writes[0] & ENABLE, 0);
                assert_ne!(writes[1] & ENABLE, 0);
            }
        }
    }

    #[test]
    fn the_dsi_readback_says_whether_a_picture_is_moving() {
        assert!(dsi_streaming(
            CTRL_ENABLE | CTRL_VID_MODE_EN,
            STATUS0_VID_BUSY
        ));
        assert!(
            !dsi_streaming(CTRL_ENABLE | CTRL_VID_MODE_EN, 0),
            "enabled but nothing scanned out"
        );
        assert!(
            !dsi_streaming(CTRL_ENABLE, STATUS0_VID_BUSY),
            "not in video mode"
        );
        assert!(
            !dsi_streaming(u32::MAX, u32::MAX),
            "a block that does not answer"
        );
    }

    /// The registers the walks are allowed to touch, as the branch table gives them.
    #[test]
    fn the_branch_table_is_the_addresses_linux_gives() {
        let at = |branch: usize, controller: Controller, reg: u32, voted: bool| {
            let b = BRANCHES[branch];
            assert_eq!(
                (b.at, b.reg, b.voted),
                (controller, reg, voted),
                "{}",
                b.name
            );
        };
        use Controller::{DispCc, Gcc};
        at(MDP, DispCc, 0x200c, false);
        at(AHB, DispCc, 0x2080, false);
        at(ROT, DispCc, 0x2014, false);
        at(LUT, DispCc, 0x201c, true);
        at(VSYNC, DispCc, 0x2024, false);
        at(BYTE, DispCc, 0x2028, false);
        at(BYTE_INTF, DispCc, 0x202c, false);
        at(PIXEL, DispCc, 0x2004, false);
        at(ESC, DispCc, 0x2038, false);
        at(BUS, Gcc, 0x0b024, false);
        at(MDSS_IFACE, Gcc, 0x0b00c, false);
        // Every branch register is a word of its own: a write to one cannot be a write to another.
        for (i, a) in BRANCHES.iter().enumerate() {
            for (j, b) in BRANCHES.iter().enumerate() {
                if i != j {
                    assert!((a.at != b.at) || (a.reg != b.reg), "{}", a.name);
                }
            }
        }
    }
}
