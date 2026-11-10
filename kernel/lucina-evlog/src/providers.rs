//! Every provider and event ID Juno writes, in one table, with the Windows event it stands for. Windows keeps this in
//! manifests and message DLLs and an event whose DLL is missing reads "the description cannot be found"; here the table is
//! documentation and a check (`known`), and each event carries its own words anyway.

use crate::event::{Channel, Level};

pub const KERNEL_POWER: &str = "Juno-Kernel-Power";
pub const CRASH: &str = "Juno-Crash";
pub const KERNEL_LIVE: &str = "Juno-Kernel-Live";
pub const APPS: &str = "Juno-Apps";
pub const NET: &str = "Juno-Net";
pub const POWER: &str = "Juno-Power";
pub const SETUP: &str = "Juno-Setup";
pub const EVENTLOG: &str = "Juno-EventLog";

/// One kind of event.
#[derive(Clone, Copy, Debug)]
pub struct Def {
    pub provider: &'static str,
    pub id: u32,
    pub name: &'static str,
    pub level: Level,
    pub channel: Channel,
    /// The Windows event(s) it stands for.
    pub windows: &'static str,
    pub meaning: &'static str,
}

pub mod id {
    // Juno-Kernel-Power
    pub const BOOT_STARTED: u32 = 12;
    pub const SHUT_DOWN: u32 = 13;
    pub const UNEXPECTED_RESET: u32 = 41;
    pub const RESTART_REQUESTED: u32 = 1074;
    // Juno-Crash
    pub const KERNEL_CRASH: u32 = 1001;
    pub const SOFT_LOCKUP: u32 = 1002;
    pub const WATCHDOG_BARK: u32 = 1003;
    pub const USER_WATCHDOG: u32 = 1004;
    // Juno-Kernel-Live
    pub const KERNEL_WARNING: u32 = 141;
    pub const MODEM_FAILED: u32 = 331;
    // Juno-Apps
    pub const APP_CRASHED: u32 = 1000;
    // Juno-Net
    pub const LINK_STOPPED: u32 = 4201;
    // Juno-Power
    pub const BATTERY_LOW: u32 = 2;
    pub const POWER_SOURCE: u32 = 105;
    // Juno-Setup
    pub const GENERATION_INSTALLED: u32 = 2;
    // Juno-EventLog
    pub const LOG_CLEARED: u32 = 104;
    pub const EVENTS_LOST: u32 = 1101;
}

pub const DEFS: &[Def] = &[
    Def {
        provider: KERNEL_POWER,
        id: id::BOOT_STARTED,
        name: "Boot started",
        level: Level::Info,
        channel: Channel::System,
        windows: "Kernel-General 12, EventLog 6005",
        meaning: "Juno started; the PMIC's power-on and power-off reasons say how, and why the boot before ended",
    },
    Def {
        provider: KERNEL_POWER,
        id: id::SHUT_DOWN,
        name: "Shut down cleanly",
        level: Level::Info,
        channel: Channel::System,
        windows: "Kernel-General 13, EventLog 6006",
        meaning: "the boot before asked for its own restart, power-off or reload",
    },
    Def {
        provider: KERNEL_POWER,
        id: id::UNEXPECTED_RESET,
        name: "Unexpected reset",
        level: Level::Critical,
        channel: Channel::System,
        windows: "Kernel-Power 41, EventLog 6008",
        meaning: "the boot before stopped without a crash report: a hang with every CPU stuck, a firmware reset, or lost power",
    },
    Def {
        provider: KERNEL_POWER,
        id: id::RESTART_REQUESTED,
        name: "Restart requested",
        level: Level::Warning,
        channel: Channel::System,
        windows: "User32 1074",
        meaning: "a program restarted Juno and said why (lucina-watchd after the network stayed dead)",
    },
    Def {
        provider: CRASH,
        id: id::KERNEL_CRASH,
        name: "Kernel crash",
        level: Level::Critical,
        channel: Channel::System,
        windows: "BugCheck 1001",
        meaning: "the kernel panicked, showed the crash screen and restarted",
    },
    Def {
        provider: CRASH,
        id: id::SOFT_LOCKUP,
        name: "CPU stopped scheduling",
        level: Level::Critical,
        channel: Channel::System,
        windows: "BugCheck 1001 (0x133 DPC_WATCHDOG_VIOLATION)",
        meaning: "a CPU ran kernel code for too long without letting anything else run",
    },
    Def {
        provider: CRASH,
        id: id::WATCHDOG_BARK,
        name: "Hardware watchdog",
        level: Level::Critical,
        channel: Channel::System,
        windows: "BugCheck 1001 (0x1CA SYNTHETIC_WATCHDOG_TIMEOUT)",
        meaning: "the hardware watchdog was about to reset the machine and the crash path caught it first",
    },
    Def {
        provider: CRASH,
        id: id::USER_WATCHDOG,
        name: "Userspace watchdog",
        level: Level::Critical,
        channel: Channel::System,
        windows: "none (systemd's RuntimeWatchdogSec)",
        meaning: "lucina-watchd stopped petting /dev/watchdog, so the kernel crashed on purpose",
    },
    Def {
        provider: KERNEL_LIVE,
        id: id::KERNEL_WARNING,
        name: "Kernel warning",
        level: Level::Warning,
        channel: Channel::System,
        windows: "LiveKernelEvent 141, 117, 1a1",
        meaning: "a driver or the kernel reported an error and carried on; repeats are counted, not repeated",
    },
    Def {
        provider: KERNEL_LIVE,
        id: id::MODEM_FAILED,
        name: "Modem DSP failed",
        level: Level::Error,
        channel: Channel::System,
        windows: "LiveKernelEvent 14b (SOC_SUBSYSTEM_FAILURE)",
        meaning: "the modem DSP, which runs Wi-Fi, failed (its watchdog or its fatal bit) while Juno kept running (patch 1057)",
    },
    Def {
        provider: APPS,
        id: id::APP_CRASHED,
        name: "App crashed",
        level: Level::Error,
        channel: Channel::Apps,
        windows: "Application Error 1000, WER 1001",
        meaning: "an app ended on a signal (segmentation fault, abort)",
    },
    Def {
        provider: NET,
        id: id::LINK_STOPPED,
        name: "Network link stopped",
        level: Level::Warning,
        channel: Channel::System,
        windows: "Tcpip 4201",
        meaning: "a network link went down",
    },
    Def {
        provider: POWER,
        id: id::BATTERY_LOW,
        name: "Battery low",
        level: Level::Warning,
        channel: Channel::System,
        windows: "Kernel-Power 42 (sleep), Battery warnings",
        meaning: "the battery fell to 10 % or less without a charger",
    },
    Def {
        provider: POWER,
        id: id::POWER_SOURCE,
        name: "Power source changed",
        level: Level::Info,
        channel: Channel::System,
        windows: "Kernel-Power 105",
        meaning: "the charger was plugged in or out (the kernel's charge: lines carry the timings)",
    },
    Def {
        provider: SETUP,
        id: id::GENERATION_INSTALLED,
        name: "Generation installed",
        level: Level::Info,
        channel: Channel::Setup,
        windows: "WindowsUpdateClient 19",
        meaning: "a new generation of LucinaOS booted for the first time",
    },
    Def {
        provider: EVENTLOG,
        id: id::LOG_CLEARED,
        name: "Log cleared",
        level: Level::Info,
        channel: Channel::System,
        windows: "EventLog 104, 1102",
        meaning: "someone cleared the event log",
    },
    Def {
        provider: EVENTLOG,
        id: id::EVENTS_LOST,
        name: "Events lost",
        level: Level::Warning,
        channel: Channel::System,
        windows: "EventLog 1101",
        meaning: "the hub dropped events before the log read them",
    },
];

pub fn known(provider: &str, id: u32) -> Option<&'static Def> {
    DEFS.iter().find(|d| d.provider == provider && d.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_per_provider() {
        for (i, a) in DEFS.iter().enumerate() {
            for b in &DEFS[i + 1..] {
                assert!(
                    !(a.provider == b.provider && a.id == b.id),
                    "{} {}",
                    a.provider,
                    a.id
                );
            }
        }
    }
}
