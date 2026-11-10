//! Which panel the phone has, from the cmdline ABL builds, and which packaged device tree goes with it.
//!
//! surya ships with one of two NT36672C panels. ABL names the one it initialised in its cmdline:
//! `msm_drm.dsi_display0=dsi_nt36672c_huaxing_fhd_video_display:` or `..._tianma_...`
//! (`docs/research/boot-chain.md` section 5). The mainline port does the same lookup in U-Boot and falls back to
//! Huaxing; so do we.

use crate::pkg::{Entry, Package};

/// The cmdline key ABL sets.
pub const KEY: &str = "msm_drm.dsi_display0=";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    Huaxing,
    Tianma,
    /// The key is missing or names neither panel (QEMU, another loader).
    Unknown,
}

impl Panel {
    /// The package's device tree name for this panel.
    pub fn dtb_name(self) -> Option<&'static str> {
        match self {
            Panel::Huaxing => Some("huaxing"),
            Panel::Tianma => Some("tianma"),
            Panel::Unknown => None,
        }
    }
}

/// The panel named by the last `msm_drm.dsi_display0=` in `cmdline` (the kernel also lets a later value win).
pub fn from_cmdline(cmdline: &str) -> Panel {
    let mut panel = Panel::Unknown;
    for arg in cmdline.split_ascii_whitespace() {
        if let Some(value) = arg.strip_prefix(KEY) {
            // dsi_nt36672c_<vendor>_fhd_video_display: -> look for the vendor as a whole `_`-separated word.
            let words = || value.split(['_', ':']);
            panel = if words().any(|w| w == "huaxing") {
                Panel::Huaxing
            } else if words().any(|w| w == "tianma") {
                Panel::Tianma
            } else {
                Panel::Unknown
            };
        }
    }
    panel
}

/// Why a device tree was chosen, for the boot log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// The packaged tree for the panel ABL named.
    Panel,
    /// The panel is unknown or has no tree: the Huaxing tree, as in the mainline port.
    HuaxingFallback,
    /// A tree named `default`.
    Default,
    /// The package's only tree.
    Only,
    /// No packaged tree fits: boot with the tree ABL passed in.
    Loader,
}

/// The packaged device tree to boot with, or `None` (with [`Choice::Loader`]) to use the loader's own.
pub fn choose<'a>(pkg: &Package<'a>, panel: Panel) -> (Option<Entry<'a>>, Choice) {
    if let Some(e) = panel.dtb_name().and_then(|n| pkg.dtb(n)) {
        return (Some(e), Choice::Panel);
    }
    if let Some(e) = pkg.dtb("huaxing") {
        return (Some(e), Choice::HuaxingFallback);
    }
    if let Some(e) = pkg.dtb("default") {
        return (Some(e), Choice::Default);
    }
    let mut dtbs = pkg.dtbs();
    match (dtbs.next(), dtbs.next()) {
        (Some(e), None) => (Some(e), Choice::Only),
        _ => (None, Choice::Loader),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pkg::{Kind, layout};

    const HUAXING: &str = "rcupdate.rcu_expedited=1 msm_drm.dsi_display0=dsi_nt36672c_huaxing_fhd_video_display: \
                           androidboot.hwname=surya";
    const TIANMA: &str =
        "msm_drm.dsi_display0=dsi_nt36672c_tianma_fhd_video_display: androidboot.hwname=surya";

    #[test]
    fn reads_both_panels() {
        assert_eq!(from_cmdline(HUAXING), Panel::Huaxing);
        assert_eq!(from_cmdline(TIANMA), Panel::Tianma);
        assert_eq!(from_cmdline("console=ttyAMA0"), Panel::Unknown);
        assert_eq!(from_cmdline(""), Panel::Unknown);
        assert_eq!(
            from_cmdline("msm_drm.dsi_display0=dsi_other_cmd_display:"),
            Panel::Unknown
        );
        // A vendor name inside another word does not count.
        assert_eq!(
            from_cmdline("msm_drm.dsi_display0=dsi_nottianma_display:"),
            Panel::Unknown
        );
        // The key must be a whole argument.
        assert_eq!(
            from_cmdline("xmsm_drm.dsi_display0=dsi_nt36672c_tianma_fhd_video_display:"),
            Panel::Unknown
        );
        // The last value wins.
        assert_eq!(from_cmdline(&format!("{HUAXING} {TIANMA}")), Panel::Tianma);
    }

    fn package(dtbs: &[&str]) -> Vec<u8> {
        let mut spec = vec![(Kind::Kernel, "", 1u64)];
        spec.extend(dtbs.iter().map(|&n| (Kind::Dtb, n, 1u64)));
        let mut out = vec![0u8; 1 << 16];
        let (_, total) = layout(&spec, &mut out).unwrap();
        out.truncate(total as usize);
        out
    }

    #[test]
    fn chooses_with_fallbacks() {
        let both = package(&["huaxing", "tianma"]);
        let p = Package::parse(&both).unwrap();
        fn name<'a>((e, c): (Option<Entry<'a>>, Choice)) -> (Option<&'a str>, Choice) {
            (e.map(|e| e.name), c)
        }
        assert_eq!(
            name(choose(&p, Panel::Tianma)),
            (Some("tianma"), Choice::Panel)
        );
        assert_eq!(
            name(choose(&p, Panel::Huaxing)),
            (Some("huaxing"), Choice::Panel)
        );
        assert_eq!(
            name(choose(&p, Panel::Unknown)),
            (Some("huaxing"), Choice::HuaxingFallback)
        );

        let only_huaxing = package(&["huaxing"]);
        let p = Package::parse(&only_huaxing).unwrap();
        assert_eq!(
            name(choose(&p, Panel::Tianma)),
            (Some("huaxing"), Choice::HuaxingFallback)
        );

        let default = package(&["default", "qemu"]);
        let p = Package::parse(&default).unwrap();
        assert_eq!(
            name(choose(&p, Panel::Tianma)),
            (Some("default"), Choice::Default)
        );

        let one = package(&["qemu"]);
        let p = Package::parse(&one).unwrap();
        assert_eq!(
            name(choose(&p, Panel::Unknown)),
            (Some("qemu"), Choice::Only)
        );

        let two = package(&["a", "b"]);
        let p = Package::parse(&two).unwrap();
        assert_eq!(name(choose(&p, Panel::Unknown)), (None, Choice::Loader));

        let none = package(&[]);
        let p = Package::parse(&none).unwrap();
        assert_eq!(name(choose(&p, Panel::Huaxing)), (None, Choice::Loader));
    }
}
