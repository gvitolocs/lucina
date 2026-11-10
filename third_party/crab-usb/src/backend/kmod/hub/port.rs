//! Port handling shared by the xHCI root hub and external hubs, after Linux's `drivers/usb/core/hub.c`: the
//! connect debounce, the port reset with its waits and retries, and the enumeration retries. Values and order are
//! Linux's; the decisions are pure functions so that both hubs and the tests run the same rules.

/// `PORT_INIT_TRIES` (hub.c:2896): enumeration attempts per connect.
pub(crate) const PORT_INIT_TRIES: usize = 4;
/// `PORT_RESET_TRIES` (hub.c:2892): resets per attempt.
pub(crate) const PORT_RESET_TRIES: usize = 5;
/// `SET_ADDRESS_TRIES` (hub.c:2893): Set Address (here the Address Device command) per descriptor round, 200 ms
/// apart (`SET_ADDRESS_RETRY_DELAY`).
pub(crate) const SET_ADDRESS_TRIES: usize = 2;
/// `GET_DESCRIPTOR_TRIES` (hub.c:2894): rounds of Set Address and the 8-byte descriptor read per attempt, 100 ms
/// apart (`GET_DESCRIPTOR_RETRY_DELAY`).
pub(crate) const GET_DESCRIPTOR_TRIES: usize = 2;
/// `GET_MAXPACKET0_TRIES` (hub.c:2895): reads of the 8-byte descriptor in one round (`get_bMaxPacketSize0`).
pub(crate) const GET_MAXPACKET0_TRIES: usize = 3;
/// The `msleep(200)` after a failed Set Address in `hub_port_init`.
pub(crate) const SET_ADDRESS_RETRY_DELAY: u64 = 200;
/// The `msleep(100)` between two rounds of `hub_port_init`'s descriptor loop.
pub(crate) const GET_DESCRIPTOR_RETRY_DELAY: u64 = 100;
/// The `msleep(10)` after Set Address in `hub_port_init`, before the descriptor read.
pub(crate) const SET_ADDRESS_SETTLE_TIME: u64 = 10;
/// `HUB_ROOT_RESET_TIME` (hub.c:2901): first wait after a root port reset, ms.
pub(crate) const HUB_ROOT_RESET_TIME: u64 = 60;
/// `HUB_SHORT_RESET_TIME` (hub.c:2902): first wait after an external hub port reset, ms.
pub(crate) const HUB_SHORT_RESET_TIME: u64 = 10;
/// `HUB_BH_RESET_TIME` (hub.c:2903): first wait after a warm reset, ms.
pub(crate) const HUB_BH_RESET_TIME: u64 = 50;
/// `HUB_LONG_RESET_TIME` (hub.c:2904): the wait after two short ones, and for low speed, ms.
pub(crate) const HUB_LONG_RESET_TIME: u64 = 200;
/// `HUB_RESET_TIMEOUT` (hub.c:2905): reset wait limit, ms.
pub(crate) const HUB_RESET_TIMEOUT: u64 = 800;
/// Reset recovery after a successful reset: TRSTRCY 10 ms plus Linux's 40 ms (`hub_port_reset`).
pub(crate) const RESET_RECOVERY_TIME: u64 = 10 + 40;

/// `HUB_DEBOUNCE_STEP` (hub.c:139), ms.
pub(crate) const HUB_DEBOUNCE_STEP: u32 = 25;
/// `HUB_DEBOUNCE_STABLE` (hub.c:140), ms.
pub(crate) const HUB_DEBOUNCE_STABLE: u32 = 100;
/// `HUB_DEBOUNCE_TIMEOUT` (hub.c:138), ms.
pub(crate) const HUB_DEBOUNCE_TIMEOUT: u32 = 2000;

/// The power-on-to-power-good wait of a hub (`hub_power_on_good_delay`, hub.h:165-173): `bPwrOn2PwrGood` x 2 ms,
/// and at least 100 ms for an external hub.
pub(crate) const fn power_on_good_delay(pwr_on_2_pwr_good: u8, root_hub: bool) -> u64 {
    let delay = pwr_on_2_pwr_good as u64 * 2;
    if root_hub || delay > 100 { delay } else { 100 }
}

/// `bPwrOn2PwrGood` of Linux's xHCI root hubs (xhci-hub.c:295 and :348): 10 for USB 2, 50 for USB 3.
pub(crate) const fn xhci_root_pwr_on_2_pwr_good(usb3: bool) -> u8 {
    if usb3 { 50 } else { 10 }
}

/// What one sample of the debounce decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DebounceStep {
    /// Connected and stable for `HUB_DEBOUNCE_STABLE`.
    Stable,
    /// Not stable by `HUB_DEBOUNCE_TIMEOUT`: give up.
    TimedOut { clear_change: bool },
    /// Sleep `HUB_DEBOUNCE_STEP` and sample again.
    Again { clear_change: bool },
}

/// `hub_port_debounce(hub, port, must_be_connected = true)` (hub.c:4696-4745). The first sample is taken at once
/// and can never count; a sample counts only without a connect change and with the same connection as the one
/// before; a connected port must stay so for 100 ms; every connect change seen is cleared.
#[derive(Debug, Clone, Default)]
pub(crate) struct Debounce {
    total_ms: u32,
    stable_ms: u32,
    last_connection: Option<bool>,
}

impl Debounce {
    pub(crate) const fn new() -> Self {
        Self {
            total_ms: 0,
            stable_ms: 0,
            last_connection: None,
        }
    }

    pub(crate) fn sample(&mut self, connected: bool, connect_change: bool) -> DebounceStep {
        if !connect_change && Some(connected) == self.last_connection {
            if connected {
                self.stable_ms += HUB_DEBOUNCE_STEP;
            }
            if self.stable_ms >= HUB_DEBOUNCE_STABLE {
                return DebounceStep::Stable;
            }
        } else {
            self.stable_ms = 0;
            self.last_connection = Some(connected);
        }

        let clear_change = connect_change;
        if self.total_ms >= HUB_DEBOUNCE_TIMEOUT {
            DebounceStep::TimedOut { clear_change }
        } else {
            self.total_ms += HUB_DEBOUNCE_STEP;
            DebounceStep::Again { clear_change }
        }
    }

    #[cfg(test)]
    pub(crate) fn total_ms(&self) -> u32 {
        self.total_ms
    }
}

/// A port as `hub_port_wait_reset` and `hub_port_reset` look at it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ResetSample {
    pub resetting: bool,
    pub connected: bool,
    pub enabled: bool,
    pub connect_change: bool,
    /// A USB 3 port in SS.Inactive or Compliance Mode (`hub_port_warm_reset_required`).
    pub warm_reset_required: bool,
}

/// The result of one reset wait (`hub_port_wait_reset`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResetWaitResult {
    /// Reset over, connected and enabled.
    Done,
    /// Still resetting, or not enabled (-EBUSY).
    Busy,
    /// Gone, or a USB 3 link that needs a warm reset (-ENOTCONN).
    NotConnected,
    /// A USB 2 port saw a connect change: clear it and reset again (-EAGAIN).
    Again,
}

/// The wait schedule of `hub_port_wait_reset` (hub.c:2953-2989): sleep `delay`, sample, until the reset is over
/// and the port connected or `HUB_RESET_TIMEOUT` is spent; after two short waits the delay becomes
/// `HUB_LONG_RESET_TIME`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ResetWait {
    delay_time: u64,
    delay: u64,
}

impl ResetWait {
    /// Starts a wait; the caller sleeps `first_sleep()` before the first sample.
    pub(crate) const fn new(delay: u64) -> Self {
        Self {
            delay_time: 0,
            delay,
        }
    }

    pub(crate) const fn first_sleep(&self) -> u64 {
        self.delay
    }

    /// After a sample: `Some(ms)` to sleep and sample again, `None` to stop and classify the sample.
    pub(crate) fn after_sample(&mut self, sample: &ResetSample) -> Option<u64> {
        if !sample.resetting && sample.connected {
            return None;
        }
        if self.delay_time >= 2 * HUB_SHORT_RESET_TIME {
            self.delay = HUB_LONG_RESET_TIME;
        }
        self.delay_time += self.delay;
        (self.delay_time < HUB_RESET_TIMEOUT).then_some(self.delay)
    }
}

/// How `hub_port_wait_reset` reads the last sample (hub.c:2991-3015).
pub(crate) fn classify_reset(sample: &ResetSample, superspeed: bool) -> ResetWaitResult {
    if sample.resetting {
        ResetWaitResult::Busy
    } else if sample.warm_reset_required || !sample.connected {
        ResetWaitResult::NotConnected
    } else if !superspeed && sample.connect_change {
        ResetWaitResult::Again
    } else if !sample.enabled {
        ResetWaitResult::Busy
    } else {
        ResetWaitResult::Done
    }
}

/// Whether `hub_port_reset` stops retrying after reset `try_index` (0-based) ended with `result`:
/// on success or disconnect, and on -EBUSY at the last try. Other results reset again with
/// `HUB_LONG_RESET_TIME`.
pub(crate) fn reset_try_ends(try_index: usize, result: ResetWaitResult) -> bool {
    matches!(
        result,
        ResetWaitResult::Done | ResetWaitResult::NotConnected
    ) || (result == ResetWaitResult::Busy && try_index == PORT_RESET_TRIES - 1)
}

/// Whether attempt `attempt` (0-based) of `hub_port_connect` is followed by a power cycle of the port: halfway
/// through the tries.
pub(crate) const fn power_cycle_after(attempt: usize) -> bool {
    attempt == (PORT_INIT_TRIES - 1) / 2
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::*;

    #[test]
    fn debounce_is_stable_at_the_fifth_connected_sample() {
        let mut d = Debounce::new();
        for _ in 0..4 {
            assert_eq!(
                d.sample(true, false),
                DebounceStep::Again {
                    clear_change: false
                }
            );
        }
        assert_eq!(d.total_ms(), 100);
        assert_eq!(d.sample(true, false), DebounceStep::Stable);
    }

    #[test]
    fn a_connect_change_restarts_the_debounce_and_is_cleared() {
        let mut d = Debounce::new();
        assert_eq!(
            d.sample(true, false),
            DebounceStep::Again {
                clear_change: false
            }
        );
        assert_eq!(
            d.sample(true, false),
            DebounceStep::Again {
                clear_change: false
            }
        );
        assert_eq!(
            d.sample(true, true),
            DebounceStep::Again { clear_change: true }
        );
        for _ in 0..3 {
            assert_eq!(
                d.sample(true, false),
                DebounceStep::Again {
                    clear_change: false
                }
            );
        }
        assert_eq!(d.sample(true, false), DebounceStep::Stable);
    }

    #[test]
    fn a_disconnect_restarts_the_debounce() {
        // Planted bug: not resetting the count on a miss would be stable two samples earlier.
        let mut d = Debounce::new();
        for _ in 0..3 {
            assert_eq!(
                d.sample(true, false),
                DebounceStep::Again {
                    clear_change: false
                }
            );
        }
        assert_eq!(
            d.sample(false, false),
            DebounceStep::Again {
                clear_change: false
            }
        );
        for _ in 0..4 {
            assert_eq!(
                d.sample(true, false),
                DebounceStep::Again {
                    clear_change: false
                }
            );
        }
        assert_eq!(d.sample(true, false), DebounceStep::Stable);
    }

    #[test]
    fn a_disconnected_port_times_out_at_2000_ms() {
        let mut d = Debounce::new();
        for _ in 0..80 {
            assert_eq!(
                d.sample(false, false),
                DebounceStep::Again {
                    clear_change: false
                }
            );
        }
        assert_eq!(d.total_ms(), 2000);
        assert_eq!(
            d.sample(false, false),
            DebounceStep::TimedOut {
                clear_change: false
            }
        );
    }

    #[test]
    fn a_flapping_port_times_out_at_2000_ms() {
        let mut d = Debounce::new();
        let mut connected = true;
        for _ in 0..80 {
            assert_eq!(
                d.sample(connected, false),
                DebounceStep::Again {
                    clear_change: false
                }
            );
            connected = !connected;
        }
        assert_eq!(
            d.sample(connected, false),
            DebounceStep::TimedOut {
                clear_change: false
            }
        );
    }

    fn resetting() -> ResetSample {
        ResetSample {
            resetting: true,
            connected: true,
            enabled: false,
            connect_change: false,
            warm_reset_required: false,
        }
    }

    #[test]
    fn the_reset_wait_follows_linux_schedule() {
        // Root port: 60 ms, 60 ms, then 200 ms steps while the time spent stays below 800 ms.
        let mut wait = ResetWait::new(HUB_ROOT_RESET_TIME);
        let mut sleeps = Vec::from([wait.first_sleep()]);
        while let Some(ms) = wait.after_sample(&resetting()) {
            sleeps.push(ms);
        }
        assert_eq!(sleeps, [60, 60, 200, 200, 200]);

        // External hub port: 10, 10, 10, then 200 ms steps.
        let mut wait = ResetWait::new(HUB_SHORT_RESET_TIME);
        let mut sleeps = Vec::from([wait.first_sleep()]);
        while let Some(ms) = wait.after_sample(&resetting()) {
            sleeps.push(ms);
        }
        assert_eq!(sleeps, [10, 10, 10, 200, 200, 200]);
    }

    #[test]
    fn the_reset_wait_stops_once_the_reset_is_over_and_connected() {
        let mut wait = ResetWait::new(HUB_ROOT_RESET_TIME);
        let done = ResetSample {
            resetting: false,
            enabled: true,
            ..resetting()
        };
        assert_eq!(wait.after_sample(&done), None);
        assert_eq!(classify_reset(&done, false), ResetWaitResult::Done);
    }

    #[test]
    fn reset_results_follow_hub_port_wait_reset() {
        let base = ResetSample {
            resetting: false,
            connected: true,
            enabled: true,
            connect_change: false,
            warm_reset_required: false,
        };
        assert_eq!(classify_reset(&resetting(), false), ResetWaitResult::Busy);
        assert_eq!(
            classify_reset(
                &ResetSample {
                    connected: false,
                    ..base
                },
                false
            ),
            ResetWaitResult::NotConnected
        );
        assert_eq!(
            classify_reset(
                &ResetSample {
                    warm_reset_required: true,
                    ..base
                },
                true
            ),
            ResetWaitResult::NotConnected
        );
        let changed = ResetSample {
            connect_change: true,
            ..base
        };
        assert_eq!(classify_reset(&changed, false), ResetWaitResult::Again);
        assert_eq!(
            classify_reset(&changed, true),
            ResetWaitResult::Done,
            "USB 3 ignores the bounce"
        );
        assert_eq!(
            classify_reset(
                &ResetSample {
                    enabled: false,
                    ..base
                },
                false
            ),
            ResetWaitResult::Busy
        );
    }

    #[test]
    fn resets_are_retried_up_to_port_reset_tries() {
        assert!(reset_try_ends(0, ResetWaitResult::Done));
        assert!(reset_try_ends(0, ResetWaitResult::NotConnected));
        for try_index in 0..PORT_RESET_TRIES - 1 {
            assert!(!reset_try_ends(try_index, ResetWaitResult::Busy));
            assert!(!reset_try_ends(try_index, ResetWaitResult::Again));
        }
        assert!(reset_try_ends(PORT_RESET_TRIES - 1, ResetWaitResult::Busy));
    }

    #[test]
    fn enumeration_power_cycles_once_halfway_through_four_tries() {
        assert_eq!(PORT_INIT_TRIES, 4);
        let cycles = (0..PORT_INIT_TRIES)
            .filter(|&a| power_cycle_after(a))
            .collect::<Vec<_>>();
        assert_eq!(cycles, [1]);
    }

    #[test]
    fn power_good_delays_are_linux_hub_power_on_good_delay() {
        assert_eq!(
            power_on_good_delay(xhci_root_pwr_on_2_pwr_good(false), true),
            20
        );
        assert_eq!(
            power_on_good_delay(xhci_root_pwr_on_2_pwr_good(true), true),
            100
        );
        assert_eq!(
            power_on_good_delay(10, false),
            100,
            "external hubs wait at least 100 ms"
        );
        assert_eq!(power_on_good_delay(100, false), 200);
    }
}
