use alloc::sync::Arc;
use core::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use ax_sync::{SpinLock as Mutex, SpinRwLock as RwLock};
use dma_api::DmaDirection;
use mbarrier::wmb;
use usb_if::err::TransferError;
use xhci::{
    registers::doorbell,
    ring::trb::{
        command,
        event::{CommandCompletion, CompletionCode},
    },
};

use super::{host::halt_now, quarantine::DmaQuarantine, reg::XhciRegisters, ring::SendRing};
use crate::{
    BusAddr,
    err::ConvertXhciError,
    osal::{HandshakeError, Kernel, Sleeper, handshake, wait_until},
    queue::{Finished, TWaiter},
};

/// Linux `XHCI_CMD_DEFAULT_TIMEOUT` (xhci.h:1323, 5000 ms): a command not completed by then is aborted.
const XHCI_CMD_DEFAULT_TIMEOUT: Duration = Duration::from_millis(5000);
/// `xhci_abort_cmd_ring`: CRR must clear within 5 s, or the host is taken as dead (xhci-ring.c:525-534).
const XHCI_CMD_ABORT_TIMEOUT: Duration = Duration::from_micros(5 * 1000 * 1000);
/// `xhci_abort_cmd_ring`: how long to wait for the Command Ring Stopped event once CRR is clear (2000 ms).
const XHCI_CMD_STOP_EVENT_TIMEOUT: Duration = Duration::from_millis(2000);
/// Poll interval while a command is outstanding; ours. A completion that the next event drain delivers costs no
/// sleep at all (see `wait_until`).
const CMD_POLL_STEP: Duration = Duration::from_millis(1);

// CRCR bits (xHCI 5.4.5).
const CRCR_CA: u64 = 1 << 2;
const CRCR_CRR: u32 = 1 << 3;

#[derive(Clone)]
pub struct CommandRing {
    inner: Arc<Mutex<Inner>>,
    /// Shared with the controller and its event handler: set once the controller is dead (Linux
    /// `XHCI_STATE_DYING`). A dead controller takes no command.
    dead: Arc<AtomicBool>,
    /// Set once HCH was seen after the controller died: it then does no more DMA, and what it owned may be freed.
    /// A dead controller that is not known to be halted (HCH never set, or registers reading all ones) may still be
    /// running its schedules.
    halted: Arc<AtomicBool>,
    /// The controller's DMA quarantine, shared with every device and endpoint through this ring.
    quarantine: DmaQuarantine,
}

impl CommandRing {
    pub fn new(
        direction: DmaDirection,
        dma: &Kernel,
        reg: Arc<RwLock<XhciRegisters>>,
        dead: Arc<AtomicBool>,
    ) -> crate::err::Result<Self> {
        let ring = SendRing::new(direction, dma)?;
        let inner = Inner {
            ring,
            reg,
            kernel: dma.clone(),
        };
        Ok(Self {
            inner: Arc::new(Mutex::new(inner)),
            dead,
            halted: Arc::new(AtomicBool::new(false)),
            quarantine: DmaQuarantine::default(),
        })
    }

    pub fn bus_addr(&self) -> crate::BusAddr {
        // SAFETY: command-ring access excludes local xHCI event re-entry.
        let inner = unsafe { self.inner.lock_raw() };
        inner.ring.bus_addr()
    }

    pub fn cycle(&self) -> bool {
        // SAFETY: command-ring access excludes local xHCI event re-entry.
        let inner = unsafe { self.inner.lock_raw() };
        inner.ring.cycle()
    }

    pub fn finished_handle(&self) -> Finished<CommandCompletion> {
        // SAFETY: command-ring access excludes local xHCI event re-entry.
        let inner = unsafe { self.inner.lock_raw() };
        inner.ring.finished_handle()
    }

    /// Whether the controller is dead (a fault in USBSTS, or a command ring that could not be stopped).
    pub fn is_dead(&self) -> bool {
        self.dead.load(Ordering::Acquire)
    }

    /// Whether the controller is dead and HCH was seen after that: it does no DMA any more.
    pub fn is_halted(&self) -> bool {
        self.halted.load(Ordering::Acquire)
    }

    /// Dead but not known to be halted: it may still read and write the memory it was given, which must then be
    /// quarantined, not freed.
    pub fn may_still_dma(&self) -> bool {
        self.is_dead() && !self.is_halted()
    }

    /// The flag the event handler sets when it halts a faulted controller itself.
    pub(crate) fn halted_flag(&self) -> Arc<AtomicBool> {
        self.halted.clone()
    }

    pub(crate) fn quarantine(&self) -> &DmaQuarantine {
        &self.quarantine
    }

    pub async fn cmd_request(
        &mut self,
        trb: command::Allowed,
    ) -> Result<CommandCompletion, TransferError> {
        if self.is_dead() {
            return Err(TransferError::NoDevice);
        }
        let stop_endpoint = matches!(trb, command::Allowed::StopEndpoint(_));
        let (waiter, trb_addr, kernel) = {
            // SAFETY: command submission excludes local xHCI event re-entry.
            let mut inner = unsafe { self.inner.lock_raw() };
            let trb_addr = inner.ring.enque_command(trb);
            let waiter = inner.ring.take_finished_future(trb_addr);
            wmb();
            inner
                .reg
                .write()
                .doorbell
                .write_volatile_at(0, doorbell::Register::default());
            (waiter, trb_addr, inner.kernel.clone())
        };

        let res = match wait_for_completion(&kernel, &waiter, &self.dead, XHCI_CMD_DEFAULT_TIMEOUT).await
        {
            Some(res) => res,
            None if self.is_dead() => return Err(TransferError::NoDevice),
            None => {
                self.handle_command_timeout(&kernel, &waiter, trb_addr, stop_endpoint)
                    .await;
                return Err(if self.is_dead() {
                    TransferError::NoDevice
                } else {
                    TransferError::Timeout
                });
            }
        };

        match res.completion_code() {
            Ok(code) => code.to_result()?,
            Err(e) => Err(TransferError::Other(anyhow!("Command failed: {e:?}")))?,
        }

        Ok(res)
    }

    /// Linux's `xhci_handle_command_timeout`, for the one command this ring has outstanding. The command fails;
    /// what is left to do is to get the ring stopped with the command turned into a no-op, so that it cannot run
    /// later, or to give the controller up.
    async fn handle_command_timeout(
        &self,
        kernel: &Kernel,
        waiter: &TWaiter<CommandCompletion>,
        trb_addr: BusAddr,
        stop_endpoint: bool,
    ) {
        let reg = {
            // SAFETY: command-ring access excludes local xHCI event re-entry.
            let inner = unsafe { self.inner.lock_raw() };
            inner.reg.clone()
        };
        let status = reg.read().operational.usbsts.read_volatile();
        warn!(
            "xHCI: command at {:#x} not completed within {XHCI_CMD_DEFAULT_TIMEOUT:?}, USBSTS {status:?}",
            trb_addr.raw()
        );

        // A Stop Endpoint the controller does not answer: Linux tears the host down.
        if stop_endpoint {
            error!("xHCI: host not responding to Stop Endpoint, controller dead");
            self.die(&reg, kernel, true);
            return;
        }

        let crcr = reg.read().read_crcr();
        if crcr == u64::MAX {
            error!("xHCI: CRCR reads all ones, controller dead");
            self.die(&reg, kernel, false);
            return;
        }

        if crcr as u32 & CRCR_CRR != 0 {
            if !self.abort_cmd_ring(&reg, kernel, waiter, trb_addr).await {
                return;
            }
        } else {
            debug!("xHCI: command timeout on a stopped ring");
        }

        // `xhci_handle_stopped_cmd_ring`: the aborted command becomes a no-op, and the doorbell restarts the ring
        // for any command queued behind it (another task's, e.g. a soft retry's Reset Endpoint; with none, the
        // controller just runs the no-op).
        // SAFETY: command-ring access excludes local xHCI event re-entry.
        let mut inner = unsafe { self.inner.lock_raw() };
        inner.ring.command_to_noop(trb_addr);
        wmb();
        inner
            .reg
            .write()
            .doorbell
            .write_volatile_at(0, doorbell::Register::default());
    }

    /// Linux's `xhci_abort_cmd_ring`. Returns false if the controller had to be given up.
    async fn abort_cmd_ring(
        &self,
        reg: &Arc<RwLock<XhciRegisters>>,
        kernel: &Kernel,
        waiter: &TWaiter<CommandCompletion>,
        trb_addr: BusAddr,
    ) -> bool {
        let (next, finished) = {
            // SAFETY: command-ring access excludes local xHCI event re-entry.
            let inner = unsafe { self.inner.lock_raw() };
            (
                inner.ring.next_trb_addr(trb_addr).unwrap_or(trb_addr),
                inner.ring.finished_handle(),
            )
        };
        // The pointer half is valid (the TRB after the timed-out command), in case the ring stops between the
        // two 32-bit writes; CA is in the low word.
        reg.read().write_crcr(abort_crcr_value(next));

        let crcr_reg = reg.clone();
        let stopped = handshake(
            kernel,
            CMD_POLL_STEP,
            XHCI_CMD_ABORT_TIMEOUT,
            move || crcr_reg.read().read_crcr() as u32,
            CRCR_CRR,
            0,
        )
        .await;
        if let Err(err) = stopped {
            error!(
                "xHCI: command ring abort failed ({}), controller dead",
                match err {
                    HandshakeError::Timeout => "CRR still set",
                    HandshakeError::Gone => "registers read all ones",
                }
            );
            self.die(reg, kernel, true);
            return false;
        }

        // The controller reports the stop with a Command Ring Stopped event at the TRB its dequeue pointer is on:
        // the aborted command (if it had not started it) or the one after it. A Command Aborted completion of the
        // command itself may come first.
        let saw_stop = wait_until(kernel, CMD_POLL_STEP, XHCI_CMD_STOP_EVENT_TIMEOUT, || {
            let own = waiter.try_take();
            let after = if next == trb_addr {
                None
            } else {
                finished.try_get_finished(next)
            };
            [own, after]
                .into_iter()
                .flatten()
                .any(|event| is_ring_stopped(event.completion_code()))
        })
        .await;
        if saw_stop.is_err() {
            debug!("xHCI: no Command Ring Stopped event after the abort");
        }
        true
    }

    /// Linux's `xhci_hc_died`, with `xhci_halt` first where Linux halts. `halted` is set only if HCH was seen:
    /// until then (and for good when `halt` is false, registers reading all ones) the controller's memory is
    /// quarantined instead of freed.
    fn die(&self, reg: &Arc<RwLock<XhciRegisters>>, kernel: &Kernel, halt: bool) {
        self.dead.store(true, Ordering::Release);
        if halt && halt_now(&mut reg.write(), kernel) {
            self.halted.store(true, Ordering::Release);
        }
    }
}

/// Waits for a command's completion at most `limit`; `None` once the limit is reached, or at once if the controller
/// dies meanwhile (no completion comes from a dead controller).
async fn wait_for_completion<S: Sleeper + ?Sized>(
    sleeper: &S,
    waiter: &TWaiter<CommandCompletion>,
    dead: &AtomicBool,
    limit: Duration,
) -> Option<CommandCompletion> {
    let mut completion = None;
    let _ = wait_until(sleeper, CMD_POLL_STEP, limit, || {
        completion = waiter.try_take();
        completion.is_some() || dead.load(Ordering::Acquire)
    })
    .await;
    completion
}

/// The CRCR value that aborts the ring: CA plus a valid pointer, RCS clear (it is ignored while the ring runs).
fn abort_crcr_value(next: BusAddr) -> u64 {
    (next.raw() & !0x3f) | CRCR_CA
}

fn is_ring_stopped(code: Result<CompletionCode, u8>) -> bool {
    matches!(code, Ok(CompletionCode::CommandRingStopped))
}

struct Inner {
    ring: SendRing<CommandCompletion>,
    reg: Arc<RwLock<XhciRegisters>>,
    kernel: Kernel,
}

#[cfg(test)]
mod tests {
    use core::{cell::Cell, time::Duration};

    use super::*;
    use crate::osal::tests::{FakeClock, run};

    #[test]
    fn a_command_without_completion_times_out_after_5_s_of_the_fake_clock() {
        let addr = BusAddr::from(0x2000u64);
        let finished = Finished::<CommandCompletion>::new([addr].into_iter());
        let waiter = finished.take_waiter(addr);
        let clock = FakeClock::default();

        let (completion, _) = run(
            wait_for_completion(&clock, &waiter, &AtomicBool::new(false), XHCI_CMD_DEFAULT_TIMEOUT),
            || {},
        );

        assert!(completion.is_none());
        assert!(clock.now.get() >= Duration::from_secs(5));
        assert!(clock.now.get() < Duration::from_secs(5) + CMD_POLL_STEP * 2);
    }

    #[test]
    fn a_command_completed_by_the_next_event_drain_costs_no_sleep() {
        let addr = BusAddr::from(0x2000u64);
        let finished = Finished::<CommandCompletion>::new([addr].into_iter());
        let waiter = finished.take_waiter(addr);
        let clock = FakeClock::default();
        let drained = Cell::new(false);

        let (completion, _) = run(
            wait_for_completion(&clock, &waiter, &AtomicBool::new(false), XHCI_CMD_DEFAULT_TIMEOUT),
            || {
                if !drained.replace(true) {
                    finished.set_finished(addr, CommandCompletion::default());
                }
            },
        );

        assert!(completion.is_some());
        assert_eq!(clock.now.get(), Duration::ZERO);
    }

    #[test]
    fn a_controller_that_dies_ends_the_wait_without_sleeping_out_the_timeout() {
        let addr = BusAddr::from(0x2000u64);
        let finished = Finished::<CommandCompletion>::new([addr].into_iter());
        let waiter = finished.take_waiter(addr);
        let clock = FakeClock::default();
        let dead = AtomicBool::new(false);

        let (completion, _) = run(
            wait_for_completion(&clock, &waiter, &dead, XHCI_CMD_DEFAULT_TIMEOUT),
            || dead.store(true, Ordering::Release),
        );

        assert!(completion.is_none());
        assert_eq!(clock.now.get(), Duration::ZERO);
    }

    #[test]
    fn the_abort_writes_ca_with_the_next_trb_pointer() {
        let value = abort_crcr_value(BusAddr::from(0x1234_5678_0040u64));
        assert_eq!(value & CRCR_CA, CRCR_CA);
        assert_eq!(value & 1, 0, "RCS is not written by the abort");
        assert_eq!(value & !0x3f, 0x1234_5678_0040);
    }

    #[test]
    fn only_command_ring_stopped_ends_the_wait_for_the_stop_event() {
        assert!(is_ring_stopped(Ok(CompletionCode::CommandRingStopped)));
        assert!(!is_ring_stopped(Ok(CompletionCode::CommandAborted)));
        assert!(!is_ring_stopped(Ok(CompletionCode::Success)));
        assert!(!is_ring_stopped(Err(0)));
    }
}
