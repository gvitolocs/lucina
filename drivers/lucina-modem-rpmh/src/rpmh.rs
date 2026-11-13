//! The cache of `drivers/soc/qcom/rpmh.c`: what each resource should hold while the application processor's cluster sleeps
//! and after it wakes. Linux 7.1 (GPL-2.0; Copyright The Linux Foundation).
//!
//! `cache_rpm_request` sees every vote, whatever its state. [`Cache::flush`] is `rpmh_flush`: it leaves commands in the
//! SLEEP and WAKE TCSes, and only for resources whose two values differ, for the hardware to run at those transitions.
//! Linux flushes when the last CPU goes down; this kernel never powers the cluster down, so it flushes once after its votes,
//! which leaves the same commands in the same registers should a later change start doing that.

use alloc::string::String;
use alloc::vec::Vec;

use lucina_qcom_soc::io::RegIo;

use crate::arc::State;
use crate::rsc::{Rsc, RscError, SLEEP_TCS, WAKE_TCS};

const UNSET: u32 = u32::MAX;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct Req {
    addr: u32,
    sleep: u32,
    wake: u32,
}

/// `ctrlr->cache`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cache {
    reqs: Vec<Req>,
}

impl Cache {
    pub fn new() -> Self {
        Self::default()
    }

    /// `cache_rpm_request`.
    pub fn record(&mut self, state: State, addr: u32, data: u32) {
        let at = match self.reqs.iter().position(|r| r.addr == addr) {
            Some(at) => at,
            None => {
                self.reqs.push(Req {
                    addr,
                    sleep: UNSET,
                    wake: UNSET,
                });
                self.reqs.len() - 1
            }
        };
        match state {
            State::Active | State::Wake => self.reqs[at].wake = data,
            State::Sleep => self.reqs[at].sleep = data,
        }
    }

    /// Every resource the cache has seen, in `rpmh_flush`'s order: `(addr, sleep, wake)`, `None` for a value never sent.
    pub fn entries(&self) -> impl Iterator<Item = (u32, Option<u32>, Option<u32>)> + '_ {
        let set = |v: u32| (v != UNSET).then_some(v);
        self.reqs
            .iter()
            .map(move |r| (r.addr, set(r.sleep), set(r.wake)))
    }

    /// `is_req_valid` over the cache, in `rpmh_flush`'s order: `(addr, sleep, wake)`.
    pub fn valid(&self) -> impl Iterator<Item = (u32, u32, u32)> + '_ {
        self.reqs
            .iter()
            .filter(|r| r.sleep != UNSET && r.wake != UNSET && r.sleep != r.wake)
            .map(|r| (r.addr, r.sleep, r.wake))
    }

    /// One line per resource of the cache, `name` naming its address: the SLEEP and WAKE values held, and what [`Cache::flush`]
    /// would leave in the SLEEP and WAKE TCSes for it (nothing where the two are the same, or one was never sent).
    pub fn report(&self, name: impl Fn(u32) -> String) -> String {
        let mut s = String::new();
        let v = |x: Option<u32>| x.map_or(String::from("-"), |x| alloc::format!("{x:#x}"));
        for (addr, sleep, wake) in self.entries() {
            let flush = match (sleep, wake) {
                (Some(z), Some(w)) if z != w => {
                    alloc::format!("flush writes sleep {z:#x}, wake {w:#x}")
                }
                _ => String::from("flush writes nothing"),
            };
            s.push_str(&alloc::format!(
                "{addr:#x} {}: sleep {} wake {}; {flush}\n",
                name(addr),
                v(sleep),
                v(wake)
            ));
        }
        s
    }

    /// `rpmh_flush`. Each `send_single`: wake sets are complete and sleep sets are not.
    pub fn flush<R: RegIo>(&self, rsc: &mut Rsc<R>) -> Result<(), RscError> {
        rsc.invalidate_ctrl()?;
        for (addr, sleep, wake) in self.valid() {
            rsc.write_ctrl(SLEEP_TCS, addr, sleep, false)?;
            rsc.write_ctrl(WAKE_TCS, addr, wake, true)?;
        }
        Ok(())
    }
}
