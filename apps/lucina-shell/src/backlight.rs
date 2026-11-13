//! The screen's light: its level in Settings and on the brightness keys, the level kept across restarts, and the screen
//! going dark after a while without input. All of it goes through the kernel's backlight class device
//! (`/sys/class/backlight/backlight`), which exists only when the kernel could take the backlight over: without it there is
//! no `Backlight`, and nothing here happens.
//!
//! Going dark is Linux's DPMS (the X server's `xset dpms` standby) and Windows' "Turn off the display after": the light goes
//! to 0, the framebuffer is not blanked and apps are not paused, so the keyboard and the touchpad are still read and any of
//! them brings the light back.

use std::fs;
use std::path::{Path, PathBuf};

/// One brightness key press, or one Left/Right in Settings: 10 levels, a tenth of this laptop's 100.
pub const STEP: u32 = 10;

/// The choices of "Turn off the display after" in Windows' Power Options, in minutes; 0 is Never.
pub const SCREEN_OFF_CHOICES: [u32; 15] =
    [0, 1, 2, 3, 5, 10, 15, 20, 25, 30, 60, 120, 180, 240, 300];
/// Windows' Balanced plan: the display turns off after 5 minutes on battery (VIDEOIDLE DC 300 s)...
pub const BATTERY_DEFAULT_MIN: u32 = 5;
/// ...and after 10 minutes on the charger (VIDEOIDLE AC 600 s).
pub const CHARGER_DEFAULT_MIN: u32 = 10;

/// The level after one step, never below 1 (0 turns the light off: that is for the idle timer, not for a key) and never
/// above `max`. Linux's hid-input.c turns the HID consumer usages 0x70 and 0x6F into KEY_BRIGHTNESSDOWN and
/// KEY_BRIGHTNESSUP; what a press does is the desktop's, and here it is a step of `STEP`.
pub fn next_level(current: u32, up: bool, max: u32) -> u32 {
    let level = if up {
        current.saturating_add(STEP)
    } else {
        current.saturating_sub(STEP)
    };
    level.clamp(1, max.max(1))
}

/// A saved level written back at start, clamped as systemd-backlight clamps one (src/backlight/backlight.c,
/// `clamp_brightness`): at least 5 % of `max` (and at least 1), so that a screen is never restored dark, and at most `max`.
pub fn restored_level(saved: u32, max: u32) -> u32 {
    let min = (max / 20).max(1);
    saved.clamp(min, max.max(min))
}

/// The level at start from the kernel's `brightness`, and whether it must be written: a light found dark (0: a kernel that found it
/// dark and could not light it, patch 1094) is lit at the least level a restore gives
/// ([`restored_level`]), as systemd-backlight never restores a screen dark; the saved level follows once /data is ready.
pub fn start_level(kernel: u32, max: u32) -> (u32, bool) {
    if kernel == 0 {
        (restored_level(0, max), true)
    } else {
        (kernel.clamp(1, max), false)
    }
}

/// When the screen goes dark: `minutes` after the last input, both on CLOCK_MONOTONIC in microseconds. None when it never
/// does (0 is Never). The shell decides with `lucina_standby` now; this and [`screen_off_at`] are kept for their tests and as
/// the rule `standby.rs`'s tests check the proved one against.
#[cfg(test)]
pub fn idle_deadline(last_input_us: i64, minutes: u32) -> Option<i64> {
    (minutes > 0).then(|| last_input_us + i64::from(minutes) * 60_000_000)
}

/// When the screen goes dark with holds counted (`lucina_app::hold`): never while an app holds it on (Windows'
/// `ES_DISPLAY_REQUIRED`, Wayland's idle inhibitor), and otherwise `minutes` after the later of the last input and the end of
/// the last hold, as Windows' request resets the display's idle timer while it stands. None when it never does.
#[cfg(test)]
pub fn screen_off_at(
    last_input_us: i64,
    hold_ended_us: i64,
    held: bool,
    minutes: u32,
) -> Option<i64> {
    if held {
        return None;
    }
    idle_deadline(last_input_us.max(hold_ended_us), minutes)
}

/// The next of `SCREEN_OFF_CHOICES` after `current`, or the one before it, going round at either end. A value that is not a
/// choice (written by hand) goes to the choices next to it.
pub fn next_choice(current: u32, up: bool) -> u32 {
    let n = SCREEN_OFF_CHOICES.len();
    let i = match SCREEN_OFF_CHOICES.iter().position(|&c| c >= current) {
        Some(i) if SCREEN_OFF_CHOICES[i] == current => i,
        // Between two choices: one step up is the larger, one step down the smaller.
        Some(i) if up => return SCREEN_OFF_CHOICES[i],
        Some(i) => return SCREEN_OFF_CHOICES[i - 1],
        None if up => return SCREEN_OFF_CHOICES[0],
        None => return SCREEN_OFF_CHOICES[n - 1],
    };
    SCREEN_OFF_CHOICES[if up { (i + 1) % n } else { (i + n - 1) % n }]
}

/// `minutes` as Windows' list words it.
pub fn choice_text(minutes: u32) -> String {
    match minutes {
        0 => "Never".into(),
        1 => "1 minute".into(),
        m if m < 60 || m % 60 != 0 => format!("{m} minutes"),
        60 => "1 hour".into(),
        m => format!("{} hours", m / 60),
    }
}

/// The level as a percentage of `max`, rounded.
pub fn percent(level: u32, max: u32) -> u32 {
    if max == 0 {
        return 0;
    }
    (level * 100 + max / 2) / max
}

/// Where everything is: the system's places, or a test's.
pub struct Paths {
    /// The backlight class device.
    pub device: PathBuf,
    /// The adapter's `online`: 1 on the charger, anything else (or nothing) on battery.
    pub adapter: PathBuf,
    /// Where the level and the timers are kept.
    pub settings: PathBuf,
    /// Present once /data can be written (`apps::DATA_READY`).
    pub data_ready: PathBuf,
}

impl Paths {
    pub fn system() -> Paths {
        Paths {
            device: "/sys/class/backlight/backlight".into(),
            adapter: "/sys/class/power_supply/bq25710-charger/online".into(),
            settings: "/data/lucina/settings".into(),
            data_ready: crate::apps::DATA_READY.into(),
        }
    }
}

fn read_u32(path: &Path) -> Option<u32> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

pub struct Backlight {
    paths: Paths,
    pub max: u32,
    /// The level chosen: the light has it, unless it is dark from idling.
    pub level: u32,
    pub battery_min: u32,
    pub charger_min: u32,
    /// "Sleep after" and "Standby after" (minutes, 0 Never): the owner's defaults 15 and 60, kept in order by
    /// `lucina_standby::settle`.
    pub sleep_min: u32,
    pub standby_min: u32,
    /// Dark after idling: the next input brings `level` back.
    pub dark: bool,
    /// Who holds the screen on now, for Settings ("Video: playing video").
    pub held_by: Option<String>,
    /// What the adapter said when it was last read.
    on_charger: bool,
    /// /data has been looked at: what is saved there was restored, or what was chosen before it came was saved.
    synced: bool,
    /// Something was chosen before /data came up.
    unsaved: bool,
}

impl Backlight {
    /// The device, if the kernel has one. The firmware's level stays until a saved one is found on /data.
    pub fn open(paths: Paths) -> Option<Backlight> {
        let max = read_u32(&paths.device.join("max_brightness")).filter(|&m| m > 0)?;
        let (level, light) = start_level(read_u32(&paths.device.join("brightness"))?, max);
        let mut b = Backlight {
            paths,
            max,
            level,
            battery_min: BATTERY_DEFAULT_MIN,
            charger_min: CHARGER_DEFAULT_MIN,
            sleep_min: lucina_standby::SLEEP_DEFAULT_MIN,
            standby_min: lucina_standby::STANDBY_DEFAULT_MIN,
            dark: false,
            on_charger: false,
            held_by: None,
            synced: false,
            unsaved: false,
        };
        b.read_adapter();
        if light {
            b.write(level);
        }
        b.sync();
        Some(b)
    }

    /// Once /data is ready: the saved level and timers come back, or, when something was chosen before /data came up,
    /// that is saved instead. Cheap until then (one `stat`), so it can be asked every few seconds.
    pub fn sync(&mut self) {
        if self.synced || !self.paths.data_ready.exists() {
            return;
        }
        self.synced = true;
        if self.unsaved {
            self.save_all();
            return;
        }
        let dir = &self.paths.settings;
        if let Some(saved) = read_u32(&dir.join("brightness")) {
            self.level = restored_level(saved, self.max);
            if !self.dark {
                self.write(self.level);
            }
        }
        if let Some(m) = read_u32(&dir.join("screen-off-battery")) {
            self.battery_min = m;
        }
        if let Some(m) = read_u32(&dir.join("screen-off-charger")) {
            self.charger_min = m;
        }
        if let Some(m) = read_u32(&dir.join("sleep-after")) {
            self.sleep_min = m;
        }
        if let Some(m) = read_u32(&dir.join("standby-after")) {
            self.standby_min = m;
        }
        self.settle();
    }

    fn write(&self, level: u32) -> bool {
        match fs::write(self.paths.device.join("brightness"), level.to_string()) {
            Ok(()) => true,
            Err(e) => {
                eprintln!("lucina-shell: backlight: brightness {level}: {e}");
                false
            }
        }
    }

    /// Written next to the file and renamed over it, so that a crash leaves the old value or the new one.
    fn save(&self, name: &str, value: u32) {
        let dir = &self.paths.settings;
        let tmp = dir.join(format!(".{name}.tmp"));
        let done = fs::create_dir_all(dir)
            .and_then(|()| fs::write(&tmp, format!("{value}\n")))
            .and_then(|()| fs::rename(&tmp, dir.join(name)));
        if let Err(e) = done {
            eprintln!("lucina-shell: {}: {e}", dir.join(name).display());
        }
    }

    fn save_all(&self) {
        self.save("brightness", self.level);
        self.save("screen-off-battery", self.battery_min);
        self.save("screen-off-charger", self.charger_min);
        self.save("sleep-after", self.sleep_min);
        self.save("standby-after", self.standby_min);
    }

    fn changed(&mut self, name: &str, value: u32) {
        if self.synced {
            self.save(name, value);
        } else {
            self.unsaved = true;
        }
    }

    /// A brightness key or Left/Right: one step. True when the level changed.
    pub fn step(&mut self, up: bool) -> bool {
        // This laptop (max 100) steps by the Verus-proved rule; another panel by the same rule scaled to its range.
        let level = if self.max == crate::fnkeys::BRIGHTNESS_MAX {
            crate::fnkeys::brightness_step(self.level, up)
        } else {
            next_level(self.level, up, self.max)
        };
        if level == self.level || !self.write(level) {
            return false;
        }
        self.level = level;
        self.changed("brightness", level);
        true
    }

    /// The next choice of one of the two timers.
    pub fn step_minutes(&mut self, charger: bool, up: bool) {
        let (name, m) = if charger {
            ("screen-off-charger", &mut self.charger_min)
        } else {
            ("screen-off-battery", &mut self.battery_min)
        };
        *m = next_choice(*m, up);
        let m = *m;
        self.changed(name, m);
    }

    /// The next choice of "Sleep after" (`standby` false) or "Standby after". A standby that would come before the sleep is
    /// moved to it (`lucina_standby::settle`), and saved with it.
    pub fn step_sleep(&mut self, standby: bool, up: bool) {
        if standby {
            self.standby_min = next_choice(self.standby_min, up);
        } else {
            self.sleep_min = next_choice(self.sleep_min, up);
        }
        self.settle();
        let (sleep, standby) = (self.sleep_min, self.standby_min);
        self.changed("sleep-after", sleep);
        self.changed("standby-after", standby);
    }

    /// The settings in bounds, as `lucina_standby::settle` keeps them.
    fn settle(&mut self) {
        let t = lucina_standby::settle(self.times());
        self.sleep_min = t.sleep;
        self.standby_min = t.standby;
    }

    /// The three timers that count now: the screen-off for the adapter as last read, the sleep, the standby; settled.
    pub fn times(&self) -> lucina_standby::Times {
        lucina_standby::settle(lucina_standby::Times {
            off: self.minutes(),
            sleep: self.sleep_min,
            standby: self.standby_min,
        })
    }

    /// Reads the adapter again; missing or unreadable is battery.
    pub fn read_adapter(&mut self) {
        self.on_charger = read_u32(&self.paths.adapter) == Some(1);
    }

    /// The timer that counts now, by the adapter as last read.
    pub fn minutes(&self) -> u32 {
        if self.on_charger {
            self.charger_min
        } else {
            self.battery_min
        }
    }

    /// The light off, the level kept. False if the kernel refused.
    pub fn go_dark(&mut self) -> bool {
        if self.dark || !self.write(0) {
            return false;
        }
        self.dark = true;
        true
    }

    /// The kept level back.
    pub fn wake(&mut self) {
        if self.dark {
            self.dark = false;
            self.write(self.level);
        }
    }

    /// A device that is not there: for the tests of what the rows show, and for the headless preview, whose host has no light.
    pub fn unattached(max: u32, level: u32, battery_min: u32, charger_min: u32) -> Backlight {
        let gone = PathBuf::from("/nonexistent/lucina-shell-test");
        Backlight {
            paths: Paths {
                device: gone.clone(),
                adapter: gone.clone(),
                settings: gone.clone(),
                data_ready: gone,
            },
            max,
            level,
            battery_min,
            charger_min,
            sleep_min: lucina_standby::SLEEP_DEFAULT_MIN,
            standby_min: lucina_standby::STANDBY_DEFAULT_MIN,
            dark: false,
            on_charger: false,
            held_by: None,
            synced: false,
            unsaved: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sleep_and_standby_step_and_stay_in_order() {
        let mut b = Backlight::unattached(100, 50, 5, 10);
        assert_eq!(
            b.times(),
            lucina_standby::Times {
                off: 5,
                sleep: 15,
                standby: 60
            }
        );
        // Sleep up through the choices: 15 → 20 → 25 → 30 → 60 (an hour); the standby (60) moves with it past 60.
        for _ in 0..4 {
            b.step_sleep(false, true);
        }
        assert_eq!((b.sleep_min, b.standby_min), (60, 60));
        b.step_sleep(false, true);
        assert_eq!((b.sleep_min, b.standby_min), (120, 120));
        // The standby can not be stepped below the sleep; Never stays reachable for both.
        b.step_sleep(true, false);
        assert_eq!((b.sleep_min, b.standby_min), (120, 120));
        b.step_sleep(true, true);
        assert_eq!(b.standby_min, 180);
        let mut never = Backlight::unattached(100, 50, 5, 10);
        never.sleep_min = 1;
        never.step_sleep(false, false);
        assert_eq!(never.sleep_min, 0);
        assert_eq!(never.times().sleep, 0);
    }

    #[test]
    fn a_light_found_dark_is_lit_at_the_least_restored_level() {
        assert_eq!(start_level(0, 100), (5, true));
        assert_eq!(start_level(50, 100), (50, false));
        assert_eq!(start_level(500, 100), (100, false));
        assert_eq!(start_level(0, 10), (1, true));
    }

    #[test]
    fn a_hold_stops_the_timer_and_its_end_starts_it_again() {
        let min = 60_000_000;
        // Input at 0, a hold from before the deadline: nothing while held.
        assert_eq!(screen_off_at(0, 0, true, 1), None);
        // Released at 90 s: a minute from then, not from the input.
        assert_eq!(
            screen_off_at(0, 90_000_000, false, 1),
            Some(90_000_000 + min)
        );
        // Input after the hold ended counts from the input.
        assert_eq!(
            screen_off_at(120_000_000, 90_000_000, false, 1),
            Some(120_000_000 + min)
        );
        // Never stays never.
        assert_eq!(screen_off_at(0, 90_000_000, false, 0), None);
        // No hold ever: as before.
        assert_eq!(screen_off_at(5, 0, false, 2), idle_deadline(5, 2));
    }

    #[test]
    fn steps_stay_between_1_and_max() {
        assert_eq!(next_level(50, true, 100), 60);
        assert_eq!(next_level(50, false, 100), 40);
        assert_eq!(next_level(95, true, 100), 100);
        assert_eq!(next_level(100, true, 100), 100);
        assert_eq!(next_level(10, false, 100), 1);
        assert_eq!(next_level(1, false, 100), 1);
        assert_eq!(next_level(0, true, 100), 10);
        assert_eq!(next_level(5, true, 7), 7);
    }

    #[test]
    fn restore_clamps_as_systemd_backlight() {
        assert_eq!(restored_level(0, 100), 5);
        assert_eq!(restored_level(3, 100), 5);
        assert_eq!(restored_level(5, 100), 5);
        assert_eq!(restored_level(70, 100), 70);
        assert_eq!(restored_level(250, 100), 100);
        // A small device still never comes back dark.
        assert_eq!(restored_level(0, 10), 1);
    }

    #[test]
    fn deadline_after_the_last_input() {
        assert_eq!(idle_deadline(1_000_000, 0), None);
        assert_eq!(idle_deadline(1_000_000, 5), Some(301_000_000));
        assert_eq!(idle_deadline(0, 300), Some(18_000_000_000));
    }

    #[test]
    fn choices_go_round() {
        assert_eq!(next_choice(0, true), 1);
        assert_eq!(next_choice(5, true), 10);
        assert_eq!(next_choice(30, true), 60);
        assert_eq!(next_choice(300, true), 0);
        assert_eq!(next_choice(0, false), 300);
        assert_eq!(next_choice(10, false), 5);
        // Written by hand: to the choices around it.
        assert_eq!(next_choice(7, true), 10);
        assert_eq!(next_choice(7, false), 5);
        assert_eq!(next_choice(1000, true), 0);
        assert_eq!(next_choice(1000, false), 300);
    }

    #[test]
    fn choices_in_words() {
        let words: Vec<String> = SCREEN_OFF_CHOICES.iter().map(|&m| choice_text(m)).collect();
        assert_eq!(words[0], "Never");
        assert_eq!(words[1], "1 minute");
        assert_eq!(words[4], "5 minutes");
        assert_eq!(words[10], "1 hour");
        assert_eq!(words[14], "5 hours");
        assert_eq!(choice_text(90), "90 minutes");
    }

    #[test]
    fn percent_of_max() {
        assert_eq!(percent(50, 100), 50);
        assert_eq!(percent(1, 100), 1);
        assert_eq!(percent(7, 15), 47);
        assert_eq!(percent(3, 0), 0);
    }

    struct Dir(PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn fixture(name: &str) -> (Dir, Paths) {
        let root = std::env::temp_dir().join(format!(
            "lucina-shell-backlight-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let device = root.join("backlight");
        fs::create_dir_all(&device).unwrap();
        fs::write(device.join("max_brightness"), "100\n").unwrap();
        fs::write(device.join("brightness"), "50\n").unwrap();
        let paths = Paths {
            device,
            adapter: root.join("online"),
            settings: root.join("data/settings"),
            data_ready: root.join("data-ready"),
        };
        (Dir(root), paths)
    }

    fn brightness(p: &Path) -> String {
        fs::read_to_string(p.join("brightness"))
            .unwrap()
            .trim()
            .to_string()
    }

    #[test]
    fn no_device_no_backlight() {
        let (_d, mut p) = fixture("none");
        p.device = p.device.join("missing");
        assert!(Backlight::open(p).is_none());
    }

    #[test]
    fn firmware_level_stays_without_a_saved_one() {
        let (_d, p) = fixture("firmware");
        fs::write(&p.data_ready, "").unwrap();
        let device = p.device.clone();
        let b = Backlight::open(p).unwrap();
        assert_eq!((b.level, b.max), (50, 100));
        assert_eq!(brightness(&device), "50");
        assert_eq!((b.battery_min, b.charger_min), (5, 10));
    }

    #[test]
    fn saved_level_comes_back_when_data_is_ready() {
        let (_d, p) = fixture("restore");
        fs::create_dir_all(&p.settings).unwrap();
        fs::write(p.settings.join("brightness"), "2\n").unwrap();
        fs::write(p.settings.join("screen-off-battery"), "15\n").unwrap();
        let (device, ready) = (p.device.clone(), p.data_ready.clone());
        let mut b = Backlight::open(p).unwrap();
        // /data is not up yet: the firmware's level stays.
        assert_eq!(brightness(&device), "50");
        fs::write(&ready, "").unwrap();
        b.sync();
        assert_eq!(b.level, 5);
        assert_eq!(brightness(&device), "5");
        assert_eq!((b.battery_min, b.charger_min), (15, 10));
    }

    #[test]
    fn a_choice_before_data_is_saved_when_it_comes() {
        let (_d, p) = fixture("early");
        fs::create_dir_all(&p.settings).unwrap();
        fs::write(p.settings.join("brightness"), "90\n").unwrap();
        let (device, settings, ready) =
            (p.device.clone(), p.settings.clone(), p.data_ready.clone());
        let mut b = Backlight::open(p).unwrap();
        assert!(b.step(false));
        assert_eq!(brightness(&device), "40");
        fs::write(&ready, "").unwrap();
        b.sync();
        // What was chosen wins over what was saved before.
        assert_eq!(brightness(&device), "40");
        assert_eq!(
            fs::read_to_string(settings.join("brightness")).unwrap(),
            "40\n"
        );
        b.step_minutes(true, true);
        assert_eq!(
            fs::read_to_string(settings.join("screen-off-charger")).unwrap(),
            "15\n"
        );
        assert!(!settings.join(".screen-off-charger.tmp").exists());
    }

    #[test]
    fn dark_keeps_the_level() {
        let (_d, p) = fixture("dark");
        fs::write(&p.adapter, "1\n").unwrap();
        let device = p.device.clone();
        let mut b = Backlight::open(p).unwrap();
        assert_eq!(b.minutes(), 10);
        assert!(b.go_dark());
        assert!(!b.go_dark());
        assert_eq!(brightness(&device), "0");
        b.wake();
        assert!(!b.dark);
        assert_eq!(brightness(&device), "50");
    }

    #[test]
    fn adapter_missing_is_battery() {
        let (_d, p) = fixture("adapter");
        let adapter = p.adapter.clone();
        let mut b = Backlight::open(p).unwrap();
        assert_eq!(b.minutes(), 5);
        fs::write(&adapter, "1\n").unwrap();
        b.read_adapter();
        assert_eq!(b.minutes(), 10);
        fs::write(&adapter, "0\n").unwrap();
        b.read_adapter();
        assert_eq!(b.minutes(), 5);
    }
}
