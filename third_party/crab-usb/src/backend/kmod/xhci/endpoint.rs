use alloc::{
    boxed::Box,
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    vec,
    vec::Vec,
};
use core::{
    sync::atomic::{AtomicBool, Ordering},
    task::{Context, Poll, Waker},
};

use ax_sync::SpinLock as Mutex;
use dma_api::DmaDirection;
use futures::task::AtomicWaker;
use mbarrier::mb;
use usb_if::{
    descriptor::{self, EndpointDescriptor},
    endpoint::{RequestId, TransferRequest},
    err::TransferError,
    transfer::{BmRequestType, Direction},
};
use xhci::{
    registers::doorbell,
    ring::trb::{
        command,
        event::{CompletionCode, TransferEvent},
        transfer::{self, Isoch, Normal},
    },
};

use super::{
    DirectionExt, SlotId,
    cmd::CommandRing,
    context::{EP_STATE_HALTED, EP_STATE_RUNNING, EP_STATE_STOPPED, OutContext},
    reg::SlotBell,
    ring::SendRing,
    transfer::TransferId,
};
use crate::{
    BusAddr,
    backend::{
        Dci,
        ty::{
            ep::{
                EndpointOp, EndpointResetFuture, OwnedCompletion, Rejected, Submission,
                transfer_to_completion,
            },
            transfer::{Transfer, TransferKind},
        },
    },
    err::ConvertXhciError,
    osal::Kernel,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct EndpointRequestId(u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ControlStage {
    Setup,
    Data,
    Status,
}

#[derive(Clone, Copy, Debug)]
struct ControlTd {
    setup_trb: TransferId,
    data_trb: Option<TransferId>,
    status_trb: TransferId,
    requested: usize,
    actual: Option<usize>,
}

impl ControlTd {
    fn trbs(self) -> impl Iterator<Item = (TransferId, ControlStage)> {
        [
            Some((self.setup_trb, ControlStage::Setup)),
            self.data_trb.map(|trb| (trb, ControlStage::Data)),
            Some((self.status_trb, ControlStage::Status)),
        ]
        .into_iter()
        .flatten()
    }

    fn register_waker(&self, ring: &SendRing<TransferEvent>, cx: &mut core::task::Context<'_>) {
        for (trb, _) in (*self).trbs() {
            ring.register_cx(trb.0, cx);
        }
    }
}

struct SubmittedTd {
    transfer: Transfer,
    kind: SubmittedTdKind,
    trb_count: usize,
    cancelled: bool,
    waker: AtomicWaker,
}

enum SubmittedTdKind {
    Normal { completion_trb: TransferId },
    Control(ControlTd),
    Iso { packets: Vec<IsoPacketTd> },
}

#[derive(Clone, Copy)]
enum ReclaimTarget {
    Normal(TransferId),
    Control(ControlTd),
    Iso,
}

#[derive(Clone, Copy)]
struct IsoPacketTd {
    trb: TransferId,
    final_packet: bool,
    event: Option<TransferEvent>,
    actual: Option<usize>,
}

pub struct Endpoint {
    slot_id: SlotId,
    dci: Dci,
    ring: SendRing<TransferEvent>,
    bell: Arc<Mutex<SlotBell>>,
    cmd: CommandRing,
    halted: Arc<AtomicBool>,
    reset_in_progress: Arc<AtomicBool>,
    next_request_id: u64,
    inflight: BTreeMap<EndpointRequestId, SubmittedTd>,
    stopped_completions: BTreeSet<EndpointRequestId>,
    trb_to_request: BTreeMap<TransferId, EndpointRequestId>,
    outstanding_trbs: usize,
    kernel: Kernel,
    max_packet_size: usize,
    max_burst_size: usize,
    interval: u8,
    iso_start_asap: bool,
    next_iso_frame_id: u16,
    /// Bulk or interrupt endpoint of a device not behind a TT: Linux soft-retries its transaction errors.
    soft_retry: bool,
    /// Transaction errors since the last success (Linux `ep->err_count`).
    err_count: u32,
    /// A soft reset in flight: the request it retries, the event that caused it, and Reset Endpoint (TSP) plus
    /// the doorbell.
    soft_reset: Option<SoftReset>,
    /// The controller's output device context of the slot: where the controller leaves the endpoint state and the
    /// TR Dequeue Pointer it stopped on (Linux `xhci_get_hw_deq`). Without it, the pointer of a Stopped event is
    /// used.
    out_ctx: Option<OutContext>,
    /// TDs cancelled and not yet taken off the ring, in the order they were cancelled (Linux `cancelled_td_list`).
    cancelled_tds: Vec<EndpointRequestId>,
    /// The Stop Endpoint, invalidate and Set TR Dequeue Pointer pass that takes `cancelled_tds` off the ring.
    cancel_op: Option<CancelOp>,
    /// TRBs of cancelled TDs turned into No-Ops that the controller may not have stepped over yet, by the id of the
    /// request they belonged to: they still take room in the ring. Ours; Linux counts them in the ring's free TRBs
    /// until its dequeue passes them.
    stale_noops: BTreeMap<EndpointRequestId, usize>,
}

struct SoftReset {
    request_id: EndpointRequestId,
    completion_trb: TransferId,
    event: TransferEvent,
    future: EndpointResetFuture,
}

/// One pass over the cancelled TDs, as Linux `xhci_urb_dequeue` queues it and `xhci_handle_cmd_stop_ep`,
/// `xhci_invalidate_cancelled_tds` and `xhci_handle_cmd_set_deq` carry it out.
struct CancelOp {
    /// The cancelled TDs this pass takes off the ring; a TD cancelled meanwhile joins the next pass.
    batch: Vec<EndpointRequestId>,
    phase: CancelPhase,
    /// Stop Endpoint commands sent in this pass.
    stop_attempts: u8,
    /// The endpoint was halted and has been reset in this pass.
    reset_halted: bool,
    /// Requests with an id from here on were submitted while the pass runs: they are queued behind the stop and
    /// are no part of it (never cancelled, never stepped over).
    first_new_id: u64,
}

enum CancelPhase {
    /// Stop Endpoint sent.
    Stopping(EndpointResetFuture),
    /// The Stop Endpoint found the endpoint halted: Reset Endpoint sent first (`xhci_handle_cmd_stop_ep`).
    ResettingHalted(EndpointResetFuture),
    /// Set TR Dequeue Pointer sent, to step the dequeue pointer past `past`, the TD the controller stopped on.
    SettingDequeue {
        future: EndpointResetFuture,
        past: EndpointRequestId,
    },
}

/// What an endpoint context state says about a Stop Endpoint that failed (`xhci_handle_cmd_stop_ep`). No context
/// (`None`) is taken as stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StopState {
    Stopped,
    Halted,
    Running,
    Other,
}

fn stop_state(state: Option<u8>) -> StopState {
    match state {
        None | Some(EP_STATE_STOPPED) => StopState::Stopped,
        Some(EP_STATE_HALTED) => StopState::Halted,
        Some(EP_STATE_RUNNING) => StopState::Running,
        Some(_) => StopState::Other,
    }
}

async fn stop_endpoint_command(
    command_ring: &mut CommandRing,
    slot_id: u8,
    endpoint_id: u8,
) -> Result<(), TransferError> {
    let stop_endpoint = *command::StopEndpoint::default()
        .set_endpoint_id(endpoint_id)
        .set_slot_id(slot_id);
    command_ring
        .cmd_request(command::Allowed::StopEndpoint(stop_endpoint))
        .await?;
    Ok(())
}

async fn reset_endpoint_command(
    command_ring: &mut CommandRing,
    slot_id: u8,
    endpoint_id: u8,
) -> Result<(), TransferError> {
    let reset_endpoint = *command::ResetEndpoint::default()
        .set_endpoint_id(endpoint_id)
        .set_slot_id(slot_id);
    command_ring
        .cmd_request(command::Allowed::ResetEndpoint(reset_endpoint))
        .await?;
    Ok(())
}

/// Ours: Linux requeues a Stop Endpoint that raced with the endpoint starting, without a bound; a controller that
/// keeps answering with a context error while the endpoint runs would never end the pass.
const MAX_STOP_ATTEMPTS: u8 = 3;

/// Ours, from crab-usb: 16 pages per endpoint ring; Linux starts endpoint rings small and grows them.
const ENDPOINT_RING_PAGES: usize = 16;

/// Linux `MAX_SOFT_RETRY` (xhci.h:1271): a transaction error is soft-retried while `err_count`, before it is
/// counted up, is at most this.
const MAX_SOFT_RETRY: u32 = 3;

impl Endpoint {
    pub fn new(
        slot_id: SlotId,
        dci: Dci,
        kernel: &Kernel,
        bell: Arc<Mutex<SlotBell>>,
        cmd: CommandRing,
    ) -> crate::err::Result<Self> {
        let ring =
            SendRing::new_with_pages(ENDPOINT_RING_PAGES, DmaDirection::Bidirectional, kernel)?;

        Ok(Self {
            slot_id,
            dci,
            ring,
            bell,
            cmd,
            halted: Arc::new(AtomicBool::new(false)),
            reset_in_progress: Arc::new(AtomicBool::new(false)),
            next_request_id: 1,
            inflight: BTreeMap::new(),
            stopped_completions: BTreeSet::new(),
            trb_to_request: BTreeMap::new(),
            outstanding_trbs: 0,
            kernel: kernel.clone(),
            max_packet_size: 0,
            max_burst_size: 0,
            interval: 1,
            iso_start_asap: true,
            next_iso_frame_id: 0,
            soft_retry: false,
            err_count: 0,
            soft_reset: None,
            out_ctx: None,
            cancelled_tds: Vec::new(),
            cancel_op: None,
            stale_noops: BTreeMap::new(),
        })
    }

    #[cfg(test)]
    fn quarantine_len(&self) -> usize {
        self.cmd.quarantine().len()
    }

    pub(crate) fn set_out_context(&mut self, out_ctx: OutContext) {
        self.out_ctx = Some(out_ctx);
    }

    pub fn set_soft_retry(&mut self, soft_retry: bool) {
        self.soft_retry = soft_retry;
    }

    /// Linux's soft retry of a transaction error (`process_bulk_intr_td`): on a bulk or interrupt endpoint not
    /// behind a TT, with fewer than `MAX_SOFT_RETRY` + 1 errors in a row, the TD is not given back: a Reset
    /// Endpoint with Transfer State Preserve keeps the dequeue pointer on it, and the doorbell starts it again
    /// (`xhci_handle_cmd_reset_ep`). Returns whether the retry was started.
    fn start_soft_retry(
        &mut self,
        request_id: EndpointRequestId,
        completion_trb: TransferId,
        event: TransferEvent,
    ) -> bool {
        let cancelled = self
            .inflight
            .get(&request_id)
            .is_none_or(|submitted| submitted.cancelled);
        if !soft_retry_allowed(
            self.soft_retry,
            event.completion_code(),
            &mut self.err_count,
        ) || cancelled
            || self.soft_reset.is_some()
        {
            return false;
        }
        debug!(
            "xhci: transaction error dci={}, soft retry {}",
            self.dci.raw(),
            self.err_count
        );
        let slot_id = self.slot_id.as_u8();
        let endpoint_id = self.dci.as_u8();
        let mut cmd = self.cmd.clone();
        let bell = self.bell.clone();
        let future: EndpointResetFuture = Box::pin(async move {
            let mut reset_endpoint = command::ResetEndpoint::default();
            reset_endpoint
                .set_endpoint_id(endpoint_id)
                .set_slot_id(slot_id)
                .set_transfer_state_preserve();
            cmd.cmd_request(command::Allowed::ResetEndpoint(reset_endpoint))
                .await?;
            let mut doorbell = doorbell::Register::default();
            doorbell.set_doorbell_target(endpoint_id);
            // SAFETY: endpoint doorbell submission is serialized by the xHCI path.
            unsafe { bell.lock_raw() }.ring(doorbell);
            Ok(())
        });
        self.soft_reset = Some(SoftReset {
            request_id,
            completion_trb,
            event,
            future,
        });
        true
    }

    /// Drives a soft reset in flight. `Some` is what `reclaim_request` returns for `request_id` now: `None` while
    /// it runs, the original error if it failed. `None` here means no soft reset holds `request_id` back.
    fn poll_soft_reset(
        &mut self,
        id: RequestId,
        request_id: EndpointRequestId,
    ) -> Option<Option<Result<OwnedCompletion, TransferError>>> {
        let soft_reset = self.soft_reset.as_mut()?;
        let mut cx = Context::from_waker(Waker::noop());
        let result = match soft_reset.future.as_mut().poll(&mut cx) {
            Poll::Pending => {
                return (soft_reset.request_id == request_id).then_some(None);
            }
            Poll::Ready(result) => result,
        };
        let soft_reset = self.soft_reset.take()?;
        match result {
            Ok(()) => None,
            Err(err) => {
                warn!(
                    "xhci: soft retry of dci={} failed: {err}; the endpoint stays halted",
                    self.dci.raw()
                );
                if soft_reset.request_id != request_id {
                    self.halted.store(true, Ordering::Release);
                    return None;
                }
                Some(Some(
                    self.complete_request(request_id, soft_reset.completion_trb, soft_reset.event)
                        .map(|transfer| transfer_to_completion(id, transfer)),
                ))
            }
        }
    }

    /// A pass over the cancelled TDs is queued or running.
    fn cancel_in_progress(&self) -> bool {
        self.cancel_op.is_some() || !self.cancelled_tds.is_empty()
    }

    /// The first and the last TRB of a TD (Linux `td->first_trb`, `td->last_trb`).
    fn td_span(submitted: &SubmittedTd) -> Option<(BusAddr, BusAddr)> {
        match &submitted.kind {
            SubmittedTdKind::Normal { completion_trb } => Some((completion_trb.0, completion_trb.0)),
            SubmittedTdKind::Control(control_td) => {
                Some((control_td.setup_trb.0, control_td.status_trb.0))
            }
            SubmittedTdKind::Iso { packets } => {
                Some((packets.first()?.trb.0, packets.last()?.trb.0))
            }
        }
    }

    /// Whether the TD's own completion is in already, so that a cancel would only race with it: Linux gives back an
    /// URB the controller has finished with its result (`usb_hcd_unlink_urb_from_ep` fails for it), and
    /// `usb_start_wait_urb` returns the completion of a transfer that finished as the timeout fired. ISO TDs
    /// report per packet and are cancelled whole.
    fn has_final_event(&self, submitted: &SubmittedTd) -> bool {
        let event_of = |trb: TransferId| {
            self.ring
                .peek_finished(trb.0)
                .filter(|event| !event_is_stop(event))
        };
        match &submitted.kind {
            SubmittedTdKind::Normal { completion_trb } => event_of(*completion_trb).is_some(),
            SubmittedTdKind::Control(control_td) => control_td.trbs().any(|(trb, stage)| {
                event_of(trb).is_some_and(|event| {
                    stage == ControlStage::Status
                        || !matches!(event.completion_code(), Ok(code) if code.to_result().is_ok())
                })
            }),
            SubmittedTdKind::Iso { .. } => false,
        }
    }

    /// Where the controller stopped: the TR Dequeue Pointer and Dequeue Cycle State of the endpoint context in the
    /// output device context (Linux `xhci_get_hw_deq`), else the TRB of a Stopped event, whose cycle bit is the
    /// cycle state the controller is at.
    fn hw_dequeue(&self) -> Option<(BusAddr, bool)> {
        if let Some(out_ctx) = &self.out_ctx {
            let (pointer, cycle) = out_ctx.endpoint_dequeue(self.dci.as_u8());
            return Some((BusAddr::from(pointer), cycle));
        }
        self.trb_to_request.keys().find_map(|trb| {
            let event = self.ring.peek_finished(trb.0)?;
            if !event_is_stop(&event) {
                return None;
            }
            Some((trb.0, self.ring.read_trb_word(trb.0, 3)? & 1 != 0))
        })
    }

    /// Drives the pass over the cancelled TDs: starts one when TDs wait and nothing else holds the endpoint (a soft
    /// reset), and moves a running one along as far as its commands have completed.
    fn poll_cancel_op(&mut self) {
        if self.cmd.is_dead() {
            // Every request is given back as `NoDevice` by `reclaim_request` (`xhci_hc_died`).
            self.cancel_op = None;
            self.cancelled_tds.clear();
            return;
        }
        let mut op = match self.cancel_op.take() {
            Some(op) => op,
            None => {
                if self.cancelled_tds.is_empty() || self.soft_reset.is_some() {
                    return;
                }
                CancelOp {
                    batch: core::mem::take(&mut self.cancelled_tds),
                    phase: CancelPhase::Stopping(self.stop_command_future()),
                    stop_attempts: 1,
                    reset_halted: false,
                    first_new_id: self.next_request_id,
                }
            }
        };
        let mut cx = Context::from_waker(Waker::noop());
        loop {
            let polled = match &mut op.phase {
                CancelPhase::Stopping(future)
                | CancelPhase::ResettingHalted(future)
                | CancelPhase::SettingDequeue { future, .. } => future.as_mut().poll(&mut cx),
            };
            let Poll::Ready(result) = polled else {
                self.cancel_op = Some(op);
                return;
            };
            match self.cancel_step(op, result) {
                Some(next) => op = next,
                None => return,
            }
        }
    }

    /// The command of the pass in `op.phase` completed with `result`. Returns the pass with its next command
    /// queued, or `None` if the pass is over.
    fn cancel_step(&mut self, op: CancelOp, result: Result<(), TransferError>) -> Option<CancelOp> {
        let CancelOp {
            batch,
            phase,
            stop_attempts,
            reset_halted,
            first_new_id,
        } = op;
        match phase {
            CancelPhase::Stopping(_) => match result {
                Ok(()) => self.invalidate_cancelled_tds(batch, stop_attempts, reset_halted, first_new_id),
                Err(TransferError::Timeout | TransferError::NoDevice) => {
                    self.give_up_cancelled_tds(batch, "the Stop Endpoint command did not complete");
                    None
                }
                Err(error) => {
                    // `xhci_handle_cmd_stop_ep`, COMP_CONTEXT_STATE_ERROR: the endpoint state says what raced.
                    let state = self
                        .out_ctx
                        .as_ref()
                        .map(|out_ctx| out_ctx.endpoint_state(self.dci.as_u8()));
                    match stop_state(state) {
                        // The endpoint halted before the stop: reset it first, the stop's effect is the same.
                        StopState::Halted => Some(CancelOp {
                            batch,
                            phase: CancelPhase::ResettingHalted(self.reset_halted_future()),
                            stop_attempts,
                            reset_halted,
                            first_new_id,
                        }),
                        // The stop was handled before the endpoint was running: stop it again.
                        StopState::Running if stop_attempts < MAX_STOP_ATTEMPTS => {
                            Some(CancelOp {
                                batch,
                                phase: CancelPhase::Stopping(self.stop_command_future()),
                                stop_attempts: stop_attempts + 1,
                                reset_halted,
                                first_new_id,
                            })
                        }
                        // Already stopped, by an earlier pass or the controller: go on.
                        StopState::Stopped => {
                            self.invalidate_cancelled_tds(batch, stop_attempts, reset_halted, first_new_id)
                        }
                        StopState::Running | StopState::Other => {
                            warn!(
                                "xhci: Stop Endpoint dci={} failed: {error}, endpoint state {state:?}",
                                self.dci.raw()
                            );
                            self.give_up_cancelled_tds(batch, "the endpoint could not be stopped");
                            None
                        }
                    }
                }
            },
            CancelPhase::ResettingHalted(_) => {
                // Linux ignores the completion code of this Reset Endpoint (`xhci_handle_cmd_reset_ep`).
                if result.is_ok() {
                    self.halted.store(false, Ordering::Release);
                }
                self.invalidate_cancelled_tds(batch, stop_attempts, true, first_new_id)
            }
            CancelPhase::SettingDequeue { past, .. } => {
                // `xhci_handle_cmd_set_deq`: a failed command is only reported, and the TDs are given back; the
                // TD the controller stopped on may still be cached by it, so its buffer is kept.
                let kept = match result {
                    Ok(()) => None,
                    Err(error) => {
                        warn!(
                            "xhci: Set TR Dequeue Pointer dci={} failed: {error}",
                            self.dci.raw()
                        );
                        Some(past)
                    }
                };
                self.give_back_cancelled_tds(batch, kept);
                None
            }
        }
    }

    /// Linux `xhci_invalidate_cancelled_tds`, with the endpoint stopped: the TD the controller stopped on is stepped
    /// over by a Set TR Dequeue Pointer, every other cancelled TD is turned into No-Ops (`td_to_noop`) for the
    /// controller to skip. Returns the pass with the Set TR Dequeue Pointer queued, or `None` if the TDs are already
    /// given back.
    fn invalidate_cancelled_tds(
        &mut self,
        batch: Vec<EndpointRequestId>,
        stop_attempts: u8,
        reset_halted: bool,
        first_new_id: u64,
    ) -> Option<CancelOp> {
        let hw = self.hw_dequeue();
        let mut cached = None;
        for &request_id in &batch {
            let Some(submitted) = self.inflight.get(&request_id) else {
                continue;
            };
            let Some((first, last)) = Self::td_span(submitted) else {
                continue;
            };
            let holds_hw = hw.is_some_and(|(addr, _)| self.ring.td_contains(first, last, addr));
            let trb_count = submitted.trb_count;
            self.ring.td_to_noop(first, last);
            if holds_hw && cached.is_none() {
                cached = Some(request_id);
            } else {
                self.stale_noops.insert(request_id, trb_count);
            }
        }
        // A TD that stalled the endpoint is not given back by this pass, but the controller must not run it again
        // after the reset: its dequeue pointer is stepped over it too (Linux `find_halted_td`).
        if cached.is_none()
            && reset_halted
            && let Some((hw_addr, _)) = hw
        {
            cached = self.inflight.iter().find_map(|(&request_id, submitted)| {
                if request_id.0 >= first_new_id {
                    return None;
                }
                let (first, last) = Self::td_span(submitted)?;
                self.ring
                    .td_contains(first, last, hw_addr)
                    .then_some(request_id)
            });
        }
        let Some(past) = cached else {
            self.give_back_cancelled_tds(batch, None);
            return None;
        };
        let moved = hw.and_then(|(hw_addr, hw_cycle)| {
            let (_, last) = Self::td_span(self.inflight.get(&past)?)?;
            self.ring.past_td(hw_addr, hw_cycle, last)
        });
        let Some((pointer, cycle)) = moved else {
            // Linux marks the TDs cleared anyway ("Failed to move past cached td").
            warn!(
                "xhci: no dequeue pointer past the TD the endpoint dci={} stopped on",
                self.dci.raw()
            );
            self.give_back_cancelled_tds(batch, Some(past));
            return None;
        };
        Some(CancelOp {
            batch,
            phase: CancelPhase::SettingDequeue {
                future: self.set_dequeue_future(pointer, cycle, false),
                past,
            },
            stop_attempts,
            reset_halted,
            first_new_id,
        })
    }

    /// Linux `xhci_giveback_invalidated_tds`, at the end of `xhci_handle_cmd_set_deq`: each cancelled TD is dropped,
    /// freeing its buffer and mapping (`kept` is quarantined instead, or any TD if the controller died),
    /// `reclaim_request` reports `Cancelled` for it, and the endpoint's remaining TDs are started again with the
    /// doorbell (`ring_doorbell_for_active_rings`).
    fn give_back_cancelled_tds(&mut self, batch: Vec<EndpointRequestId>, kept: Option<EndpointRequestId>) {
        for request_id in batch {
            let Some(submitted) = self.remove_request(request_id) else {
                continue;
            };
            submitted.waker.wake();
            if kept == Some(request_id) {
                self.cmd.quarantine().keep(submitted);
            } else {
                self.release_td(submitted);
            }
            self.stopped_completions.insert(request_id);
        }
        // The Stopped events of the TDs that go on would hide their real completions (a slot holds one event).
        let live_trbs = self.trb_to_request.keys().copied().collect::<Vec<_>>();
        for trb in live_trbs {
            self.ring.take_finished_if(trb.0, event_is_stop);
        }
        if !self.inflight.is_empty() && self.cancelled_tds.is_empty() {
            mb();
            self.doorbell();
        }
    }

    /// The endpoint could not be stopped, so the controller may still run the cancelled TDs: they are given up with
    /// their buffers kept in the quarantine, and their requests end as cancelled.
    fn give_up_cancelled_tds(&mut self, batch: Vec<EndpointRequestId>, why: &str) {
        warn!(
            "xhci: cancel on dci={}: {why}; {} TDs quarantined",
            self.dci.raw(),
            batch.len()
        );
        for request_id in batch {
            let Some(submitted) = self.remove_request(request_id) else {
                continue;
            };
            submitted.waker.wake();
            self.cmd.quarantine().keep(submitted);
            self.stopped_completions.insert(request_id);
        }
    }

    /// Reset Endpoint without Transfer State Preserve, for an endpoint a Stop Endpoint found halted.
    fn reset_halted_future(&self) -> EndpointResetFuture {
        let (slot_id, endpoint_id) = (self.slot_id.as_u8(), self.dci.as_u8());
        let mut cmd = self.cmd.clone();
        Box::pin(async move { reset_endpoint_command(&mut cmd, slot_id, endpoint_id).await })
    }

    /// Set TR Dequeue Pointer to `dequeue_pointer` with Dequeue Cycle State `dequeue_cycle_state`; then the
    /// doorbell, if `ring_bell`.
    fn set_dequeue_future(
        &self,
        dequeue_pointer: BusAddr,
        dequeue_cycle_state: bool,
        ring_bell: bool,
    ) -> EndpointResetFuture {
        let slot_id = self.slot_id.as_u8();
        let endpoint_id = self.dci.as_u8();
        let mut command_ring = self.cmd.clone();
        let bell = self.bell.clone();
        Box::pin(async move {
            let mut set_dequeue_pointer = command::SetTrDequeuePointer::default();
            set_dequeue_pointer
                .set_new_tr_dequeue_pointer(dequeue_pointer.raw())
                .set_endpoint_id(endpoint_id)
                .set_slot_id(slot_id);
            if dequeue_cycle_state {
                set_dequeue_pointer.set_dequeue_cycle_state();
            } else {
                set_dequeue_pointer.clear_dequeue_cycle_state();
            }
            command_ring
                .cmd_request(command::Allowed::SetTrDequeuePointer(set_dequeue_pointer))
                .await?;

            if ring_bell {
                let mut doorbell = doorbell::Register::default();
                doorbell.set_doorbell_target(endpoint_id);
                // SAFETY: Configure rollback keeps the endpoint unpublished while
                // the task path restores its dequeue pointer and rings the bell.
                unsafe { bell.lock_raw() }.ring(doorbell);
            }
            Ok(())
        })
    }

    pub fn configure_periodic(
        &mut self,
        max_packet_size: usize,
        max_burst_size: usize,
        interval: u8,
    ) {
        self.max_packet_size = max_packet_size;
        self.max_burst_size = max_burst_size;
        self.interval = interval.max(1);
        self.iso_start_asap = true;
    }

    pub fn bus_addr(&self) -> BusAddr {
        self.ring.bus_addr()
    }

    fn doorbell(&mut self) {
        let mut bell = doorbell::Register::default();
        bell.set_doorbell_target(self.dci.into());
        // SAFETY: endpoint doorbell submission is serialized by the xHCI path.
        unsafe { self.bell.lock_raw() }.ring(bell);
    }

    pub fn ring(&self) -> &SendRing<TransferEvent> {
        &self.ring
    }

    fn allocate_request_id(&mut self) -> EndpointRequestId {
        loop {
            let id = EndpointRequestId(self.next_request_id);
            self.next_request_id = self.next_request_id.wrapping_add(1).max(1);
            if !self.inflight.contains_key(&id) {
                return id;
            }
        }
    }

    fn public_request_id(id: EndpointRequestId) -> RequestId {
        RequestId::new(id.0)
    }

    fn private_request_id(id: RequestId) -> EndpointRequestId {
        EndpointRequestId(id.raw())
    }

    fn validate_completion_code(
        &self,
        event: TransferEvent,
        transfer: &Transfer,
    ) -> Result<(), TransferError> {
        let kind_name = match &transfer.kind {
            TransferKind::Control(_) => "control",
            TransferKind::Bulk => "bulk",
            TransferKind::Interrupt => "interrupt",
            TransferKind::Isochronous { .. } => "iso",
        };
        match event.completion_code() {
            Ok(code) => {
                if let Err(e) = code.to_result() {
                    warn!(
                        "xhci: transfer error dci={} kind={} code={:?} remaining={}",
                        self.dci.raw(),
                        kind_name,
                        code,
                        event.trb_transfer_length()
                    );
                    return Err(e);
                }
            }
            Err(e) => {
                warn!(
                    "xhci: transfer error dci={} kind={} unknown_code={} remaining={}",
                    self.dci.raw(),
                    kind_name,
                    e,
                    event.trb_transfer_length()
                );
                return Err(TransferError::Other(anyhow!("Transfer failed")));
            }
        }
        Ok(())
    }

    fn complete_request(
        &mut self,
        request_id: EndpointRequestId,
        event_trb: TransferId,
        event: TransferEvent,
    ) -> Result<Transfer, TransferError> {
        let submitted = self.remove_request(request_id).ok_or_else(|| {
            warn!(
                "xhci: completion for missing request dci={} request_id={} event_trb={:#x}",
                self.dci.raw(),
                request_id.0,
                event_trb.0.raw()
            );
            TransferError::InvalidEndpoint
        })?;

        if submitted.cancelled {
            return Err(TransferError::Cancelled);
        }
        // The controller works through the ring in order: a TD that completed is behind every No-Op of an earlier
        // cancelled one.
        self.stale_noops.retain(|&cancelled, _| cancelled > request_id);

        if matches!(event.completion_code(), Ok(CompletionCode::Success)) {
            self.err_count = 0;
        }
        // Linux `xhci_halted_host_endpoint` (xhci-ring.c:2191-2215): Stall halts every endpoint; Babble, USB
        // Transaction Error and Split Transaction Error halt every non-isochronous one. The driver's reset()
        // then sends Reset Endpoint and Set TR Dequeue Pointer.
        if !matches!(submitted.kind, SubmittedTdKind::Iso { .. })
            && completion_halts_endpoint(&event)
        {
            self.halted.store(true, Ordering::Release);
        }
        if !matches!(submitted.kind, SubmittedTdKind::Iso { .. }) {
            self.validate_completion_code(event, &submitted.transfer)?;
        }
        self.transfer_from_completion(submitted, event_trb, event)
    }

    /// Frees a TD taken off this endpoint, unless the controller died without being seen to halt: it may then still
    /// be working through the ring, reading or writing the TD's buffer (bounce buffer or streaming mapping). Linux
    /// frees it (`xhci_hc_died` gives the URB back), its IOMMU faulting a late DMA; ours is in bypass, so the TD is
    /// quarantined for good instead. The caller still gets its error.
    fn release_td(&self, submitted: SubmittedTd) {
        if self.cmd.may_still_dma() {
            let quarantine = self.cmd.quarantine();
            quarantine.keep(submitted);
            warn!(
                "xHCI slot {} ep {}: controller dead but not halted, TD quarantined ({} kept)",
                self.slot_id.as_u8(),
                self.dci.as_u8(),
                quarantine.len()
            );
        } else {
            drop(submitted);
        }
    }

    fn remove_request(&mut self, request_id: EndpointRequestId) -> Option<SubmittedTd> {
        let submitted = self.inflight.remove(&request_id)?;
        self.cancelled_tds.retain(|&cancelled| cancelled != request_id);
        self.outstanding_trbs = self.outstanding_trbs.saturating_sub(submitted.trb_count);
        match &submitted.kind {
            SubmittedTdKind::Normal { completion_trb } => {
                self.trb_to_request.remove(completion_trb);
            }
            SubmittedTdKind::Control(control_td) => {
                for (trb, _) in (*control_td).trbs() {
                    self.trb_to_request.remove(&trb);
                }
            }
            SubmittedTdKind::Iso { packets } => {
                for packet in packets {
                    self.trb_to_request.remove(&packet.trb);
                }
            }
        }
        Some(submitted)
    }

    fn transfer_from_completion(
        &mut self,
        submitted: SubmittedTd,
        event_trb: TransferId,
        event: TransferEvent,
    ) -> Result<Transfer, TransferError> {
        let mut transfer = submitted.transfer;
        if let SubmittedTdKind::Iso { packets } = &submitted.kind {
            let TransferKind::Isochronous { packet_lengths } = &transfer.kind else {
                return Err(TransferError::Other(anyhow!("non-ISO transfer has ISO TD")));
            };
            if packets.len() != packet_lengths.len() {
                return Err(TransferError::Other(anyhow!(
                    "ISO completion count mismatch: ids={}, packets={}",
                    packets.len(),
                    packet_lengths.len()
                )));
            }

            let mut actual_lengths = Vec::with_capacity(packets.len());
            for (index, packet) in packets.iter().copied().enumerate() {
                let requested = packet_lengths[index];
                let actual = match packet.actual {
                    Some(actual) => actual,
                    None if packet.trb == event_trb => iso_packet_actual_length(requested, event)?,
                    None => 0,
                };
                actual_lengths.push(actual);
            }

            let transfer_len = actual_lengths.iter().sum();
            transfer.iso_packet_actual_lengths = actual_lengths;
            if transfer_len > 0 && matches!(transfer.direction, Direction::In) {
                transfer.complete_for_cpu();
            }
            transfer.transfer_len = transfer_len;
            trace!("ISO transfer data length: {}", transfer.transfer_len);
            return Ok(transfer);
        }

        let transfer_len = match submitted.kind {
            SubmittedTdKind::Control(control_td) => {
                control_td.actual.unwrap_or(control_td.requested)
            }
            SubmittedTdKind::Normal { .. } => {
                let remaining = event.trb_transfer_length() as usize;
                transfer.buffer_len().saturating_sub(remaining)
            }
            SubmittedTdKind::Iso { .. } => unreachable!("ISO was handled above"),
        };

        if transfer_len > 0 && matches!(transfer.direction, Direction::In) {
            transfer.complete_for_cpu();
        }
        transfer.transfer_len = transfer_len;
        trace!("Transfer data length: {}", transfer.transfer_len);
        Ok(transfer)
    }

    fn reclaim_control_request(
        &mut self,
        id: RequestId,
        request_id: EndpointRequestId,
        control_td: ControlTd,
    ) -> Option<Result<OwnedCompletion, TransferError>> {
        for (event_trb, stage) in control_td.trbs() {
            let Some(event) = self.ring.get_finished(event_trb.0) else {
                continue;
            };
            if event_is_stop(&event) {
                continue;
            }
            let remaining = event.trb_transfer_length() as usize;
            if let Some(submitted) = self.inflight.get_mut(&request_id)
                && let SubmittedTdKind::Control(control_td) = &mut submitted.kind
            {
                match stage {
                    ControlStage::Setup => {}
                    ControlStage::Data => {
                        control_td.actual =
                            Some(submitted.transfer.buffer_len().saturating_sub(remaining));
                    }
                    ControlStage::Status if control_td.actual.is_none() => {
                        control_td.actual = Some(control_td.requested);
                    }
                    ControlStage::Status => {}
                }
            }
            match event.completion_code() {
                Ok(code) if code.to_result().is_ok() => {
                    if matches!(stage, ControlStage::Status) {
                        return Some(
                            self.complete_request(request_id, event_trb, event)
                                .map(|transfer| transfer_to_completion(id, transfer)),
                        );
                    }
                }
                _ => {
                    return Some(
                        self.complete_request(request_id, event_trb, event)
                            .map(|transfer| transfer_to_completion(id, transfer)),
                    );
                }
            }
        }
        None
    }

    fn reclaim_iso_request(
        &mut self,
        id: RequestId,
        request_id: EndpointRequestId,
    ) -> Option<Result<OwnedCompletion, TransferError>> {
        let packet_count = self.iso_packet_count(request_id)?;
        for index in 0..packet_count {
            if self.iso_packet_done(request_id, index) {
                continue;
            }
            let (packet_trb, requested) = self.iso_packet_info(request_id, index)?;

            let Some(event) = self.ring.get_finished(packet_trb.0) else {
                continue;
            };
            if event_is_stop(&event) {
                continue;
            }
            let actual = match iso_packet_actual_length(requested, event) {
                Ok(actual) => actual,
                Err(err) => {
                    let cleanup_result = self.complete_request(request_id, packet_trb, event);
                    let result = match cleanup_result {
                        Ok(_) => Err(err),
                        Err(cleanup_err) => Err(cleanup_err),
                    };
                    return Some(result.map(|transfer| transfer_to_completion(id, transfer)));
                }
            };

            let fatal = iso_packet_is_fatal(event);
            let should_complete =
                self.record_iso_packet(request_id, index, event, actual, fatal)?;
            if should_complete {
                return Some(
                    self.complete_request(request_id, packet_trb, event)
                        .map(|transfer| transfer_to_completion(id, transfer)),
                );
            }
        }
        None
    }

    fn iso_packet_count(&self, request_id: EndpointRequestId) -> Option<usize> {
        self.inflight
            .get(&request_id)
            .and_then(|submitted| match &submitted.kind {
                SubmittedTdKind::Iso { packets } => Some(packets.len()),
                _ => None,
            })
    }

    fn iso_packet_done(&self, request_id: EndpointRequestId, index: usize) -> bool {
        self.inflight
            .get(&request_id)
            .and_then(|submitted| match &submitted.kind {
                SubmittedTdKind::Iso { packets } => {
                    packets.get(index).map(|packet| packet.actual.is_some())
                }
                _ => None,
            })
            .unwrap_or(true)
    }

    fn iso_packet_info(
        &self,
        request_id: EndpointRequestId,
        index: usize,
    ) -> Option<(TransferId, usize)> {
        let submitted = self.inflight.get(&request_id)?;
        let SubmittedTdKind::Iso { packets } = &submitted.kind else {
            return None;
        };
        let packet = packets.get(index)?;
        let requested = match &submitted.transfer.kind {
            TransferKind::Isochronous { packet_lengths } => {
                packet_lengths.get(index).copied().unwrap_or(0)
            }
            _ => return None,
        };
        Some((packet.trb, requested))
    }

    fn record_iso_packet(
        &mut self,
        request_id: EndpointRequestId,
        index: usize,
        event: TransferEvent,
        actual: usize,
        fatal: bool,
    ) -> Option<bool> {
        let submitted = self.inflight.get_mut(&request_id)?;
        let SubmittedTdKind::Iso { packets } = &mut submitted.kind else {
            return None;
        };
        let final_packet = packets.get(index).is_some_and(|packet| packet.final_packet);
        if final_packet || fatal {
            for packet in packets.iter_mut().take(index) {
                if packet.actual.is_none() {
                    packet.actual = Some(0);
                }
            }
        }
        if let Some(packet) = packets.get_mut(index) {
            packet.event = Some(event);
            packet.actual = Some(actual);
        }
        Some(final_packet || fatal || packets.iter().all(|packet| packet.actual.is_some()))
    }

    fn enque_trb(&mut self, trb: transfer::Allowed) -> TransferId {
        TransferId(self.ring.enque_transfer(trb))
    }

    fn enque_iso(
        &mut self,
        bus_addr: u64,
        packet_lengths: &[usize],
        interrupt_on_short_packet: bool,
    ) -> Vec<IsoPacketTd> {
        let mut packets = Vec::with_capacity(packet_lengths.len().max(1));
        let mut offset = 0u64;
        let packet_count = packet_lengths.len().max(1);
        let interval = self.interval.max(1);
        let mut frame_id = self.next_iso_frame_id;

        for index in 0..packet_count {
            let packet_length = packet_lengths.get(index).copied().unwrap_or(0);
            let last_packet = index + 1 == packet_count;
            let trb = self.enque_iso_trb(
                bus_addr + offset,
                packet_length,
                last_packet,
                !last_packet,
                frame_id,
                interrupt_on_short_packet,
            );
            packets.push(IsoPacketTd {
                trb,
                final_packet: last_packet,
                event: None,
                actual: None,
            });
            offset += packet_length as u64;
            frame_id = frame_id.wrapping_add(interval as u16) & 0x7ff;
        }

        self.next_iso_frame_id = frame_id;
        packets
    }

    fn enque_iso_trb(
        &mut self,
        bus_addr: u64,
        buff_len: usize,
        last_packet: bool,
        block_event_interrupt: bool,
        frame_id: u16,
        interrupt_on_short_packet: bool,
    ) -> TransferId {
        let mut trb = Isoch::new();
        trb.set_data_buffer_pointer(bus_addr as _)
            .set_trb_transfer_length(buff_len as _)
            .set_interrupter_target(0);

        if self.use_iso_sia() {
            trb.set_start_isoch_asap();
        } else {
            trb.set_frame_id(frame_id & 0x7ff);
        }
        if interrupt_on_short_packet {
            trb.set_interrupt_on_short_packet();
        }
        let total_packets = if self.max_packet_size == 0 {
            1
        } else {
            buff_len.div_ceil(self.max_packet_size).max(1)
        };
        let packets_per_burst = self.max_burst_size.saturating_add(1).max(1);
        let burst_count = total_packets.div_ceil(packets_per_burst).saturating_sub(1);
        let last_burst_packet_count = match total_packets % packets_per_burst {
            0 => packets_per_burst.saturating_sub(1),
            residue => residue.saturating_sub(1),
        };
        trb.set_td_size_or_tbc(burst_count.min(0x1f) as u8)
            .set_transfer_last_burst_packet_count(last_burst_packet_count.min(0xf) as u8);
        trb.set_interrupt_on_completion();
        if block_event_interrupt && !last_packet {
            trb.set_block_event_interrupt();
        }

        let trb = transfer::Allowed::Isoch(trb);
        self.enque_trb(trb)
    }

    fn use_iso_sia(&self) -> bool {
        self.iso_start_asap
    }

    fn required_trbs(transfer: &Transfer) -> usize {
        match &transfer.kind {
            TransferKind::Control(_) => {
                if transfer.buffer_len() > 0 {
                    3
                } else {
                    2
                }
            }
            TransferKind::Bulk | TransferKind::Interrupt => 1,
            TransferKind::Isochronous { packet_lengths } => packet_lengths.len().max(1),
        }
    }

    fn ensure_ring_capacity(&self, required: usize) -> Result<(), TransferError> {
        let usable = self.ring.usable_capacity().saturating_sub(1);
        let stale = self.stale_noops.values().sum::<usize>();
        if self
            .outstanding_trbs
            .saturating_add(stale)
            .saturating_add(required)
            > usable
        {
            return Err(TransferError::QueueFull);
        }
        Ok(())
    }

    fn required_trbs_for_request(request: &TransferRequest) -> usize {
        match request {
            TransferRequest::Control { buffer, .. } => {
                if buffer.is_some_and(|buffer| buffer.len > 0) {
                    3
                } else {
                    2
                }
            }
            TransferRequest::Bulk { .. } | TransferRequest::Interrupt { .. } => 1,
            TransferRequest::Isochronous { packets, .. } => packets.len().max(1),
        }
    }

    /// The bare Stop Endpoint command; the cancel pass reads its error against the endpoint state itself.
    fn stop_command_future(&self) -> EndpointResetFuture {
        let (slot_id, endpoint_id) = (self.slot_id.as_u8(), self.dci.as_u8());
        let mut command_ring = self.cmd.clone();
        Box::pin(async move { stop_endpoint_command(&mut command_ring, slot_id, endpoint_id).await })
    }

    /// Gets the endpoint stopped, for a reconfiguration or a disconnect. As Linux, which sends no Stop Endpoint to
    /// reconfigure (Configure Endpoint works on a stopped endpoint, `xhci_check_bandwidth`) and takes a Context State
    /// Error for an endpoint whose context says Stopped as done (`xhci_handle_cmd_stop_ep`): an endpoint the output
    /// context already shows stopped (a cancel pass leaves it so) gets no command; a halted one is reset first
    /// (`EP_STATE_HALTED`: Reset Endpoint); a running one that answers with a context error is stopped again.
    pub(crate) fn stop_future(&self) -> EndpointResetFuture {
        let (slot_id, endpoint_id) = (self.slot_id.as_u8(), self.dci.as_u8());
        let mut command_ring = self.cmd.clone();
        let out_ctx = self.out_ctx.clone();
        let halted = self.halted.clone();
        Box::pin(async move {
            let state = || {
                out_ctx
                    .as_ref()
                    .map(|out_ctx| out_ctx.endpoint_state(endpoint_id))
            };
            let mut attempts = 0;
            loop {
                match stop_state(state()) {
                    StopState::Stopped if state().is_some() => return Ok(()),
                    StopState::Halted => {
                        reset_endpoint_command(&mut command_ring, slot_id, endpoint_id).await?;
                        halted.store(false, Ordering::Release);
                        return Ok(());
                    }
                    _ => {}
                }
                attempts += 1;
                match stop_endpoint_command(&mut command_ring, slot_id, endpoint_id).await {
                    Ok(()) => return Ok(()),
                    Err(error @ (TransferError::Timeout | TransferError::NoDevice)) => {
                        return Err(error);
                    }
                    Err(error) => match stop_state(state()) {
                        StopState::Stopped | StopState::Halted if state().is_some() => {}
                        StopState::Running if attempts < MAX_STOP_ATTEMPTS => {}
                        _ => return Err(error),
                    },
                }
            }
        })
    }

    pub(crate) fn retire_all_after_stop(&mut self) {
        // The dequeue pointer is set to the enqueue pointer next: nothing is left to take off the ring.
        self.cancelled_tds.clear();
        self.cancel_op = None;
        self.stale_noops.clear();
        let request_ids = self.inflight.keys().copied().collect::<Vec<_>>();
        for request_id in request_ids {
            if let Some(submitted) = self.remove_request(request_id) {
                self.stopped_completions.insert(request_id);
                submitted.waker.wake();
            }
        }
        debug_assert_eq!(self.outstanding_trbs, 0);
        debug_assert!(self.trb_to_request.is_empty());
    }

    pub(crate) fn resume_future(&self) -> EndpointResetFuture {
        let (dequeue_pointer, dequeue_cycle_state) = self.ring.enqueue_pointer();
        self.set_dequeue_future(dequeue_pointer, dequeue_cycle_state, true)
    }
}

impl Drop for Endpoint {
    /// TDs still in flight, and the buffers they own, go into the quarantine when the controller died without being
    /// seen to halt, as in `release_td`. Otherwise the endpoint is only dropped once the controller is done with it:
    /// the device keeps the endpoints of a slot not known to be disabled in the quarantine.
    fn drop(&mut self) {
        if self.inflight.is_empty() || !self.cmd.may_still_dma() {
            return;
        }
        for (_, submitted) in core::mem::take(&mut self.inflight) {
            self.release_td(submitted);
        }
    }
}

impl EndpointOp for Endpoint {
    fn submit_request(&mut self, submission: Submission) -> Result<RequestId, Rejected> {
        if self.cmd.is_dead() {
            return Err(submission.reject(TransferError::NoDevice));
        }
        // Whoever submits drives a pass that its owner left unpolled (a cancel followed by a drop).
        self.poll_cancel_op();
        if self.reset_in_progress.load(Ordering::Acquire) {
            return Err(submission.reject(TransferError::QueueFull));
        }
        let required_trbs = Self::required_trbs_for_request(submission.request());
        if let Err(error) = self.ensure_ring_capacity(required_trbs) {
            return Err(submission.reject(error));
        }
        let mut transfer =
            Transfer::from_submission(&self.kernel, submission).map_err(|error| (error, None))?;
        debug_assert_eq!(required_trbs, Self::required_trbs(&transfer));

        let mut data_bus_addr = 0;
        if transfer.buffer_len() > 0 {
            transfer.prepare_for_device();
            data_bus_addr = transfer.dma_addr();
            let buffer_end = data_bus_addr + transfer.buffer_len() as u64;
            let dma_mask = self.kernel.info().constraints().addr_mask;
            if data_bus_addr > dma_mask || buffer_end > dma_mask {
                let error = TransferError::Other(anyhow!(
                    "DMA buffer [{:#x}, {:#x}) exceeds controller DMA mask {:#x}",
                    data_bus_addr,
                    buffer_end,
                    dma_mask
                ));
                // Nothing was queued: the controller never saw the mapping.
                return Err((error, Some(transfer.take_buffer())));
            }
        }

        let data_len = transfer.buffer_len();
        let dir = transfer.direction;
        let request_id = self.allocate_request_id();

        let kind = match &transfer.kind {
            TransferKind::Control(t) => {
                let bm_request_type = BmRequestType {
                    direction: transfer.direction,
                    request_type: t.request_type,
                    recipient: t.recipient,
                };

                let mut setup = transfer::SetupStage::default();
                setup
                    .set_request_type(bm_request_type.into())
                    .set_request(t.request.into())
                    .set_value(t.value)
                    .set_index(t.index)
                    .set_length(0)
                    .set_transfer_type(transfer::TransferType::No);

                let mut data = None;

                if transfer.buffer_len() > 0 {
                    setup
                        .set_transfer_type(dir.to_xhci_transfer_type())
                        .set_length(data_len as _);

                    let mut _data = transfer::DataStage::default();
                    _data
                        .set_data_buffer_pointer(data_bus_addr)
                        .set_trb_transfer_length(data_len as _)
                        .set_direction(transfer.direction.to_xhci_direction());
                    if matches!(transfer.direction, Direction::In) {
                        _data.set_interrupt_on_short_packet();
                    }
                    data = Some(_data);
                }

                let mut status = transfer::StatusStage::default();
                status.set_interrupt_on_completion();

                if matches!(transfer.direction, Direction::In) && transfer.buffer_len() > 0 {
                    status.clear_direction();
                } else {
                    status.set_direction();
                }

                let has_data_stage = data.is_some();
                let mut control_trbs: Vec<transfer::Allowed> = vec![setup.into()];
                if let Some(data) = data {
                    control_trbs.push(data.into());
                }
                control_trbs.push(status.into());

                let control_addrs = self.ring.enqueue_transfer_td(&mut control_trbs);
                let mut control_addrs = control_addrs.into_iter();
                let setup_trb = TransferId(
                    control_addrs
                        .next()
                        .expect("control TD must contain setup TRB"),
                );
                let data_trb = if has_data_stage {
                    Some(TransferId(
                        control_addrs
                            .next()
                            .expect("control data TD must contain data TRB"),
                    ))
                } else {
                    None
                };
                let status_trb = TransferId(
                    control_addrs
                        .next()
                        .expect("control TD must contain status TRB"),
                );
                debug_assert!(
                    control_addrs.next().is_none(),
                    "control TD yielded more TRB addresses than expected"
                );
                for trb in [Some(setup_trb), data_trb, Some(status_trb)]
                    .into_iter()
                    .flatten()
                {
                    self.trb_to_request.insert(trb, request_id);
                }
                SubmittedTdKind::Control(ControlTd {
                    setup_trb,
                    data_trb,
                    status_trb,
                    requested: data_len,
                    actual: None,
                })
            }
            TransferKind::Interrupt | TransferKind::Bulk => {
                let trb = transfer::Allowed::Normal(
                    *Normal::new()
                        .set_data_buffer_pointer(data_bus_addr as _)
                        .set_trb_transfer_length(data_len as _)
                        .set_interrupter_target(0)
                        .set_interrupt_on_short_packet()
                        .set_interrupt_on_completion(),
                );
                let completion_trb = TransferId(self.ring.enque_transfer(trb));
                self.trb_to_request.insert(completion_trb, request_id);
                SubmittedTdKind::Normal { completion_trb }
            }
            TransferKind::Isochronous { packet_lengths } => {
                let packets = self.enque_iso(
                    data_bus_addr,
                    packet_lengths,
                    matches!(transfer.direction, Direction::In),
                );
                for packet in &packets {
                    self.trb_to_request.insert(packet.trb, request_id);
                }
                SubmittedTdKind::Iso { packets }
            }
        };

        self.outstanding_trbs += required_trbs;
        self.inflight.insert(
            request_id,
            SubmittedTd {
                transfer,
                kind,
                trb_count: required_trbs,
                cancelled: false,
                waker: AtomicWaker::new(),
            },
        );
        mb();
        // While the cancelled TDs are taken off the ring the doorbell stays quiet, as Linux's `xhci_ring_ep_doorbell`
        // for `EP_STOP_CMD_PENDING | SET_DEQ_PENDING`: the end of the pass rings it for the TDs that remain. The new
        // TD lies behind the enqueue point of the stop, so neither the No-Ops nor the new dequeue pointer reach it.
        if !self.cancel_in_progress() {
            self.doorbell();
        }

        Ok(Self::public_request_id(request_id))
    }

    fn reclaim_request(
        &mut self,
        id: RequestId,
    ) -> Option<Result<OwnedCompletion, TransferError>> {
        let request_id = Self::private_request_id(id);
        if self.stopped_completions.remove(&request_id) {
            return Some(Err(TransferError::Cancelled));
        }
        // A dead controller completes nothing any more; Linux gives every URB back (`xhci_hc_died`).
        if self.cmd.is_dead()
            && let Some(submitted) = self.remove_request(request_id)
        {
            self.release_td(submitted);
            return Some(Err(TransferError::NoDevice));
        }
        if let Some(held) = self.poll_soft_reset(id, request_id) {
            return held;
        }
        self.poll_cancel_op();
        if self.stopped_completions.remove(&request_id) {
            return Some(Err(TransferError::Cancelled));
        }
        // A cancelled TD is given back by the pass that takes it off the ring, not by an event of its own.
        if self.inflight.get(&request_id)?.cancelled {
            return None;
        }
        let target = match &self.inflight.get(&request_id)?.kind {
            SubmittedTdKind::Normal { completion_trb } => ReclaimTarget::Normal(*completion_trb),
            SubmittedTdKind::Control(control_td) => ReclaimTarget::Control(*control_td),
            SubmittedTdKind::Iso { .. } => ReclaimTarget::Iso,
        };
        match target {
            ReclaimTarget::Normal(completion_trb) => {
                let event = self.ring.get_finished(completion_trb.0)?;
                // Linux `process_bulk_intr_td`: a Stopped event is the endpoint stopping on the TD, which goes on
                // after the doorbell.
                if event_is_stop(&event) {
                    return None;
                }
                if self.start_soft_retry(request_id, completion_trb, event) {
                    return self.poll_soft_reset(id, request_id).unwrap_or(None);
                }
                Some(
                    self.complete_request(request_id, completion_trb, event)
                        .map(|transfer| transfer_to_completion(id, transfer)),
                )
            }
            ReclaimTarget::Control(control_td) => {
                self.reclaim_control_request(id, request_id, control_td)
            }
            ReclaimTarget::Iso => self.reclaim_iso_request(id, request_id),
        }
    }

    fn register_waker(&self, id: RequestId, cx: &mut core::task::Context<'_>) {
        let request_id = Self::private_request_id(id);
        if self.stopped_completions.contains(&request_id) {
            cx.waker().wake_by_ref();
            return;
        }
        let Some(submitted) = self.inflight.get(&request_id) else {
            return;
        };
        // The pass over the cancelled TDs is driven by polling `reclaim_request`; none of its commands wakes a task.
        if self.cancel_in_progress() {
            cx.waker().wake_by_ref();
        }
        submitted.waker.register(cx.waker());
        match &submitted.kind {
            SubmittedTdKind::Normal { completion_trb } => {
                self.ring.register_cx(completion_trb.0, cx);
            }
            SubmittedTdKind::Control(control_td) => {
                control_td.register_waker(&self.ring, cx);
            }
            SubmittedTdKind::Iso { packets } => {
                for packet in packets {
                    self.ring.register_cx(packet.trb.0, cx);
                }
            }
        }
    }

    fn cancel_request(&mut self, id: RequestId) -> Result<(), TransferError> {
        let request_id = Self::private_request_id(id);
        let submitted = self
            .inflight
            .get(&request_id)
            .ok_or(TransferError::InvalidEndpoint)?;
        if submitted.cancelled || self.has_final_event(submitted) {
            return Ok(());
        }
        if let Some(submitted) = self.inflight.get_mut(&request_id) {
            submitted.cancelled = true;
        }
        // `xhci_urb_dequeue`: the TD joins the list, and a Stop Endpoint starts the pass unless one is running.
        self.cancelled_tds.push(request_id);
        self.poll_cancel_op();
        Ok(())
    }

    fn abandon_request(&mut self, id: RequestId) -> Result<(), TransferError> {
        let submitted = self
            .remove_request(Self::private_request_id(id))
            .ok_or(TransferError::InvalidEndpoint)?;
        self.release_td(submitted);
        Ok(())
    }

    fn reset(&mut self) -> EndpointResetFuture {
        if !self.inflight.is_empty() || self.outstanding_trbs != 0 {
            return Box::pin(async { Err(TransferError::QueueFull) });
        }
        if !self.halted.load(Ordering::Acquire) {
            return Box::pin(async { Ok(()) });
        }
        if self
            .reset_in_progress
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Box::pin(async { Err(TransferError::QueueFull) });
        }

        let (dequeue_pointer, dequeue_cycle_state) = self.ring.enqueue_pointer();
        let slot_id = self.slot_id.as_u8();
        let endpoint_id = self.dci.as_u8();
        let mut cmd = self.cmd.clone();
        let halted = self.halted.clone();
        let reset_in_progress = self.reset_in_progress.clone();

        Box::pin(async move {
            let result = async {
                let reset_endpoint = *command::ResetEndpoint::default()
                    .set_endpoint_id(endpoint_id)
                    .set_slot_id(slot_id);
                cmd.cmd_request(command::Allowed::ResetEndpoint(reset_endpoint))
                    .await?;

                let mut set_dequeue_pointer = command::SetTrDequeuePointer::default();
                set_dequeue_pointer
                    .set_new_tr_dequeue_pointer(dequeue_pointer.raw())
                    .set_endpoint_id(endpoint_id)
                    .set_slot_id(slot_id);
                if dequeue_cycle_state {
                    set_dequeue_pointer.set_dequeue_cycle_state();
                } else {
                    set_dequeue_pointer.clear_dequeue_cycle_state();
                }
                cmd.cmd_request(command::Allowed::SetTrDequeuePointer(set_dequeue_pointer))
                    .await?;
                Ok(())
            }
            .await;

            if result.is_ok() {
                halted.store(false, Ordering::Release);
            }
            reset_in_progress.store(false, Ordering::Release);
            result
        })
    }
}

/// Whether a transfer completion code halts a non-isochronous endpoint, as Linux's `xhci_halted_host_endpoint`
/// (xhci-ring.c:2191-2215): Stall, Babble Detected, USB Transaction Error and Split Transaction Error. Linux's
/// exception for a babbling control endpoint on an xHCI 0.95 controller (it reads the endpoint context) is not
/// ported: it concerns only controllers older than xHCI 0.96.
fn completion_code_halts(code: Result<CompletionCode, u8>) -> bool {
    matches!(
        code,
        Ok(CompletionCode::StallError)
            | Ok(CompletionCode::BabbleDetectedError)
            | Ok(CompletionCode::UsbTransactionError)
            | Ok(CompletionCode::SplitTransactionError)
    )
}

/// Whether the event is the endpoint stopping on the TD (Linux `COMP_STOPPED`, `COMP_STOPPED_LENGTH_INVALID`,
/// `COMP_STOPPED_SHORT_PACKET`), not the TD finishing.
fn event_is_stop(event: &TransferEvent) -> bool {
    matches!(
        event.completion_code(),
        Ok(CompletionCode::Stopped
            | CompletionCode::StoppedLengthInvalid
            | CompletionCode::StoppedShortPacket)
    )
}

fn completion_halts_endpoint(event: &TransferEvent) -> bool {
    completion_code_halts(event.completion_code())
}

/// Linux's soft-retry rule for a bulk or interrupt TD (`process_bulk_intr_td`): a USB Transaction Error on an
/// endpoint that allows it is retried while `err_count`, counted up here, was at most `MAX_SOFT_RETRY`.
fn soft_retry_allowed(
    endpoint_allows: bool,
    code: Result<CompletionCode, u8>,
    err_count: &mut u32,
) -> bool {
    if !endpoint_allows || !matches!(code, Ok(CompletionCode::UsbTransactionError)) {
        return false;
    }
    let previous = *err_count;
    *err_count = err_count.saturating_add(1);
    previous <= MAX_SOFT_RETRY
}

fn iso_packet_actual_length(
    requested: usize,
    event: TransferEvent,
) -> Result<usize, TransferError> {
    let remaining = event.trb_transfer_length() as usize;
    match event.completion_code() {
        Ok(code) => match code {
            xhci::ring::trb::event::CompletionCode::Success => {
                if remaining == 0 {
                    Ok(requested)
                } else {
                    Ok(requested.saturating_sub(remaining))
                }
            }
            xhci::ring::trb::event::CompletionCode::ShortPacket
            | xhci::ring::trb::event::CompletionCode::BabbleDetectedError
            | xhci::ring::trb::event::CompletionCode::IsochBufferOverrun
            | xhci::ring::trb::event::CompletionCode::MissedServiceError
            | xhci::ring::trb::event::CompletionCode::UsbTransactionError
            | xhci::ring::trb::event::CompletionCode::Stopped => {
                Ok(requested.saturating_sub(remaining))
            }
            xhci::ring::trb::event::CompletionCode::StoppedShortPacket => Ok(remaining),
            xhci::ring::trb::event::CompletionCode::StoppedLengthInvalid => Ok(0),
            code => {
                code.to_result()?;
                Ok(requested.saturating_sub(remaining))
            }
        },
        Err(e) => Err(TransferError::Other(anyhow!(
            "unknown XHCI ISO completion code: {e:?}"
        ))),
    }
}

fn iso_packet_is_fatal(event: TransferEvent) -> bool {
    match event.completion_code() {
        Ok(
            xhci::ring::trb::event::CompletionCode::Success
            | xhci::ring::trb::event::CompletionCode::ShortPacket
            | xhci::ring::trb::event::CompletionCode::BabbleDetectedError
            | xhci::ring::trb::event::CompletionCode::IsochBufferOverrun
            | xhci::ring::trb::event::CompletionCode::MissedServiceError
            | xhci::ring::trb::event::CompletionCode::UsbTransactionError
            | xhci::ring::trb::event::CompletionCode::Stopped
            | xhci::ring::trb::event::CompletionCode::StoppedShortPacket
            | xhci::ring::trb::event::CompletionCode::StoppedLengthInvalid,
        ) => false,
        Ok(_) | Err(_) => true,
    }
}

pub(crate) trait EndpointDescriptorExt {
    fn endpoint_type(&self) -> xhci::context::EndpointType;
}

impl EndpointDescriptorExt for EndpointDescriptor {
    fn endpoint_type(&self) -> xhci::context::EndpointType {
        match self.transfer_type {
            descriptor::EndpointType::Control => xhci::context::EndpointType::Control,
            descriptor::EndpointType::Isochronous => match self.direction {
                usb_if::transfer::Direction::Out => xhci::context::EndpointType::IsochOut,
                usb_if::transfer::Direction::In => xhci::context::EndpointType::IsochIn,
            },
            descriptor::EndpointType::Bulk => match self.direction {
                usb_if::transfer::Direction::Out => xhci::context::EndpointType::BulkOut,
                usb_if::transfer::Direction::In => xhci::context::EndpointType::BulkIn,
            },
            descriptor::EndpointType::Interrupt => match self.direction {
                usb_if::transfer::Direction::Out => xhci::context::EndpointType::InterruptOut,
                usb_if::transfer::Direction::In => xhci::context::EndpointType::InterruptIn,
            },
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    #[test]
    fn halting_completion_codes_match_linux_handle_tx_event() {
        use xhci::ring::trb::event::CompletionCode::*;
        for code in [
            StallError,
            BabbleDetectedError,
            UsbTransactionError,
            SplitTransactionError,
        ] {
            assert!(
                completion_code_halts(Ok(code)),
                "{code:?} must halt the endpoint"
            );
        }
        for code in [
            Success,
            ShortPacket,
            DataBufferError,
            TrbError,
            RingUnderrun,
        ] {
            assert!(
                !completion_code_halts(Ok(code)),
                "{code:?} must not halt the endpoint"
            );
        }
        assert!(
            !completion_code_halts(Err(200)),
            "unknown codes are not halting"
        );
    }

    #[test]
    fn transaction_errors_are_soft_retried_four_times_then_halt() {
        use xhci::ring::trb::event::CompletionCode::*;
        let mut err_count = 0;
        // err_count 0, 1, 2, 3 before counting: retried (Linux `ep->err_count++ > MAX_SOFT_RETRY` is false).
        for retry in 0..4 {
            assert!(
                soft_retry_allowed(true, Ok(UsbTransactionError), &mut err_count),
                "retry {retry}"
            );
        }
        // The fifth error in a row halts the endpoint for the driver's reset.
        assert!(!soft_retry_allowed(
            true,
            Ok(UsbTransactionError),
            &mut err_count
        ));
        assert!(completion_code_halts(Ok(UsbTransactionError)));
    }

    #[test]
    fn soft_retry_is_only_for_transaction_errors_on_endpoints_that_allow_it() {
        use xhci::ring::trb::event::CompletionCode::*;
        let mut err_count = 0;
        assert!(!soft_retry_allowed(
            false,
            Ok(UsbTransactionError),
            &mut err_count
        ));
        assert_eq!(
            err_count, 0,
            "a control, isochronous or TT endpoint does not count"
        );
        for code in [
            StallError,
            BabbleDetectedError,
            SplitTransactionError,
            Success,
        ] {
            assert!(
                !soft_retry_allowed(true, Ok(code), &mut err_count),
                "{code:?}"
            );
        }
        assert_eq!(err_count, 0);
    }

    use alloc::{
        alloc::{alloc_zeroed, dealloc},
        sync::Arc,
    };
    use core::{
        alloc::Layout,
        num::NonZeroUsize,
        ptr::NonNull,
        sync::atomic::{AtomicU64, Ordering as AtomicOrdering},
    };

    use ax_sync::SpinRwLock as RwLock;
    use dma_api::{DmaAllocHandle, DmaConstraints, DmaError, DmaMapHandle, DmaOp};
    use usb_if::err::TransferError;

    use super::*;
    use crate::backend::kmod::xhci::context::ContextData;
    use crate::backend::ty::ep::{EndpointHandle, OwnedRequest};
    use crate::{
        backend::kmod::xhci::{reg::XhciRegisters, transfer::TransferResultHandler},
        osal::KernelOp,
    };

    pub(crate) struct TestKernel;

    static TEST_DMA_ADDR: AtomicU64 = AtomicU64::new(0x1000);
    extern crate std;

    std::thread_local! {
        // Per test thread: tests run in parallel and map through the one `TEST_KERNEL`, so a shared counter let
        // another test's mapping show up in the count a test asserts. Mapping is synchronous, on the test's thread.
        static ACTIVE_STREAMING_MAPPINGS: core::cell::Cell<usize> = const { core::cell::Cell::new(0) };
        // Coherent (and contiguous) allocations not yet freed, per test thread for the same reason: rings, contexts.
        static LIVE_COHERENT: core::cell::Cell<usize> = const { core::cell::Cell::new(0) };
        // The bounce buffer of each streaming mapping, by its fake bus address: what a test writes as the device.
        static BOUNCES: core::cell::RefCell<alloc::collections::BTreeMap<u64, (NonNull<u8>, usize)>> =
            const { core::cell::RefCell::new(alloc::collections::BTreeMap::new()) };
    }

    /// Coherent DMA allocations of this test thread not freed yet.
    pub(crate) fn live_coherent() -> usize {
        LIVE_COHERENT.with(|count| count.get())
    }

    impl DmaOp for TestKernel {
        fn page_size(&self) -> usize {
            4096
        }

        unsafe fn alloc_contiguous(
            &self,
            constraints: DmaConstraints,
            layout: Layout,
        ) -> Option<DmaAllocHandle> {
            // SAFETY: The test kernel models contiguous DMA with the same heap-backed
            // allocation used for coherent DMA below.
            unsafe { self.alloc_coherent(constraints, layout) }
        }

        unsafe fn dealloc_contiguous(&self, handle: DmaAllocHandle) {
            // SAFETY: Contiguous handles are allocated by `alloc_coherent` above.
            unsafe { self.dealloc_coherent(handle) }
                .expect("test coherent DMA release must succeed")
        }

        unsafe fn alloc_coherent(
            &self,
            constraints: DmaConstraints,
            layout: Layout,
        ) -> Option<DmaAllocHandle> {
            // SAFETY: Unit tests pass valid layouts, and the allocation remains owned by
            // the returned DMA handle until `dealloc_coherent` consumes it.
            let ptr = NonNull::new(unsafe { alloc_zeroed(layout) })?;
            let align = constraints.align.max(layout.align()).max(1) as u64;
            let size = layout.size().max(1) as u64;
            let current = TEST_DMA_ADDR.fetch_add(size + align, AtomicOrdering::Relaxed);
            let dma_addr = (current + align - 1) & !(align - 1);
            LIVE_COHERENT.with(|count| count.set(count.get() + 1));
            // SAFETY: `ptr` and `layout` describe the allocation above. The fake bus
            // address is deterministic and never reaches hardware.
            Some(unsafe { DmaAllocHandle::new(ptr, ptr, dma_addr.into(), layout) })
        }

        unsafe fn dealloc_coherent(&self, handle: DmaAllocHandle) -> Result<(), DmaError> {
            // SAFETY: The handle was allocated by `alloc_zeroed` with this exact layout.
            unsafe { dealloc(handle.as_ptr().as_ptr(), handle.layout()) };
            LIVE_COHERENT.with(|count| count.set(count.get() - 1));
            Ok(())
        }

        unsafe fn map_streaming(
            &self,
            constraints: DmaConstraints,
            addr: NonNull<u8>,
            size: NonZeroUsize,
            _direction: DmaDirection,
        ) -> Result<DmaMapHandle, DmaError> {
            let layout = Layout::from_size_align(size.get(), 1)?;
            let align = constraints.align.max(1) as u64;
            let current =
                TEST_DMA_ADDR.fetch_add(size.get() as u64 + align, AtomicOrdering::Relaxed);
            let dma_addr = (current + align - 1) & !(align - 1);
            // Bounced, as the kernel's glue always does: the "device" (a test) writes the bounce buffer, and
            // completing an IN copies it into the caller's buffer, the copy of unsafe audit F-01.
            // SAFETY: `layout` has a non-zero size.
            let bounce = NonNull::new(unsafe { alloc_zeroed(layout) }).ok_or(DmaError::NoMemory)?;
            // SAFETY: The caller keeps `addr..addr + size` alive until `unmap_streaming` (the transfer owns it), and
            // `bounce` is a live allocation of the same size, freed in `unmap_streaming`. The fake bus address never
            // reaches hardware.
            let handle =
                unsafe { DmaMapHandle::new(addr, dma_addr.into(), layout, Some(bounce)) };
            ACTIVE_STREAMING_MAPPINGS.with(|count| count.set(count.get() + 1));
            BOUNCES.with(|bounces| bounces.borrow_mut().insert(dma_addr, (bounce, size.get())));
            Ok(handle)
        }

        unsafe fn unmap_streaming(&self, handle: DmaMapHandle) {
            ACTIVE_STREAMING_MAPPINGS.with(|count| count.set(count.get() - 1));
            BOUNCES.with(|bounces| bounces.borrow_mut().remove(&handle.dma_addr().as_u64()));
            let bounce = handle.bounce_ptr().expect("test mappings always bounce");
            // SAFETY: `map_streaming` allocated `bounce` with this layout.
            unsafe { dealloc(bounce.as_ptr(), handle.layout()) };
        }
    }

    impl KernelOp for TestKernel {
        fn delay(&self, _duration: core::time::Duration) {}
    }

    pub(crate) static TEST_KERNEL: TestKernel = TestKernel;

    pub(crate) fn test_device_dma() -> dma_api::DeviceDma {
        dma_api::DeviceDma::new(
            dma_api::DmaDeviceInfo::new(
                dma_api::DmaDomainId::Direct,
                dma_api::DmaCoherency::NonCoherent,
                dma_api::DmaConstraints::new(u64::MAX),
            ),
            &TEST_KERNEL,
        )
    }

    pub(crate) fn test_kernel() -> Kernel {
        Kernel::new(
            dma_api::DeviceDma::new(
                dma_api::DmaDeviceInfo::new(
                    dma_api::DmaDomainId::Direct,
                    dma_api::DmaCoherency::NonCoherent,
                    dma_api::DmaConstraints::new(u64::MAX),
                ),
                &TEST_KERNEL,
            ),
            &TEST_KERNEL,
        )
    }

    #[repr(align(64))]
    struct AlignedMmio([u32; 4096]);

    fn test_endpoint() -> (Box<AlignedMmio>, Endpoint) {
        let mut mmio = Box::new(AlignedMmio([0; 4096]));
        // The xHCI accessor requires a doorbell entry for slot one and a port array.
        mmio.0[1] = 2 | (1 << 24);
        let mmio_base = NonNull::new(mmio.0.as_mut_ptr().cast::<u8>()).unwrap();
        let registers = Arc::new(RwLock::new(XhciRegisters::new(mmio_base)));
        let slot_id = SlotId::from(1);
        let bell = Arc::new(Mutex::new(SlotBell::new(slot_id, registers.read().clone())));
        let kernel = test_kernel();
        let command_ring = CommandRing::new(
            DmaDirection::Bidirectional,
            &kernel,
            registers,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let endpoint = Endpoint::new(slot_id, Dci::from(2), &kernel, bell, command_ring).unwrap();
        (mmio, endpoint)
    }

    fn bulk_out(len: usize) -> Submission {
        Submission::from(OwnedRequest::bulk_out(vec![0u8; len].into_boxed_slice()))
    }

    /// What the controller would do for a TD: write `data` at the bus address of its buffer (the bounce buffer).
    fn device_writes(dma_addr: u64, data: &[u8]) {
        BOUNCES.with(|bounces| {
            let (bounce, len) = bounces.borrow()[&dma_addr];
            assert!(data.len() <= len);
            // SAFETY: `bounce` is the live bounce allocation of `len` bytes behind this mapping; nothing else
            // accesses it during this copy.
            unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), bounce.as_ptr(), data.len()) };
        });
    }

    /// A Transfer Event with `code` and `remaining` bytes not transferred.
    fn transfer_event(code: CompletionCode, remaining: u32) -> TransferEvent {
        let mut raw = TransferEvent::default().into_raw();
        raw[2] = ((code as u32) << 24) | remaining;
        TransferEvent::try_from(raw).unwrap()
    }

    /// The bus address of the only TD in flight's buffer, and its completion TRB.
    fn only_normal_td(endpoint: &Endpoint) -> (u64, TransferId) {
        let submitted = endpoint.inflight.values().next().unwrap();
        let SubmittedTdKind::Normal { completion_trb } = submitted.kind else {
            panic!("not a normal TD");
        };
        (submitted.transfer.dma_addr(), completion_trb)
    }

    /// Unsafe audit F-01: the buffer moves into the request, so the completion's copy from the bounce buffer lands in
    /// memory the request owns, and the caller gets that buffer back. Run under Miri, the old API (a borrowed buffer
    /// dropped before `reclaim`) was a write to freed memory; it no longer compiles, since `submit` takes an
    /// `OwnedRequest` and nothing else.
    #[test]
    fn an_owned_bulk_in_buffer_comes_back_with_the_received_bytes() {
        let (_mmio, mut endpoint) = test_endpoint();
        let request = OwnedRequest::bulk_in(vec![0xaau8; 64].into_boxed_slice());
        let id = endpoint.submit_request(Submission::from(request)).unwrap();
        assert_eq!(ACTIVE_STREAMING_MAPPINGS.with(|count| count.get()), 1);
        let (dma_addr, trb) = only_normal_td(&endpoint);

        device_writes(dma_addr, b"hello");
        endpoint
            .ring
            .finished_handle()
            .set_finished(trb.0, transfer_event(CompletionCode::ShortPacket, 59));
        let done = endpoint.reclaim_request(id).unwrap().unwrap();

        assert_eq!(done.actual_length, 5);
        assert_eq!(done.buffer.len(), 64);
        assert_eq!(&done.buffer[..5], b"hello");
        assert!(endpoint.inflight.is_empty());
        assert_eq!(ACTIVE_STREAMING_MAPPINGS.with(|count| count.get()), 0, "unmapped before it is handed back");
    }

    /// The endpoint goes away (its last handle dropped) with an IN in flight on a controller that died without being
    /// seen to halt: the TD and the buffer it owns are quarantined, not freed, since the controller may still write.
    #[test]
    fn an_endpoint_dropped_with_a_request_in_flight_keeps_its_buffer() {
        let mappings = || ACTIVE_STREAMING_MAPPINGS.with(|count| count.get());
        let before = mappings();
        let (_mmio, endpoint, dead, _) = endpoint_with_a_td_in_flight();
        let quarantine = endpoint.cmd.quarantine().clone();
        let handle = EndpointHandle::new(
            usb_if::endpoint::EndpointInfo {
                address: usb_if::endpoint::EndpointAddress::new(0x81),
                transfer_type: descriptor::EndpointType::Bulk,
                direction: Direction::In,
                max_packet_size: 512,
                packets_per_microframe: 1,
                interval: 0,
            },
            endpoint,
        );
        let id = handle
            .submit(OwnedRequest::bulk_in(vec![0u8; 512].into_boxed_slice()))
            .unwrap();
        assert_eq!(mappings(), before + 2);
        assert_eq!(handle.reclaim(id).unwrap().map(|done| done.actual_length), None);

        dead.store(true, AtomicOrdering::Release);
        drop(handle);

        assert_eq!(mappings(), before + 2, "both TDs, with their buffers, are kept");
        assert_eq!(quarantine.len(), 2);
    }

    /// The same drop on a controller that is alive frees the TDs, as before: the device only lets an endpoint go
    /// once its slot is disabled, and keeps it in the quarantine otherwise.
    #[test]
    fn an_endpoint_dropped_on_a_live_controller_frees_its_tds() {
        let mappings = || ACTIVE_STREAMING_MAPPINGS.with(|count| count.get());
        let before = mappings();
        let (_mmio, endpoint, _dead, _) = endpoint_with_a_td_in_flight();
        let quarantine = endpoint.cmd.quarantine().clone();
        drop(endpoint);
        assert_eq!(mappings(), before);
        assert_eq!(quarantine.len(), 0);
    }

    #[test]
    fn a_dead_controller_fails_new_and_in_flight_transfers() {
        let mut mmio = Box::new(AlignedMmio([0; 4096]));
        mmio.0[1] = 2 | (1 << 24);
        let mmio_base = NonNull::new(mmio.0.as_mut_ptr().cast::<u8>()).unwrap();
        let registers = Arc::new(RwLock::new(XhciRegisters::new(mmio_base)));
        let slot_id = SlotId::from(1);
        let bell = Arc::new(Mutex::new(SlotBell::new(slot_id, registers.read().clone())));
        let kernel = test_kernel();
        let dead = Arc::new(AtomicBool::new(false));
        let command_ring = CommandRing::new(
            DmaDirection::Bidirectional,
            &kernel,
            registers,
            dead.clone(),
        )
        .unwrap();
        let mut endpoint =
            Endpoint::new(slot_id, Dci::from(2), &kernel, bell, command_ring).unwrap();
        let in_flight = endpoint
            .submit_request(bulk_out(64))
            .unwrap();

        dead.store(true, AtomicOrdering::Release);

        assert!(matches!(
            endpoint.reclaim_request(in_flight),
            Some(Err(TransferError::NoDevice))
        ));
        assert!(endpoint.inflight.is_empty());
        assert!(matches!(
            endpoint.submit_request(bulk_out(64)),
            Err((TransferError::NoDevice, Some(_)))
        ));
        let _ = mmio;
    }

    /// An endpoint on a controller that is about to die, with one bulk OUT in flight.
    fn endpoint_with_a_td_in_flight() -> (Box<AlignedMmio>, Endpoint, Arc<AtomicBool>, RequestId) {
        let mut mmio = Box::new(AlignedMmio([0; 4096]));
        mmio.0[1] = 2 | (1 << 24);
        let mmio_base = NonNull::new(mmio.0.as_mut_ptr().cast::<u8>()).unwrap();
        let registers = Arc::new(RwLock::new(XhciRegisters::new(mmio_base)));
        let slot_id = SlotId::from(1);
        let bell = Arc::new(Mutex::new(SlotBell::new(slot_id, registers.read().clone())));
        let kernel = test_kernel();
        let dead = Arc::new(AtomicBool::new(false));
        let command_ring =
            CommandRing::new(DmaDirection::Bidirectional, &kernel, registers, dead.clone()).unwrap();
        let mut endpoint =
            Endpoint::new(slot_id, Dci::from(2), &kernel, bell, command_ring).unwrap();
        // The TD owns its buffer: a quarantined TD keeps it.
        let in_flight = endpoint.submit_request(bulk_out(64)).unwrap();
        (mmio, endpoint, dead, in_flight)
    }

    #[test]
    fn a_dead_controller_not_seen_to_halt_keeps_the_td_buffer() {
        let mappings = || ACTIVE_STREAMING_MAPPINGS.with(|count| count.get());
        let before = mappings();
        let (_mmio, mut endpoint, dead, in_flight) = endpoint_with_a_td_in_flight();
        assert_eq!(mappings(), before + 1);

        // Dead, HCH never seen (halt timed out, or registers read all ones).
        dead.store(true, AtomicOrdering::Release);
        assert!(endpoint.cmd.may_still_dma());

        assert!(matches!(
            endpoint.reclaim_request(in_flight),
            Some(Err(TransferError::NoDevice))
        ));
        assert!(endpoint.inflight.is_empty(), "the caller is done with it");
        assert_eq!(mappings(), before + 1, "the bounce buffer is not unmapped");
        assert_eq!(endpoint.cmd.quarantine().len(), 1);
        drop(endpoint);
        assert_eq!(mappings(), before + 1, "not even when the endpoint goes");
    }

    #[test]
    fn a_dead_controller_not_seen_to_halt_keeps_an_abandoned_td_buffer() {
        let mappings = || ACTIVE_STREAMING_MAPPINGS.with(|count| count.get());
        let before = mappings();
        let (_mmio, mut endpoint, dead, in_flight) = endpoint_with_a_td_in_flight();
        dead.store(true, AtomicOrdering::Release);

        endpoint.abandon_request(in_flight).unwrap();
        assert_eq!(mappings(), before + 1);
        assert_eq!(endpoint.cmd.quarantine().len(), 1);
    }

    #[test]
    fn a_dead_controller_seen_to_halt_frees_the_td_buffer() {
        let mappings = || ACTIVE_STREAMING_MAPPINGS.with(|count| count.get());
        let before = mappings();
        let (_mmio, mut endpoint, dead, in_flight) = endpoint_with_a_td_in_flight();

        dead.store(true, AtomicOrdering::Release);
        endpoint.cmd.halted_flag().store(true, AtomicOrdering::Release);
        assert!(!endpoint.cmd.may_still_dma());

        assert!(matches!(
            endpoint.reclaim_request(in_flight),
            Some(Err(TransferError::NoDevice))
        ));
        assert_eq!(mappings(), before, "HCH seen: the buffer is freed, as before");
        assert_eq!(endpoint.cmd.quarantine().len(), 0);
    }

    #[test]
    fn stopped_iso_request_publishes_cancelled_exactly_once() {
        let (_mmio, mut endpoint) = test_endpoint();
        let request_id = endpoint
            .submit_request(Submission::from(OwnedRequest::iso_out(
                vec![0u8; 128].into_boxed_slice(),
                &[64, 64],
            )))
            .unwrap();

        assert_eq!(endpoint.inflight.len(), 1);
        assert_eq!(endpoint.trb_to_request.len(), 2);
        assert_eq!(endpoint.outstanding_trbs, 2);
        assert_eq!(ACTIVE_STREAMING_MAPPINGS.with(|count| count.get()), 1);

        endpoint.cancel_request(request_id).unwrap();
        endpoint.retire_all_after_stop();

        assert!(endpoint.inflight.is_empty());
        assert!(endpoint.trb_to_request.is_empty());
        assert_eq!(endpoint.outstanding_trbs, 0);
        assert_eq!(ACTIVE_STREAMING_MAPPINGS.with(|count| count.get()), 0);
        assert!(matches!(
            endpoint.reclaim_request(request_id),
            Some(Err(TransferError::Cancelled))
        ));
        assert!(endpoint.reclaim_request(request_id).is_none());
    }

    // The cancel pass (Linux `xhci_urb_dequeue`, `xhci_handle_cmd_stop_ep`, `xhci_invalidate_cancelled_tds`,
    // `xhci_handle_cmd_set_deq`), with the controller played by the test.

    const CMD_RESET_ENDPOINT: u32 = 14;
    const CMD_STOP_ENDPOINT: u32 = 15;
    const CMD_SET_TR_DEQUEUE: u32 = 16;
    const TRB_TYPE_NOOP: u32 = 8;

    /// The type of the command TRB the endpoint queued as its `index`th command.
    fn command_type(endpoint: &Endpoint, index: usize) -> u32 {
        (endpoint.cmd.trb_word(index, 3) >> 10) & 0x3f
    }

    /// The controller completes the `index`th command of the endpoint's command ring with `code`.
    fn complete_command(endpoint: &Endpoint, index: usize, code: CompletionCode) {
        let mut raw = xhci::ring::trb::event::CommandCompletion::default().into_raw();
        raw[2] = (code as u32) << 24;
        let event = xhci::ring::trb::event::CommandCompletion::try_from(raw).unwrap();
        let addr = BusAddr::from(endpoint.cmd.bus_addr().raw() + (index * 16) as u64);
        endpoint.cmd.finished_handle().set_finished(addr, event);
    }

    fn td_trb(endpoint: &Endpoint, id: RequestId) -> BusAddr {
        match endpoint.inflight[&Endpoint::private_request_id(id)].kind {
            SubmittedTdKind::Normal { completion_trb } => completion_trb.0,
            _ => panic!("not a normal TD"),
        }
    }

    fn ring_index(endpoint: &Endpoint, addr: BusAddr) -> u64 {
        (addr.raw() - endpoint.ring.bus_addr().raw()) / 16
    }

    /// Three bulk OUT TDs in flight, TRBs 0, 1 and 2 of the ring.
    fn three_tds() -> (Box<AlignedMmio>, Endpoint, [RequestId; 3]) {
        let (mmio, mut endpoint) = test_endpoint();
        let ids = [
            endpoint.submit_request(bulk_out(64)).unwrap(),
            endpoint.submit_request(bulk_out(64)).unwrap(),
            endpoint.submit_request(bulk_out(64)).unwrap(),
        ];
        (mmio, endpoint, ids)
    }

    fn trb_type(endpoint: &Endpoint, addr: BusAddr) -> u32 {
        (endpoint.ring.read_trb_word(addr, 3).unwrap() >> 10) & 0x3f
    }

    #[test]
    fn a_cancelled_td_off_the_controllers_dequeue_pointer_becomes_no_ops_and_is_given_back() {
        let (_mmio, mut endpoint, [a, b, c]) = three_tds();
        let (trb_a, trb_b, trb_c) = (td_trb(&endpoint, a), td_trb(&endpoint, b), td_trb(&endpoint, c));
        let mappings = ACTIVE_STREAMING_MAPPINGS.with(|count| count.get());

        endpoint.cancel_request(b).unwrap();
        // Stop Endpoint is queued, the request waits for it.
        assert_eq!(command_type(&endpoint, 0), CMD_STOP_ENDPOINT);
        assert!(endpoint.reclaim_request(b).is_none());
        assert_eq!(endpoint.inflight.len(), 3, "not given back before the stop completes");

        complete_command(&endpoint, 0, CompletionCode::Success);
        assert!(matches!(
            endpoint.reclaim_request(b),
            Some(Err(TransferError::Cancelled))
        ));
        assert!(endpoint.reclaim_request(b).is_none(), "reported once");
        assert_eq!(endpoint.inflight.len(), 2);
        assert_eq!(
            ACTIVE_STREAMING_MAPPINGS.with(|count| count.get()),
            mappings - 1,
            "the cancelled TD's buffer is unmapped"
        );
        assert_eq!(trb_type(&endpoint, trb_b), TRB_TYPE_NOOP);
        assert_eq!(trb_type(&endpoint, trb_a), 1, "the other TDs are untouched");
        assert_eq!(trb_type(&endpoint, trb_c), 1);
        assert!(endpoint.cancel_op.is_none() && endpoint.cancelled_tds.is_empty());
        assert_eq!(endpoint.cmd.trb_word(1, 3) & 0xfc00, 0, "no Set TR Dequeue Pointer was sent");

        // The No-Op still takes room in the ring until the controller is known to be past it.
        assert_eq!(endpoint.stale_noops.len(), 1);
        for (id, trb) in [(a, trb_a), (c, trb_c)] {
            endpoint
                .ring
                .finished_handle()
                .set_finished(trb, transfer_event(CompletionCode::Success, 0));
            assert!(endpoint.reclaim_request(id).unwrap().is_ok());
        }
        assert!(endpoint.stale_noops.is_empty());
    }

    #[test]
    fn the_td_the_controller_stopped_on_is_stepped_over_with_a_set_tr_dequeue_pointer() {
        let (_mmio, mut endpoint, [a, b, c]) = three_tds();
        let (trb_a, trb_b, trb_c) = (td_trb(&endpoint, a), td_trb(&endpoint, b), td_trb(&endpoint, c));
        // The controller stopped on B: a Stopped event for it, and for A, which is not cancelled, none.
        endpoint
            .ring
            .finished_handle()
            .set_finished(trb_b, transfer_event(CompletionCode::Stopped, 64));

        endpoint.cancel_request(b).unwrap();
        complete_command(&endpoint, 0, CompletionCode::Success);
        assert!(endpoint.reclaim_request(b).is_none(), "waits for the Set TR Dequeue Pointer");
        assert_eq!(command_type(&endpoint, 1), CMD_SET_TR_DEQUEUE);
        // New dequeue pointer: the TRB after B (TRB 2), cycle state 1 (first lap).
        let expected = trb_c.raw() | 1;
        assert_eq!(endpoint.cmd.trb_word(1, 0), expected as u32);
        assert_eq!(endpoint.cmd.trb_word(1, 1), (expected >> 32) as u32);
        assert_eq!(ring_index(&endpoint, trb_b), 1);
        assert_eq!(endpoint.inflight.len(), 3);

        complete_command(&endpoint, 1, CompletionCode::Success);
        assert!(matches!(
            endpoint.reclaim_request(b),
            Some(Err(TransferError::Cancelled))
        ));
        assert_eq!(endpoint.inflight.len(), 2);
        assert!(endpoint.stale_noops.is_empty(), "the dequeue pointer is past B");
        assert!(endpoint.quarantine_len() == 0);
        // The TDs that go on complete as before.
        for (id, trb) in [(a, trb_a), (c, trb_c)] {
            endpoint
                .ring
                .finished_handle()
                .set_finished(trb, transfer_event(CompletionCode::Success, 0));
            assert!(endpoint.reclaim_request(id).unwrap().is_ok());
        }
    }

    #[test]
    fn a_failed_set_tr_dequeue_pointer_keeps_the_buffer_of_the_td_the_controller_may_still_hold() {
        let (_mmio, mut endpoint, [_a, b, _c]) = three_tds();
        let trb_b = td_trb(&endpoint, b);
        let mappings = ACTIVE_STREAMING_MAPPINGS.with(|count| count.get());
        endpoint
            .ring
            .finished_handle()
            .set_finished(trb_b, transfer_event(CompletionCode::Stopped, 64));

        endpoint.cancel_request(b).unwrap();
        complete_command(&endpoint, 0, CompletionCode::Success);
        assert!(endpoint.reclaim_request(b).is_none());
        complete_command(&endpoint, 1, CompletionCode::ContextStateError);

        assert!(matches!(
            endpoint.reclaim_request(b),
            Some(Err(TransferError::Cancelled))
        ));
        assert_eq!(ACTIVE_STREAMING_MAPPINGS.with(|count| count.get()), mappings, "kept, not unmapped");
        assert_eq!(endpoint.quarantine_len(), 1);
    }

    #[test]
    fn a_stopped_event_on_a_td_that_is_not_cancelled_is_not_its_completion() {
        let (_mmio, mut endpoint) = test_endpoint();
        let id = endpoint.submit_request(bulk_out(64)).unwrap();
        let trb = td_trb(&endpoint, id);
        for code in [
            CompletionCode::Stopped,
            CompletionCode::StoppedLengthInvalid,
            CompletionCode::StoppedShortPacket,
        ] {
            endpoint
                .ring
                .finished_handle()
                .set_finished(trb, transfer_event(code, 64));
            assert!(endpoint.reclaim_request(id).is_none(), "{code:?}");
            assert_eq!(endpoint.inflight.len(), 1, "{code:?}");
        }
        endpoint
            .ring
            .finished_handle()
            .set_finished(trb, transfer_event(CompletionCode::Success, 0));
        assert!(endpoint.reclaim_request(id).unwrap().is_ok());
    }

    #[test]
    fn the_stopped_events_of_the_tds_that_go_on_are_cleared_so_that_their_completions_arrive() {
        let (_mmio, mut endpoint, [a, b, _c]) = three_tds();
        let trb_a = td_trb(&endpoint, a);
        endpoint
            .ring
            .finished_handle()
            .set_finished(trb_a, transfer_event(CompletionCode::Stopped, 64));

        endpoint.cancel_request(b).unwrap();
        complete_command(&endpoint, 0, CompletionCode::Success);
        // A's Stopped event is the hardware dequeue pointer (A is not cancelled): no Set TR Dequeue Pointer.
        assert!(matches!(
            endpoint.reclaim_request(b),
            Some(Err(TransferError::Cancelled))
        ));
        // The slot is free for the completion A gets once the doorbell restarts it: a slot holds one event, and
        // a second set_finished on a full slot would be dropped.
        assert!(endpoint.ring.peek_finished(trb_a).is_none());
    }

    #[test]
    fn a_td_that_completed_before_the_cancel_returns_its_completion() {
        let (_mmio, mut endpoint) = test_endpoint();
        let id = endpoint.submit_request(bulk_out(64)).unwrap();
        let trb = td_trb(&endpoint, id);
        endpoint
            .ring
            .finished_handle()
            .set_finished(trb, transfer_event(CompletionCode::Success, 0));

        endpoint.cancel_request(id).unwrap();

        assert!(endpoint.cancel_op.is_none(), "no Stop Endpoint for a finished TD");
        assert!(endpoint.reclaim_request(id).unwrap().is_ok());
    }

    /// Doorbell array offset 0 is word 1 of the fake registers (the `mmio.0[1]` of `test_endpoint`), so the doorbell
    /// of slot 1 is that word. A value the controller never writes stands for "not rung".
    const BELL_IDLE: u32 = 0xdead_0000;

    fn bell_word(mmio: &mut AlignedMmio) -> usize {
        mmio.0[1] = BELL_IDLE;
        1
    }

    /// Whether the doorbell was rung since the last call.
    fn bell_rung(mmio: &mut AlignedMmio, word: usize) -> bool {
        core::mem::replace(&mut mmio.0[word], BELL_IDLE) != BELL_IDLE
    }

    #[test]
    fn a_td_submitted_while_the_cancelled_tds_are_taken_off_the_ring_waits_for_the_end_of_the_pass() {
        let (mut mmio, mut endpoint, [_a, b, _c]) = three_tds();
        let bell = bell_word(&mut mmio);
        endpoint.cancel_request(b).unwrap();

        // Cancel, then nobody polls the request (the owner dropped it): the submit goes on the ring, quietly.
        let d = endpoint.submit_request(bulk_out(64)).unwrap();
        assert!(!bell_rung(&mut mmio, bell), "no doorbell while the stop is pending");
        assert_eq!(endpoint.inflight.len(), 4);

        // The next submit drives the pass: the stop completed, B is given back, the doorbell restarts the TDs.
        complete_command(&endpoint, 0, CompletionCode::Success);
        let e = endpoint.submit_request(bulk_out(64)).unwrap();
        assert!(bell_rung(&mut mmio, bell), "the end of the pass rings the doorbell");
        assert_eq!(endpoint.inflight.len(), 4, "B is gone, D and E are in");

        for id in [d, e] {
            let trb = td_trb(&endpoint, id);
            assert_eq!(trb_type(&endpoint, trb), 1, "a new TD is never turned into a No-Op");
            endpoint
                .ring
                .finished_handle()
                .set_finished(trb, transfer_event(CompletionCode::Success, 0));
            assert!(endpoint.reclaim_request(id).unwrap().is_ok());
        }
    }

    #[test]
    fn a_td_submitted_during_the_pass_is_not_stepped_over_and_the_doorbell_comes_once_the_dequeue_pointer_is_set() {
        let (mut mmio, mut endpoint, [_a, b, c]) = three_tds();
        let bell = bell_word(&mut mmio);
        let (trb_b, trb_c) = (td_trb(&endpoint, b), td_trb(&endpoint, c));
        endpoint
            .ring
            .finished_handle()
            .set_finished(trb_b, transfer_event(CompletionCode::Stopped, 64));
        endpoint.cancel_request(b).unwrap();
        let d = endpoint.submit_request(bulk_out(64)).unwrap();
        assert!(!bell_rung(&mut mmio, bell));

        complete_command(&endpoint, 0, CompletionCode::Success);
        assert!(endpoint.reclaim_request(d).is_none());
        assert_eq!(command_type(&endpoint, 1), CMD_SET_TR_DEQUEUE);
        assert_eq!(endpoint.cmd.trb_word(1, 0), (trb_c.raw() as u32) | 1, "the pointer is just past B");
        assert!(!bell_rung(&mut mmio, bell), "not before the Set TR Dequeue Pointer completes");

        complete_command(&endpoint, 1, CompletionCode::Success);
        assert!(endpoint.reclaim_request(d).is_none());
        assert!(bell_rung(&mut mmio, bell));
        assert!(!bell_rung(&mut mmio, bell), "once");
        assert_eq!(trb_type(&endpoint, td_trb(&endpoint, d)), 1);
    }

    #[test]
    fn nothing_is_left_in_the_stopped_completions_once_the_cancelled_requests_are_reclaimed() {
        let (_mmio, mut endpoint, [a, b, c]) = three_tds();
        endpoint.cancel_request(a).unwrap();
        endpoint.cancel_request(c).unwrap();
        complete_command(&endpoint, 0, CompletionCode::Success);
        assert!(matches!(endpoint.reclaim_request(a), Some(Err(TransferError::Cancelled))));
        assert!(endpoint.reclaim_request(c).is_none());
        complete_command(&endpoint, 1, CompletionCode::Success);
        assert!(matches!(endpoint.reclaim_request(c), Some(Err(TransferError::Cancelled))));
        assert!(endpoint.stopped_completions.is_empty());
        assert!(endpoint.cancel_op.is_none() && endpoint.cancelled_tds.is_empty());
        assert!(endpoint.inflight.contains_key(&Endpoint::private_request_id(b)));
    }

    /// An endpoint with its output context, state `state`.
    fn with_out_context(endpoint: &mut Endpoint, state: u8) -> ContextData {
        let ctx = ContextData::new(false, &test_kernel()).unwrap();
        endpoint.set_out_context(ctx.out_context());
        ctx.controller_writes(endpoint.dci.as_u8(), 0, u32::from(state));
        ctx
    }

    #[test]
    fn an_endpoint_a_cancel_pass_left_stopped_is_reconfigured_without_a_stop_command_and_then_runs_again() {
        use crate::osal::tests::run;
        let (mut mmio, mut endpoint, [a, b, c]) = three_tds();
        let bell = bell_word(&mut mmio);
        let ctx = with_out_context(&mut endpoint, EP_STATE_RUNNING);
        let dci = endpoint.dci.as_u8();

        // Every TD cancelled and no one left to ring the doorbell: the pass ends with the endpoint stopped.
        for id in [a, b, c] {
            endpoint.cancel_request(id).unwrap();
        }
        ctx.controller_writes(dci, 0, u32::from(EP_STATE_STOPPED));
        // TDs cancelled while a pass runs join the next one: it stops the stopped endpoint again, which the
        // controller answers with a context error.
        complete_command(&endpoint, 0, CompletionCode::Success);
        for id in [a, b, c] {
            let _ = endpoint.reclaim_request(id);
        }
        complete_command(&endpoint, 1, CompletionCode::ContextStateError);
        for id in [a, b, c] {
            let _ = endpoint.reclaim_request(id);
        }
        assert!(endpoint.inflight.is_empty() && endpoint.stopped_completions.is_empty());
        assert!(!bell_rung(&mut mmio, bell), "nothing is left to start");

        // `Device::stop_endpoints` before a reconfiguration: no Stop Endpoint for a stopped endpoint.
        let queued = endpoint.cmd.trb_word(2, 3);
        let (result, _) = run(endpoint.stop_future(), || {});
        assert!(result.is_ok());
        assert_eq!(endpoint.cmd.trb_word(2, 3), queued, "no command was queued");
        endpoint.retire_all_after_stop();

        // A new stream: the doorbell starts the stopped endpoint, and the TD completes.
        let d = endpoint.submit_request(bulk_out(64)).unwrap();
        assert!(bell_rung(&mut mmio, bell));
        endpoint
            .ring
            .finished_handle()
            .set_finished(td_trb(&endpoint, d), transfer_event(CompletionCode::Success, 0));
        assert!(endpoint.reclaim_request(d).unwrap().is_ok());
    }

    #[test]
    fn a_stop_endpoint_context_error_on_a_stopped_endpoint_is_done() {
        use crate::osal::tests::run;
        let (_mmio, mut endpoint) = test_endpoint();
        let ctx = with_out_context(&mut endpoint, EP_STATE_RUNNING);
        let dci = endpoint.dci.as_u8();
        let mut answered = false;
        let (result, _) = run(endpoint.stop_future(), || {
            if !core::mem::replace(&mut answered, true) {
                // The endpoint stopped on its own before the command was executed.
                ctx.controller_writes(dci, 0, u32::from(EP_STATE_STOPPED));
                complete_command(&endpoint, 0, CompletionCode::ContextStateError);
            }
        });
        assert!(result.is_ok());
        assert_eq!(command_type(&endpoint, 0), CMD_STOP_ENDPOINT);
    }

    #[test]
    fn a_stop_endpoint_on_a_halted_endpoint_resets_it_and_clears_the_halt() {
        use crate::osal::tests::run;
        let (_mmio, mut endpoint) = test_endpoint();
        let ctx = with_out_context(&mut endpoint, EP_STATE_HALTED);
        let dci = endpoint.dci.as_u8();
        endpoint.halted.store(true, Ordering::Release);
        let mut answered = false;
        let (result, _) = run(endpoint.stop_future(), || {
            if !core::mem::replace(&mut answered, true) {
                ctx.controller_writes(dci, 0, u32::from(EP_STATE_STOPPED));
                complete_command(&endpoint, 0, CompletionCode::Success);
            }
        });
        assert!(result.is_ok());
        assert_eq!(command_type(&endpoint, 0), CMD_RESET_ENDPOINT);
        assert!(!endpoint.halted.load(Ordering::Acquire));
    }

    #[test]
    fn a_td_cancelled_during_a_pass_joins_the_next_one() {
        let (_mmio, mut endpoint, [a, b, c]) = three_tds();
        endpoint.cancel_request(a).unwrap();
        endpoint.cancel_request(c).unwrap();
        assert_eq!(endpoint.cancelled_tds, [Endpoint::private_request_id(c)]);

        complete_command(&endpoint, 0, CompletionCode::Success);
        assert!(matches!(
            endpoint.reclaim_request(a),
            Some(Err(TransferError::Cancelled))
        ));
        // The first pass is over; C's pass starts with the next poll.
        assert!(endpoint.reclaim_request(c).is_none());
        assert_eq!(command_type(&endpoint, 1), CMD_STOP_ENDPOINT);
        complete_command(&endpoint, 1, CompletionCode::Success);
        assert!(matches!(
            endpoint.reclaim_request(c),
            Some(Err(TransferError::Cancelled))
        ));
        assert_eq!(endpoint.inflight.len(), 1);
        assert!(endpoint.reclaim_request(b).is_none());
    }

    #[test]
    fn a_stop_endpoint_that_finds_the_endpoint_halted_resets_it_first() {
        let (_mmio, mut endpoint, [a, b, _c]) = three_tds();
        let ctx = ContextData::new(false, &test_kernel()).unwrap();
        endpoint.set_out_context(ctx.out_context());
        let dci = endpoint.dci.as_u8();
        // Halted at A, which stalled and is not cancelled; B is.
        let trb_a = td_trb(&endpoint, a);
        ctx.controller_writes(dci, 0, u32::from(EP_STATE_HALTED));
        ctx.controller_writes(dci, 2, (trb_a.raw() as u32) | 1);
        ctx.controller_writes(dci, 3, (trb_a.raw() >> 32) as u32);
        endpoint.halted.store(true, Ordering::Release);

        endpoint.cancel_request(b).unwrap();
        complete_command(&endpoint, 0, CompletionCode::ContextStateError);
        assert!(endpoint.reclaim_request(b).is_none());
        assert_eq!(command_type(&endpoint, 1), CMD_RESET_ENDPOINT);
        assert_eq!(endpoint.cmd.trb_word(1, 3) & (1 << 9), 0, "no Transfer State Preserve");

        ctx.controller_writes(dci, 0, u32::from(EP_STATE_STOPPED));
        complete_command(&endpoint, 1, CompletionCode::Success);
        // The stalled TD A holds the dequeue pointer: stepped over, so that it is not run again.
        assert!(endpoint.reclaim_request(b).is_none());
        assert_eq!(command_type(&endpoint, 2), CMD_SET_TR_DEQUEUE);
        let past_a = endpoint.ring.bus_addr().raw() + 16;
        assert_eq!(endpoint.cmd.trb_word(2, 0), (past_a as u32) | 1);
        assert!(!endpoint.halted.load(Ordering::Acquire), "the reset cleared the halt");

        complete_command(&endpoint, 2, CompletionCode::Success);
        assert!(matches!(
            endpoint.reclaim_request(b),
            Some(Err(TransferError::Cancelled))
        ));
        assert!(endpoint.inflight.contains_key(&Endpoint::private_request_id(a)), "A is not given back");
    }

    #[test]
    fn a_stop_endpoint_that_keeps_racing_the_endpoint_is_given_up_with_the_buffers_kept() {
        let (_mmio, mut endpoint, [_a, b, _c]) = three_tds();
        let ctx = ContextData::new(false, &test_kernel()).unwrap();
        endpoint.set_out_context(ctx.out_context());
        ctx.controller_writes(endpoint.dci.as_u8(), 0, u32::from(EP_STATE_RUNNING));
        let mappings = ACTIVE_STREAMING_MAPPINGS.with(|count| count.get());

        endpoint.cancel_request(b).unwrap();
        for attempt in 0..usize::from(MAX_STOP_ATTEMPTS) {
            assert!(endpoint.reclaim_request(b).is_none());
            assert_eq!(command_type(&endpoint, attempt), CMD_STOP_ENDPOINT);
            complete_command(&endpoint, attempt, CompletionCode::ContextStateError);
        }
        assert!(matches!(
            endpoint.reclaim_request(b),
            Some(Err(TransferError::Cancelled))
        ));
        assert_eq!(ACTIVE_STREAMING_MAPPINGS.with(|count| count.get()), mappings);
        assert_eq!(endpoint.quarantine_len(), 1);
    }

    #[test]
    fn pending_ring_registration_does_not_replace_active_completion_route() {
        let mut mmio = Box::new(AlignedMmio([0; 4096]));
        mmio.0[1] = 2 | (1 << 24);
        let mmio_base = NonNull::new(mmio.0.as_mut_ptr().cast::<u8>()).unwrap();
        let registers = Arc::new(RwLock::new(XhciRegisters::new(mmio_base)));
        let slot_id = SlotId::from(1);
        let bell = Arc::new(Mutex::new(SlotBell::new(slot_id, registers.read().clone())));
        let kernel = test_kernel();
        let command_ring = CommandRing::new(
            DmaDirection::Bidirectional,
            &kernel,
            registers.clone(),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let active = Endpoint::new(
            slot_id,
            Dci::from(2),
            &kernel,
            bell.clone(),
            command_ring.clone(),
        )
        .unwrap();
        let pending = Endpoint::new(slot_id, Dci::from(2), &kernel, bell, command_ring).unwrap();
        let active_addr = active.ring.bus_addr();
        let handler = TransferResultHandler::new(registers);

        handler.register_queue(1, 2, &active.ring).unwrap();
        assert!(matches!(
            handler.register_queue(1, 2, &pending.ring),
            Err(TransferError::QueueFull)
        ));
        // SAFETY: The test does not run an IRQ handler concurrently with route
        // registration and both endpoint rings outlive the dispatch.
        unsafe {
            handler.set_finished(1, 2, active_addr, TransferEvent::default());
        }

        assert!(active.ring.get_finished(active_addr).is_some());
    }

    #[test]
    fn failed_multi_endpoint_route_replacement_preserves_all_active_routes() {
        let mut mmio = Box::new(AlignedMmio([0; 4096]));
        mmio.0[1] = 2 | (1 << 24);
        let mmio_base = NonNull::new(mmio.0.as_mut_ptr().cast::<u8>()).unwrap();
        let registers = Arc::new(RwLock::new(XhciRegisters::new(mmio_base)));
        let slot_id = SlotId::from(1);
        let bell = Arc::new(Mutex::new(SlotBell::new(slot_id, registers.read().clone())));
        let kernel = test_kernel();
        let command_ring = CommandRing::new(
            DmaDirection::Bidirectional,
            &kernel,
            registers.clone(),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let old = Endpoint::new(
            slot_id,
            Dci::from(2),
            &kernel,
            bell.clone(),
            command_ring.clone(),
        )
        .unwrap();
        let unrelated = Endpoint::new(
            slot_id,
            Dci::from(3),
            &kernel,
            bell.clone(),
            command_ring.clone(),
        )
        .unwrap();
        let pending = Endpoint::new(slot_id, Dci::from(2), &kernel, bell, command_ring).unwrap();
        let old_addr = old.ring.bus_addr();
        let handler = TransferResultHandler::new(registers);
        handler.register_queue(1, 2, &old.ring).unwrap();
        handler.register_queue(1, 3, &unrelated.ring).unwrap();

        assert!(matches!(
            handler.replace_queues(
                1,
                core::iter::once((2, old.ring.finished_handle())),
                core::iter::once((3, pending.ring.finished_handle())),
            ),
            Err(TransferError::QueueFull)
        ));
        // SAFETY: No IRQ handler runs concurrently and all rings outlive this
        // deterministic route transaction test.
        unsafe {
            handler.set_finished(1, 2, old_addr, TransferEvent::default());
        }
        assert!(old.ring.get_finished(old_addr).is_some());
    }
}
