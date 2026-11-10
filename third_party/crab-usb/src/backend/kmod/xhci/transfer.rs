use alloc::{
    collections::{BTreeMap, btree_map::Entry},
    sync::Arc,
};

use core::sync::atomic::{AtomicUsize, Ordering};

use xhci::ring::trb::event::{CompletionCode, TransferEvent};

use super::{reg::XhciRegistersShared, ring::SendRing, sync::IrqLock};
use crate::{BusAddr, queue::Finished, usb_if::err::TransferError};

/// How many events off their ring are logged; a controller gone wrong must not fill the screen.
static OFF_RING_LOG_BUDGET: AtomicUsize = AtomicUsize::new(16);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TransferId(pub(crate) BusAddr);

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy)]
pub struct TransQueueId {
    slot_id: u8,
    ep_id: u8,
}

#[derive(Clone)]
pub struct TransferResultHandler {
    inner: Arc<IrqLock<BTreeMap<TransQueueId, Finished<TransferEvent>>>>,
}

impl TransferResultHandler {
    pub fn new(reg: XhciRegistersShared) -> Self {
        Self {
            inner: Arc::new(IrqLock::new(BTreeMap::new(), reg)),
        }
    }

    pub fn register_queue(
        &self,
        slot_id: u8,
        ep_id: u8,
        ring: &SendRing<TransferEvent>,
    ) -> Result<(), TransferError> {
        let id = TransQueueId { slot_id, ep_id };
        let handle = ring.finished_handle();
        match self.inner.lock().entry(id) {
            Entry::Vacant(entry) => {
                entry.insert(handle);
                Ok(())
            }
            Entry::Occupied(_) => Err(TransferError::QueueFull),
        }
    }

    /// Atomically replaces all completion routes participating in one endpoint
    /// configuration transaction.
    pub fn replace_queues(
        &self,
        slot_id: u8,
        old: impl Iterator<Item = (u8, Finished<TransferEvent>)>,
        new: impl Iterator<Item = (u8, Finished<TransferEvent>)>,
    ) -> Result<(), TransferError> {
        let old = old
            .map(|(ep_id, queue)| (TransQueueId { slot_id, ep_id }, queue))
            .collect::<BTreeMap<_, _>>();
        let new = new
            .map(|(ep_id, queue)| (TransQueueId { slot_id, ep_id }, queue))
            .collect::<BTreeMap<_, _>>();
        let mut queues = self.inner.lock();

        for (id, expected) in &old {
            if !queues
                .get(id)
                .is_some_and(|active| active.same_queue(expected))
            {
                return Err(TransferError::InvalidEndpoint);
            }
        }
        for (id, pending) in &new {
            if queues
                .get(id)
                .is_some_and(|active| !old.contains_key(id) && !active.same_queue(pending))
            {
                return Err(TransferError::QueueFull);
            }
        }

        for id in old.keys() {
            queues.remove(id);
        }
        queues.extend(new);
        Ok(())
    }

    /// Marks a queue completion from the xHCI interrupt path.
    ///
    /// This runs while handling an interrupt, so it must not acquire OS-facing
    /// locks or call into device/file managers. Queue registration is protected
    /// by `IrqLock::lock`, which disables this interrupt source before mutating
    /// the map. The IRQ hot path uses `force_use` and only touches the
    /// pre-registered queue completion slot, then wakes queue-local waiters.
    pub unsafe fn set_finished(
        &self,
        slot_id: u8,
        ep_id: u8,
        ptr: BusAddr,
        res: TransferEvent,
    ) -> bool {
        // xHCI reports ISO ring underrun/overrun when the periodic ring is
        // empty. Linux treats these as ring xrun events, not TD completions.
        if is_iso_ring_xrun(res) {
            trace!(
                "xhci: ignore ISO ring xrun event slot={} ep={} ptr={:#x} code={:?}",
                slot_id,
                ep_id,
                ptr.raw(),
                res.completion_code()
            );
            return true;
        }

        let queue_id = TransQueueId { slot_id, ep_id };
        if let Some(q) = unsafe { self.inner.force_use().get(&queue_id) } {
            trace!(
                "xhci: dispatch transfer event slot={} ep={} ptr={:#x} code={:?} len={}",
                slot_id,
                ep_id,
                ptr.raw(),
                res.completion_code(),
                res.trb_transfer_length()
            );
            if q.try_set_finished(ptr, res) {
                true
            } else {
                self.log_event_off_ring(slot_id, ep_id, ptr, res, q);
                false
            }
        } else {
            error!(
                "xhci: controller fault: transfer event has no active endpoint route slot={} \
                 ep={} ptr={:#x} code={:?} len={}",
                slot_id,
                ep_id,
                ptr.raw(),
                res.completion_code(),
                res.trb_transfer_length()
            );
            false
        }
    }
}

impl TransferResultHandler {
    /// Says what is known about an event whose pointer is not on its endpoint's ring, in lines that fit a 100-column
    /// screen under the log prefix. Which ring the pointer is on tells a stale read of the event ring (another
    /// endpoint's) from a controller that went off the ring (none). The transfer it finished stays open; the class
    /// drivers' pipes get past it when a later transfer on the endpoint finishes.
    fn log_event_off_ring(
        &self,
        slot_id: u8,
        ep_id: u8,
        ptr: BusAddr,
        res: TransferEvent,
        queue: &Finished<TransferEvent>,
    ) {
        if OFF_RING_LOG_BUDGET
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |left| {
                left.checked_sub(1)
            })
            .is_err()
        {
            return;
        }
        error!(
            "xhci: off ring ptr={:#x} slot={} ep={} {:?} len={}",
            ptr.raw(),
            slot_id,
            ep_id,
            res.completion_code(),
            res.trb_transfer_length()
        );
        let (first, last) = queue
            .bounds()
            .unwrap_or((BusAddr::from(0), BusAddr::from(0)));
        // SAFETY: as in `set_finished`, this runs on the event path.
        let owner = unsafe { self.inner.force_use() }
            .iter()
            .find(|(_, queue)| queue.contains(ptr))
            .map(|(id, _)| (id.slot_id, id.ep_id));
        error!(
            "xhci: its ring {:#x}..={:#x}; ptr on slot/ep {:?}",
            first.raw(),
            last.raw(),
            owner
        );
    }
}

fn is_iso_ring_xrun(event: TransferEvent) -> bool {
    matches!(
        event.completion_code(),
        Ok(CompletionCode::RingUnderrun | CompletionCode::RingOverrun)
    )
}
