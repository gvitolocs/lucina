//! What a system sleep takes away from this DRV's votes, device by device, and gives back on the way up (patch 1354).
//!
//! Derived from Linux v6.12 (GPL-2.0): `drivers/pmdomain/qcom/rpmhpd.c`, `drivers/pmdomain/core.c`,
//! `drivers/clk/qcom/gcc-sc7180.c`, `drivers/clk/qcom/clk-rpmh.c`, `drivers/gpu/drm/msm/disp/dpu1/dpu_kms.c`,
//! `drivers/media/platform/qcom/venus/core.c`, `drivers/clk/qcom/gdsc.c`, `drivers/ufs/core/ufshcd.c`,
//! `drivers/ufs/host/ufs-qcom.c`, `drivers/usb/dwc3/core.c`, `drivers/usb/dwc3/dwc3-qcom.c`,
//! `drivers/phy/qualcomm/phy-qcom-qusb2.c`, `drivers/phy/qualcomm/phy-qcom-qmp-combo.c`, `arch/arm64/boot/dts/qcom/sc7180.dtsi`.
//! Line numbers are v6.12's.
//!
//! Under Linux CX's sleep level falls only because its consumers power off: a device's runtime suspend drops its
//! performance state and genpd turns its domain off once every device in it is off (`rpmhpd_power_off`,
//! `rpmhpd.c:759-773`, which aggregates corner 0, `:692-739`). Here each device's hook asks for exactly that, through
//! [`crate::share::Votes::power_consumer`] for a device tree consumer and its own requests for a driver that votes for
//! itself (Venus: [`crate::share::Client::Venus`] and its two interconnect paths). Nothing is dropped unless the device's
//! own `lucina.suspend.<dev>=on` switch is on: the hooks read those, and pass them in here.
//!
//! Only what the kernel holds through RPMh can be let go. What each 1354 point of the hooks maps to:
//!
//! * scanout (`fb/scanout.rs`): the DPU's `<&rpmhpd SC7180_CX>` consumer ([`DPU`]) may go only once the timing engine is
//!   quiet and the MDP core clock, whose rate its `cx` level is for, is off ([`scanout_down`]), as genpd's power-off of the
//!   DPU comes after `dpu_runtime_suspend`'s `clk_bulk_disable_unprepare` (`dpu_kms.c:1406-1407`). In Linux that clock
//!   (`disp_cc_mdss_mdp_clk`, `dispcc-sc7180.c:501`) is also the MDSS's (`sc7180.dtsi:3172-3174`), and goes off only with
//!   the MDSS's runtime suspend (`msm_mdss.c:485-490`, `:346-350`), after every child of the MDSS, the DSI host among
//!   them, is down. This kernel powers down neither the DSI host nor the MDSS and gates no display clock, so the clock
//!   runs at the firmware's rate through a sleep and the DPU keeps its `cx` vote. The display's `mdp0-mem` path
//!   (`dpu_kms.c:1409-1410`) is not ours: it asks for no bandwidth here (the floor client holds what the firmware left).
//! * Venus: its `cx` ([`crate::share::Client::Venus`]) already goes in `core_power_v4(POWER_OFF)`; with the switch its
//!   `video-mem` and `cpu-cfg` requests go to 0 after it (`core.c:486`, `:490`), into the per-consumer sum that never goes
//!   below the floor client ([`crate::icc`]). `venus_runtime_resume` votes them again before power comes on.
//! * GPU, audio (the VA macro), Wi-Fi: nothing on this DRV. The GPU's domains are the GPU clock controller's GDSCs, whose
//!   controller holds no RPMh domain in the device tree, and its `gfx-mem` path is not voted here; the VA macro's clocks
//!   are the audio DSP's; Wi-Fi's are an RF clock and regulators no driver here holds.
//! * UFS (`ufs_sleep.rs`, patch 1355): the host ([`UFS_HOST`]) and its PHY ([`UFS_PHY`]) are both in `UFS_PHY_GDSC`
//!   (`sc7180.dtsi:1547`, `:1594`), a GCC GDSC, not an `rpmhpd` domain. genpd reaches `cx` through it: `gdsc_register`
//!   makes every GCC GDSC a subdomain of the domain the GCC node holds (`gdsc.c:559-560`), and that is
//!   `<&rpmhpd SC7180_CX>` (`sc7180.dtsi:800`). Neither node has `required-opps` or an OPP table (the clocks scale by
//!   `freq-table-hz`, `:1566-1572`), so each asks for level 0: what it holds is its share of `cx` being on, which goes
//!   with genpd's power-off of the GDSC once both devices are down (`genpd_finish_suspend`, `pmdomain/core.c:1430-1432`;
//!   `genpd_runtime_suspend` `:1146-1147`). [`ufs_down`] powers the two consumers off, host then PHY (the PHY is the
//!   host's supplier and goes after it); with them off no level falls, because GCC's own consumer, which nothing
//!   powers off, keeps `cx` on. Everything else at the 1353 points is not a vote on this DRV: the lane symbol clocks,
//!   the ICE core clock and the host clocks but one are GCC branches (`ufs-qcom.c:580`, `:583`, `ufshcd.c:10022`);
//!   the one that is RPMh, the host's `ref_clk` (`<&rpmhcc RPMH_CXO_CLK>`, `sc7180.dtsi:1562`), is XO, and no XO
//!   client here is the UFS host's; the clock scaling (`ufshcd.c:9656`) changes rates only, with no OPP behind it;
//!   the two interconnect paths (`ufs-qcom.c:966`, `ufs-ddr` and `cpu-ufs`) are not requests this kernel holds.
//! * USB (`usb_sleep.rs`, patch 1355): the DWC3 glue ([`USB`]) is in `USB30_PRIM_GDSC` (`sc7180.dtsi:3051`), the same
//!   way under GCC's `cx`, and asks for `rpmhpd_opp_nom` (`:3052`), the level its master clock's 150 MHz
//!   (`:3035-3038`) wants. Linux lets it go after `dwc3_qcom_suspend` has stopped that clock
//!   (`dwc3-qcom.c:440-441`, genpd's drop of the performance state after the callback, `pmdomain/core.c:1147`). This kernel
//!   gates none of the glue's clocks (GCC branches, `sc7180.dtsi:3025-3029`), so the clock runs at its rate through a
//!   sleep and, as the DPU's, the vote stays: [`usb_down`] lets it go only with the clocks off. The PHYs
//!   (`dwc3/core.c:2386-2389`) have no power domain in the tree; QUSB2's runtime suspend gates its `ref` XO and `cfg_ahb`
//!   (`phy-qcom-qusb2.c:664-667`) and the QMP PHY's GCC clocks only (`phy-qcom-qmp-combo.c:3051-3052`): no XO client
//!   here is theirs. The glue's two interconnect paths (`dwc3-qcom.c:443-445`, `usb-ddr` and `apps-usb`) are not
//!   requests this kernel holds.
//!
//! The XO hold ([`gcc_xo_ao`]): Linux's GCC takes `bi_tcxo_ao` for good, through the critical `gcc_cpuss_ahb_clk`
//! (`gcc-sc7180.c:979`) whose source runs from `bi_tcxo_ao` (`gcc_parent_data_0_ao`, `:172-176`, `:262-275`), and takes
//! `bi_tcxo` only through GPLL0 (`:43-46`) while one of GPLL0's users is prepared. With `lucina.suspend.xo=ao` the kernel
//! casts GCC's hold the same way: GCC on `bi_tcxo_ao`, and [`crate::share::Client::Gpll0`] on `bi_tcxo`. This kernel has no
//! GCC clock framework that counts GPLL0's users, so the GPLL0 hold is never let go and the XO stays on asleep, as today.

/// The device tree node of the DPU, the scanout's consumer of `cx`: the last component of its path, as
/// [`node_is`] matches it (`sc7180.dtsi`'s `mdss_mdp`).
pub const DPU: &str = "display-controller@ae01000";

/// The UFS host controller, a consumer of `cx` through `UFS_PHY_GDSC` (`sc7180.dtsi`'s `ufs_mem_hc`).
pub const UFS_HOST: &str = "ufshc@1d84000";

/// The UFS PHY, in the same GDSC (`sc7180.dtsi`'s `ufs_mem_phy`).
pub const UFS_PHY: &str = "phy@1d87000";

/// The DWC3 glue of the USB host, a consumer of `cx` through `USB30_PRIM_GDSC` (`sc7180.dtsi`'s `usb_1`).
pub const USB: &str = "usb@a6f8800";

/// Whether the device tree path `path` is the node `name`: the whole path, or its last component.
pub fn node_is(path: &str, name: &str) -> bool {
    path == name || path.rsplit('/').next() == Some(name)
}

/// One vote a hook takes away (or gives back).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Release {
    /// genpd's power-off of this device tree consumer (named as [`node_is`] matches it); back: its power-on.
    Consumer(&'static str),
    /// Venus's `video-mem` and `cpu-cfg` requests to 0, in that order (`core.c:486`, `:490`).
    VenusPaths,
}

/// What the scanout's `suspend` lets go, after the timing engine: the DPU's consumer, when the switch is on, the engine is
/// quiet and the MDP core clock is off (`mdp_clock_on` false). A timing engine that still runs, or a clock that runs, keeps
/// the vote: no `cx` level is let go while the clock it is for runs at its rate.
pub fn scanout_down(switch: bool, quiet: bool, mdp_clock_on: bool) -> &'static [Release] {
    if switch && quiet && !mdp_clock_on {
        &[Release::Consumer(DPU)]
    } else {
        &[]
    }
}

/// What the scanout's way back takes again before the engine starts: the DPU's consumer, genpd's power-on before the
/// driver's runtime resume. A consumer that was not let go is on already, and this sends nothing.
pub fn scanout_up(switch: bool) -> &'static [Release] {
    if switch {
        &[Release::Consumer(DPU)]
    } else {
        &[]
    }
}

/// What Venus's power-down lets go after its `cx`: the two paths, with the switch. Its power-up already votes them.
pub fn venus_down(switch: bool) -> &'static [Release] {
    if switch { &[Release::VenusPaths] } else { &[] }
}

/// What the disk's `suspend` lets go once its links are in hibern8 and its interrupt is masked: genpd's power-off of
/// the host's consumer, then the PHY's (the PHY is the host's supplier, so the way down reaches it after the host).
/// Both ask for level 0, so no level falls with them.
pub fn ufs_down(switch: bool) -> &'static [Release] {
    if switch {
        &[Release::Consumer(UFS_HOST), Release::Consumer(UFS_PHY)]
    } else {
        &[]
    }
}

/// What the disk's way back takes again before its interrupt is lifted and its links leave hibern8: the PHY first,
/// then the host, genpd's power-on before the driver's resume.
pub fn ufs_up(switch: bool) -> &'static [Release] {
    if switch {
        &[Release::Consumer(UFS_PHY), Release::Consumer(UFS_HOST)]
    } else {
        &[]
    }
}

/// What the USB hooks' `suspend` lets go after the glue's step: the glue's consumer, when the switch is on and the
/// glue's clocks are off (`glue_clocks_on` false). Its `nom` level is for the master clock's rate, so a clock that
/// runs keeps it, as the DPU's does.
pub fn usb_down(switch: bool, glue_clocks_on: bool) -> &'static [Release] {
    if switch && !glue_clocks_on {
        &[Release::Consumer(USB)]
    } else {
        &[]
    }
}

/// What the USB way back takes again before the glue's step: the glue's consumer. One that was not let go is on
/// already, and this sends nothing.
pub fn usb_up(switch: bool) -> &'static [Release] {
    if switch {
        &[Release::Consumer(USB)]
    } else {
        &[]
    }
}

/// `lucina.suspend.xo=ao` on the command line (before any `--`): GCC's XO hold cast as Linux's GCC casts it. The last
/// `lucina.suspend.xo=` wins; anything but `ao` is today's single `bi_tcxo` hold.
pub fn gcc_xo_ao(cmdline: &str) -> bool {
    cmdline
        .split_ascii_whitespace()
        .take_while(|a| *a != "--")
        .filter_map(|a| a.strip_prefix("lucina.suspend.xo="))
        .last()
        == Some("ao")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_node_is_its_path_or_its_last_component() {
        let p = "/soc@0/display-subsystem@ae00000/display-controller@ae01000";
        for (path, name, is) in [
            (p, DPU, true),
            (p, p, true),
            (p, "display-subsystem@ae00000", false),
            (p, "controller@ae01000", false),
            ("/soc@0/display-subsystem@ae00000/dsi@ae94000", DPU, false),
            (DPU, DPU, true),
        ] {
            assert_eq!(node_is(path, name), is, "{path} {name}");
        }
    }

    #[test]
    fn nothing_is_dropped_without_the_switch() {
        for quiet in [false, true] {
            for clock in [false, true] {
                assert!(scanout_down(false, quiet, clock).is_empty());
            }
        }
        assert!(scanout_up(false).is_empty());
        assert!(venus_down(false).is_empty());
        assert_eq!(scanout_down(true, true, false), [Release::Consumer(DPU)]);
        assert!(
            scanout_down(true, false, false).is_empty(),
            "a running engine keeps its vote"
        );
        assert_eq!(scanout_up(true), [Release::Consumer(DPU)]);
        assert_eq!(venus_down(true), [Release::VenusPaths]);
    }

    #[test]
    fn the_xo_switch() {
        for (cmdline, ao) in [
            ("", false),
            ("lucina.suspend.xo=ao", true),
            ("quiet lucina.suspend.xo=ao root=/dev/x", true),
            ("lucina.suspend.xo=on", false),
            ("lucina.suspend.xo=ao lucina.suspend.xo=off", false),
            ("lucina.suspend.xo=off lucina.suspend.xo=ao", true),
            ("-- lucina.suspend.xo=ao", false),
            ("lucina.suspend.xo=aoo", false),
        ] {
            assert_eq!(gcc_xo_ao(cmdline), ao, "{cmdline:?}");
        }
    }

    /// The disk's and USB's rules, for each switch and clock state: the order of each list, and nothing without the
    /// switch.
    #[test]
    fn the_disk_and_usb_consumers() {
        // (switch, glue clocks on) -> ufs_down, ufs_up, usb_down, usb_up.
        type Row = (bool, bool, Rules, Rules, Rules, Rules);
        type Rules = &'static [Release];
        let table: [Row; 4] = [
            (false, false, &[], &[], &[], &[]),
            (false, true, &[], &[], &[], &[]),
            (
                true,
                false,
                &[Release::Consumer(UFS_HOST), Release::Consumer(UFS_PHY)],
                &[Release::Consumer(UFS_PHY), Release::Consumer(UFS_HOST)],
                &[Release::Consumer(USB)],
                &[Release::Consumer(USB)],
            ),
            (
                true,
                true,
                &[Release::Consumer(UFS_HOST), Release::Consumer(UFS_PHY)],
                &[Release::Consumer(UFS_PHY), Release::Consumer(UFS_HOST)],
                &[],
                &[Release::Consumer(USB)],
            ),
        ];
        for (switch, clocks, ufs_d, ufs_u, usb_d, usb_u) in table {
            assert_eq!(ufs_down(switch), ufs_d, "switch {switch}");
            assert_eq!(ufs_up(switch), ufs_u, "switch {switch}");
            assert_eq!(
                usb_down(switch, clocks),
                usb_d,
                "switch {switch} clocks {clocks}"
            );
            assert_eq!(usb_up(switch), usb_u, "switch {switch}");
        }
        let laptop = [
            ("/soc@0/ufshc@1d84000", UFS_HOST),
            ("/soc@0/phy@1d87000", UFS_PHY),
            ("/soc@0/usb@a6f8800", USB),
        ];
        for (path, name) in laptop {
            assert!(node_is(path, name), "{path}");
        }
        assert!(
            !node_is("/soc@0/usb@a6f8800/usb@a600000", USB),
            "the DWC3 core is not the glue"
        );
        assert!(!node_is("/soc@0/phy@88e8000", UFS_PHY));
    }

    /// The order Linux keeps (`dpu_kms.c:1406-1407`, genpd's power-off after the driver's suspend): no `cx` vote goes
    /// while the MDP clock runs, whatever else holds.
    #[test]
    fn no_vote_goes_while_the_mdp_clock_runs() {
        for switch in [false, true] {
            for quiet in [false, true] {
                assert!(
                    scanout_down(switch, quiet, true).is_empty(),
                    "switch {switch} quiet {quiet}"
                );
                assert_eq!(
                    scanout_down(switch, quiet, false).is_empty(),
                    !(switch && quiet),
                    "switch {switch} quiet {quiet}"
                );
            }
        }
    }
}
