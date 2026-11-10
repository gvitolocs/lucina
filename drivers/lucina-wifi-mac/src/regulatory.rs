//! Which channels may be used, simply and explicitly.
//!
//! The table is ath10k's channel list (`ath10k_2ghz_channels`, `ath10k_5ghz_channels`) cut by cfg80211's world
//! regulatory domain (`world_regdom` in `net/wireless/reg.c`), which is what a device with no country set uses:
//!
//! | Channels | World rule | Here |
//! |---|---|---|
//! | 1-11 | free | active scan, transmit |
//! | 12-13 | `NO_IR`, 20 MHz | passive until a beacon is heard there |
//! | 14 | `NO_IR`, no OFDM (Japan only) | not used |
//! | 36-48, 149-165 | `NO_IR` | passive until a beacon is heard there |
//! | 52-64, 100-144 | `NO_IR`, `DFS` | always passive |
//! | 169, 173 | outside every rule | not used |
//!
//! "Passive until a beacon" is cfg80211's beacon hint (`regulatory_hint_found_beacon`, `handle_reg_beacon`): a
//! beacon proves that someone may radiate there, so `NO_IR` is lifted, except on radar channels, and on 2.4 GHz
//! only for 12-14. A station may join an AP on any listed channel once it has heard that AP's beacon: `NO_IR`
//! forbids starting radiation, not answering.
//!
//! An AP's Country element can only restrict further: channels it does not list become unusable and the power
//! limit becomes the lower of the two. It never adds a channel, and it is dropped on disconnect, as
//! `regulatory_hint_disconnect()` does.

use crate::ie::{EID_COUNTRY, find};
use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Band {
    Ghz2,
    Ghz5,
}

/// One channel's world-domain rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChannelRule {
    pub channel: u8,
    pub freq_mhz: u16,
    pub band: Band,
    pub no_ir: bool,
    pub dfs: bool,
    /// EIRP limit of the rule.
    pub max_power_dbm: u8,
}

const fn rule(channel: u8, freq_mhz: u16, no_ir: bool, dfs: bool) -> ChannelRule {
    ChannelRule {
        channel,
        freq_mhz,
        band: if freq_mhz < 3000 {
            Band::Ghz2
        } else {
            Band::Ghz5
        },
        no_ir,
        dfs,
        max_power_dbm: 20,
    }
}

/// The channels this station will ever touch.
pub const WORLD_CHANNELS: [ChannelRule; 38] = [
    rule(1, 2412, false, false),
    rule(2, 2417, false, false),
    rule(3, 2422, false, false),
    rule(4, 2427, false, false),
    rule(5, 2432, false, false),
    rule(6, 2437, false, false),
    rule(7, 2442, false, false),
    rule(8, 2447, false, false),
    rule(9, 2452, false, false),
    rule(10, 2457, false, false),
    rule(11, 2462, false, false),
    rule(12, 2467, true, false),
    rule(13, 2472, true, false),
    rule(36, 5180, true, false),
    rule(40, 5200, true, false),
    rule(44, 5220, true, false),
    rule(48, 5240, true, false),
    rule(52, 5260, true, true),
    rule(56, 5280, true, true),
    rule(60, 5300, true, true),
    rule(64, 5320, true, true),
    rule(100, 5500, true, true),
    rule(104, 5520, true, true),
    rule(108, 5540, true, true),
    rule(112, 5560, true, true),
    rule(116, 5580, true, true),
    rule(120, 5600, true, true),
    rule(124, 5620, true, true),
    rule(128, 5640, true, true),
    rule(132, 5660, true, true),
    rule(136, 5680, true, true),
    rule(140, 5700, true, true),
    rule(144, 5720, true, true),
    rule(149, 5745, true, false),
    rule(153, 5765, true, false),
    rule(157, 5785, true, false),
    rule(161, 5805, true, false),
    rule(165, 5825, true, false),
];

pub fn rule_for_freq(freq_mhz: u16) -> Option<&'static ChannelRule> {
    WORLD_CHANNELS.iter().find(|c| c.freq_mhz == freq_mhz)
}

pub fn rule_for_channel(band: Band, channel: u8) -> Option<&'static ChannelRule> {
    WORLD_CHANNELS
        .iter()
        .find(|c| c.band == band && c.channel == channel)
}

/// A channel as it stands now, for the firmware's channel list (`ath10k_update_channel_list`) and scans.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChannelState {
    pub channel: u8,
    pub freq_mhz: u16,
    pub band: Band,
    /// No probe requests here (`IEEE80211_CHAN_NO_IR` as ath10k's `passive`).
    pub passive: bool,
    pub dfs: bool,
    pub max_power_dbm: u8,
}

/// One subband of a Country element.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CountryBand {
    first: u8,
    count: u8,
    power_dbm: u8,
}

/// A parsed Country element.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Country {
    pub alpha2: [u8; 2],
    bands: Vec<CountryBand>,
}

impl Country {
    /// `data` is the element's body. Operating-class triplets (first octet 201 and up) are skipped; a Country
    /// element with no subband triplets gives `None` and so restricts nothing.
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 3 {
            return None;
        }
        let mut bands = Vec::new();
        for t in data[3..].as_chunks::<3>().0 {
            if t[0] != 0 && t[0] < 201 && t[1] != 0 {
                bands.push(CountryBand {
                    first: t[0],
                    count: t[1],
                    power_dbm: t[2],
                });
            }
        }
        if bands.is_empty() {
            return None;
        }
        Some(Self {
            alpha2: [data[0], data[1]],
            bands,
        })
    }

    /// The subband's limit for a channel, or `None` if the country does not list it.
    fn limit(&self, rule: &ChannelRule) -> Option<u8> {
        let step = if rule.band == Band::Ghz2 { 1 } else { 4 };
        self.bands.iter().find_map(|b| {
            // The subband must be in the channel's band: 5 GHz numbers start well above 14.
            if (b.first <= 14) != (rule.band == Band::Ghz2) {
                return None;
            }
            let last = b.first as u32 + (b.count as u32 - 1) * step;
            ((b.first as u32..=last).contains(&(rule.channel as u32))
                && (rule.channel as u32 - b.first as u32).is_multiple_of(step))
            .then_some(b.power_dbm)
        })
    }
}

/// The live state: beacon hints and the current AP's Country element.
#[derive(Clone, Debug)]
pub struct Regulatory {
    beacon_seen: [bool; WORLD_CHANNELS.len()],
    country: Option<Country>,
}

impl Default for Regulatory {
    fn default() -> Self {
        Self::new()
    }
}

impl Regulatory {
    pub fn new() -> Self {
        Self {
            beacon_seen: [false; WORLD_CHANNELS.len()],
            country: None,
        }
    }

    /// A beacon was heard on this frequency. It lifts `NO_IR` only where cfg80211's hint does.
    pub fn on_beacon(&mut self, freq_mhz: u16) {
        if let Some(i) = WORLD_CHANNELS.iter().position(|c| c.freq_mhz == freq_mhz) {
            let c = &WORLD_CHANNELS[i];
            if !c.dfs && (c.band == Band::Ghz5 || c.channel >= 12) {
                self.beacon_seen[i] = true;
            }
        }
    }

    /// Applies the Country element of the AP being joined, from that AP's raw IEs.
    pub fn apply_country_ie(&mut self, ies: &[u8]) {
        self.country = find(ies, EID_COUNTRY).and_then(Country::parse);
    }

    pub fn set_country(&mut self, country: Option<Country>) {
        self.country = country;
    }

    pub fn country(&self) -> Option<&Country> {
        self.country.as_ref()
    }

    pub fn clear_country(&mut self) {
        self.country = None;
    }

    fn state_of(&self, index: usize) -> Option<ChannelState> {
        let c = &WORLD_CHANNELS[index];
        let mut power = c.max_power_dbm;
        if let Some(country) = &self.country {
            power = power.min(country.limit(c)?);
        }
        Some(ChannelState {
            channel: c.channel,
            freq_mhz: c.freq_mhz,
            band: c.band,
            passive: c.no_ir && !self.beacon_seen[index],
            dfs: c.dfs,
            max_power_dbm: power,
        })
    }

    /// A channel's current state; `None` if it may not be used at all.
    pub fn channel(&self, freq_mhz: u16) -> Option<ChannelState> {
        let i = WORLD_CHANNELS.iter().position(|c| c.freq_mhz == freq_mhz)?;
        self.state_of(i)
    }

    /// Every usable channel.
    pub fn channels(&self) -> Vec<ChannelState> {
        (0..WORLD_CHANNELS.len())
            .filter_map(|i| self.state_of(i))
            .collect()
    }
}
