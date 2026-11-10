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
    DirectionExt, SlotId, cmd::CommandRing, reg::SlotBell, ring::SendRing, transfer::TransferId,
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
}

struct SoftReset {
    request_id: EndpointRequestId,
    completion_trb: TransferId,
    event: TransferEvent,
    future: EndpointResetFuture,
}

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
        })
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
        if self.outstanding_trbs.saturating_add(required) > usable {
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

    pub(crate) fn stop_future(&self) -> EndpointResetFuture {
        let slot_id = self.slot_id.as_u8();
        let endpoint_id = self.dci.as_u8();
        let mut command_ring = self.cmd.clone();
        Box::pin(async move {
            let stop_endpoint = *command::StopEndpoint::default()
                .set_endpoint_id(endpoint_id)
                .set_slot_id(slot_id);
            command_ring
                .cmd_request(command::Allowed::StopEndpoint(stop_endpoint))
                .await?;
            Ok(())
        })
    }

    pub(crate) fn retire_all_after_stop(&mut self) {
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

            let mut doorbell = doorbell::Register::default();
            doorbell.set_doorbell_target(endpoint_id);
            // SAFETY: Configure rollback keeps the endpoint unpublished while
            // the task path restores its dequeue pointer and rings the bell.
            unsafe { bell.lock_raw() }.ring(doorbell);
            Ok(())
        })
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
        self.doorbell();

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
        let target = match &self.inflight.get(&request_id)?.kind {
            SubmittedTdKind::Normal { completion_trb } => ReclaimTarget::Normal(*completion_trb),
            SubmittedTdKind::Control(control_td) => ReclaimTarget::Control(*control_td),
            SubmittedTdKind::Iso { .. } => ReclaimTarget::Iso,
        };
        match target {
            ReclaimTarget::Normal(completion_trb) => {
                let event = self.ring.get_finished(completion_trb.0)?;
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
            .get_mut(&request_id)
            .ok_or(TransferError::InvalidEndpoint)?;
        submitted.cancelled = true;
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
