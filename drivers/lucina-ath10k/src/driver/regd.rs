//! Regulatory domain and channel list for `ath10k_regd_update`.
//!
//! Derived from Linux `drivers/net/wireless/ath/regd.c`, `regd_common.h` and `ath10k/mac.c` (SPDX ISC; Copyright (c)
//! 2008-2009 Atheros Communications Inc., 2005-2011 Atheros, 2011-2017 Qualcomm Atheros, Inc., 2018 The Linux
//! Foundation). `tests/driver.rs` compares [`REG_PAIRS`] with `regDomainPairs[]` compiled from the header.
//!
//! The firmware reports its EEPROM regulatory domain in service ready. Linux uses it as a pair value, as a country
//! code (through `allCountries`, not here), and for 0 as the United States. This driver answers:
//!
//! * a value in the pair table: that pair, for the `WMI_PDEV_SET_REGDOMAIN` command;
//! * anything else, 0 and a country code included: world, `WOR4_WORLD` 0x64 (Linux's `ath_regd_sanitize` value for a
//!   blank EEPROM, as the owner chose; a country code here means this unit left the factory with one and would get
//!   a tighter rule in Linux, never a looser one).
//!
//! The channels are always the world rules (`ath_world_regdom_60_61_62` of `regd.c`), whatever the pair says: active
//! scan only on 1-11, everything else passive; there is no wireless-regdb here to do better. 5 GHz radar channels
//! are `ath_is_radar_freq`.

use alloc::vec::Vec;

use crate::wmi_tlv::{ChannelArg, phy_mode};

/// `WOR4_WORLD`.
pub const WORLD: u16 = 0x64;
/// `COUNTRY_ERD_FLAG` and `WORLDWIDE_ROAMING_FLAG`.
const COUNTRY_ERD_FLAG: u16 = 0x8000;
const WORLDWIDE_ROAMING_FLAG: u16 = 0x4000;
/// `.max_power` of `CHAN2G`/`CHAN5G`.
const CHAN_MAX_POWER: u32 = 30;
/// `ATH_2GHZ_*` EIRP, and `ATH_5GHZ_*`.
const RULE_POWER_2G: u32 = 20;
const RULE_POWER_5G: u32 = 30;

/// `ath10k_2ghz_channels`, the frequencies.
const CHANNELS_2G: [u16; 14] = [
    2412, 2417, 2422, 2427, 2432, 2437, 2442, 2447, 2452, 2457, 2462, 2467, 2472, 2484,
];
/// `ath10k_5ghz_channels` inside the world rule's 5150-5350 and 5470-5850 (so without 5865).
const CHANNELS_5G: [u16; 26] = [
    5180, 5200, 5220, 5240, 5260, 5280, 5300, 5320, 5500, 5520, 5540, 5560, 5580, 5600, 5620, 5640,
    5660, 5680, 5700, 5720, 5745, 5765, 5785, 5805, 5825, 5845,
];

/// The pair for what the firmware reports: `(reg_domain, 5 GHz CTL, 2 GHz CTL)`, see the module comment.
pub fn regpair_for(eeprom_rd: u32) -> (u16, u16, u16) {
    let rd = (eeprom_rd as u16) & !WORLDWIDE_ROAMING_FLAG;
    let world = REG_PAIRS.iter().find(|p| p.0 == WORLD);
    let hit = if rd == 0 || rd & COUNTRY_ERD_FLAG != 0 {
        None
    } else {
        REG_PAIRS.iter().find(|p| p.0 == rd)
    };
    hit.or(world).copied().unwrap_or((WORLD, 0xff, 0xff))
}

/// One channel of `ath10k_update_channel_list`.
fn channel(freq: u16, ht40plus: bool, passive_rule: bool, radar: bool) -> ChannelArg {
    let is_2g = freq < 3000;
    let power = CHAN_MAX_POWER.min(if is_2g { RULE_POWER_2G } else { RULE_POWER_5G }) * 2;
    ChannelArg {
        freq: u32::from(freq),
        band_center_freq1: u32::from(freq),
        allow_ht: true,
        allow_vht: true,
        allow_ibss: !passive_rule,
        ht40plus,
        chan_radar: radar,
        passive: passive_rule || radar,
        min_power: 0,
        max_power: power,
        max_reg_power: power,
        max_antenna_gain: 0,
        mode: if is_2g {
            phy_mode::MODE_11G
        } else {
            phy_mode::MODE_11A
        },
        ..ChannelArg::default()
    }
}

/// The channel list. `low_5g`/`high_5g` come from the firmware (`ath10k_mac_update_channel_list`, zero: no limit).
/// `ht40plus` is cfg80211's `reg_process_ht_flags_channel`: the channel 20 MHz above is in the list.
pub fn channels(low_5g: u32, high_5g: u32) -> Vec<ChannelArg> {
    let limited = low_5g != 0 && high_5g != 0;
    let five: Vec<u16> = CHANNELS_5G
        .iter()
        .copied()
        .filter(|f| !limited || (u32::from(*f) >= low_5g && u32::from(*f) <= high_5g))
        .collect();
    let mut out = Vec::with_capacity(CHANNELS_2G.len() + five.len());
    for f in CHANNELS_2G {
        let up = CHANNELS_2G.contains(&(f + 20));
        out.push(channel(f, up, f > 2462, false));
    }
    for f in &five {
        let up = five.contains(&(f + 20));
        out.push(channel(*f, up, true, (5260..=5700).contains(f)));
    }
    out
}

/// `regDomainPairs[]`: `(regpair, 5 GHz CTL, 2 GHz CTL)`.
pub static REG_PAIRS: [(u16, u16, u16); 113] = [
    (0x0, 0x1ff, 0x1ff),
    (0x3, 0xff, 0x30),
    (0x7, 0xff, 0x30),
    (0x8, 0xff, 0x30),
    (0x20, 0x10, 0x10),
    (0x21, 0x10, 0x30),
    (0x22, 0x10, 0x30),
    (0x3a, 0x10, 0x10),
    (0x3b, 0x10, 0x30),
    (0x3f, 0x10, 0x30),
    (0x12, 0x10, 0x10),
    (0x13, 0x10, 0x10),
    (0x14, 0x10, 0x10),
    (0x23, 0x10, 0x30),
    (0x37, 0x30, 0x30),
    (0x35, 0x30, 0x30),
    (0x36, 0x30, 0x30),
    (0x30, 0x30, 0x30),
    (0x39, 0x30, 0x30),
    (0x34, 0x30, 0x30),
    (0x3d, 0x30, 0x30),
    (0x3e, 0x30, 0x30),
    (0x32, 0x30, 0x30),
    (0x31, 0x30, 0x30),
    (0x11, 0x10, 0x30),
    (0x10, 0x10, 0x10),
    (0x52, 0x10, 0x30),
    (0x45, 0x10, 0x30),
    (0x4d, 0x10, 0x10),
    (0x47, 0x10, 0x30),
    (0x42, 0x10, 0x30),
    (0x58, 0x10, 0x30),
    (0x5a, 0x30, 0x30),
    (0x5b, 0x30, 0x30),
    (0x5d, 0x30, 0x30),
    (0x5e, 0x30, 0x30),
    (0x5f, 0x30, 0x30),
    (0x50, 0x10, 0x10),
    (0x5c, 0x10, 0x10),
    (0x55, 0x10, 0x30),
    (0x56, 0x10, 0x30),
    (0x49, 0x10, 0xff),
    (0x40, 0x40, 0x40),
    (0x41, 0x40, 0x40),
    (0x48, 0x40, 0x10),
    (0x4a, 0x40, 0x40),
    (0x4b, 0x40, 0x40),
    (0x4c, 0x40, 0x40),
    (0x43, 0x40, 0x40),
    (0xf0, 0x40, 0x40),
    (0x80, 0x40, 0x40),
    (0xf1, 0x40, 0x40),
    (0x81, 0x40, 0x40),
    (0x82, 0x40, 0x40),
    (0xf2, 0x40, 0x10),
    (0xf3, 0x40, 0x40),
    (0x83, 0x40, 0x40),
    (0xf4, 0x40, 0x40),
    (0x84, 0x40, 0x40),
    (0x85, 0x40, 0x40),
    (0xf5, 0x40, 0x10),
    (0x86, 0x40, 0x40),
    (0x87, 0x40, 0x40),
    (0x88, 0x40, 0x40),
    (0x89, 0x40, 0x40),
    (0xf8, 0x40, 0x40),
    (0x8a, 0x40, 0x40),
    (0x8b, 0x40, 0x40),
    (0xf9, 0x40, 0x10),
    (0x8c, 0x40, 0x40),
    (0xfa, 0x40, 0x40),
    (0x8d, 0x40, 0x40),
    (0x8e, 0x40, 0x40),
    (0xfb, 0x40, 0x10),
    (0x8f, 0x40, 0x40),
    (0x90, 0x40, 0x40),
    (0x91, 0x40, 0x40),
    (0xf6, 0x40, 0x40),
    (0xfc, 0x40, 0x10),
    (0xfd, 0x40, 0x40),
    (0xff, 0x40, 0x40),
    (0xfe, 0x40, 0x40),
    (0xf7, 0x40, 0x40),
    (0xd0, 0x40, 0x10),
    (0xd1, 0x40, 0x40),
    (0xd3, 0x40, 0x40),
    (0xd2, 0x40, 0x40),
    (0xd4, 0x40, 0x40),
    (0xd5, 0x40, 0x10),
    (0xd6, 0x40, 0x40),
    (0xd8, 0x40, 0x40),
    (0xd7, 0x40, 0x40),
    (0xd9, 0x40, 0x40),
    (0xda, 0x40, 0x10),
    (0xdb, 0x40, 0x40),
    (0xdd, 0x40, 0x40),
    (0xdc, 0x40, 0x40),
    (0xde, 0x40, 0x40),
    (0x92, 0x40, 0x40),
    (0x93, 0x40, 0x40),
    (0x60, 0xff, 0xff),
    (0x61, 0xff, 0xff),
    (0x62, 0xff, 0xff),
    (0x63, 0xff, 0xff),
    (0x64, 0xff, 0xff),
    (0x65, 0xff, 0xff),
    (0x66, 0xff, 0xff),
    (0x67, 0xff, 0xff),
    (0x68, 0xff, 0xff),
    (0x69, 0xff, 0xff),
    (0x6a, 0xff, 0xff),
    (0x6b, 0xff, 0xff),
    (0x6c, 0xff, 0xff),
];
