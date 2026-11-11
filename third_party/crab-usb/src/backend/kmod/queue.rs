use alloc::{collections::BTreeMap, sync::Arc};
use core::{
    cell::UnsafeCell,
    hint::spin_loop,
    pin::Pin,
    sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
    task::{Context, Poll},
};

use futures::task::AtomicWaker;

use crate::BusAddr;

static FINISHED_QUEUE_LOG_BUDGET: AtomicUsize = AtomicUsize::new(128);

fn take_queue_log_budget() -> bool {
    FINISHED_QUEUE_LOG_BUDGET
        .try_update(Ordering::AcqRel, Ordering::Acquire, |left| {
            left.checked_sub(1)
        })
        .is_ok()
}

pub struct Finished<C> {
    inner: Arc<FinishedInner<C>>,
}

impl<C> Clone for Finished<C> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

pub struct FinishedInner<C> {
    data: BTreeMap<BusAddr, Arc<FinishedData<C>>>,
}

const SLOT_EMPTY: u8 = 0;
const SLOT_WRITING: u8 = 1;
const SLOT_READY: u8 = 2;
const SLOT_READING: u8 = 3;

pub struct FinishedData<C> {
    taken: AtomicBool,
    state: AtomicU8,
    waker: AtomicWaker,
    data: UnsafeCell<Option<C>>,
}

impl<C> FinishedData<C> {
    fn new() -> Self {
        Self {
            state: AtomicU8::new(SLOT_EMPTY),
            taken: AtomicBool::new(false),
            waker: AtomicWaker::new(),
            data: UnsafeCell::new(None),
        }
    }
}

unsafe impl<C: Send> Send for FinishedData<C> {}
unsafe impl<C: Send> Sync for FinishedData<C> {}

unsafe impl<C: Send> Send for FinishedInner<C> {}
unsafe impl<C: Send> Sync for FinishedInner<C> {}

impl<C> FinishedInner<C> {
    fn clear_finished(&self, addr: BusAddr) {
        if let Some(data) = self.data.get(&addr) {
            data.clear();
        }
    }
}

impl<C> Finished<C> {
    pub fn new(addrs: impl Iterator<Item = BusAddr>) -> Self {
        let mut data = BTreeMap::new();

        for addr in addrs {
            data.insert(addr, Arc::new(FinishedData::new()));
        }
        Self {
            inner: Arc::new(FinishedInner { data }),
        }
    }

    pub fn clear_finished(&self, addr: BusAddr) {
        self.inner.clear_finished(addr);
    }

    /// The first and the last address of the queue.
    pub(crate) fn bounds(&self) -> Option<(BusAddr, BusAddr)> {
        let first = *self.inner.data.keys().next()?;
        let last = *self.inner.data.keys().next_back()?;
        Some((first, last))
    }

    pub(crate) fn contains(&self, addr: BusAddr) -> bool {
        self.inner.data.contains_key(&addr)
    }

    pub(crate) fn same_queue(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    pub fn set_finished(&self, addr: BusAddr, value: C) {
        if !self.try_set_finished(addr, value) && take_queue_log_budget() {
            warn!(
                "usb queue: completion address {:#x} is not registered",
                addr.raw()
            );
        }
    }

    pub fn try_set_finished(&self, addr: BusAddr, value: C) -> bool {
        if let Some(slot) = self.inner.data.get(&addr) {
            slot.set_finished(value);
            true
        } else {
            false
        }
    }

    pub fn get_finished(&self, addr: BusAddr) -> Option<C> {
        self.waiter(addr).get_finished()
    }

    /// The completion stored at `addr`, if any, left where it is.
    pub fn peek_finished(&self, addr: BusAddr) -> Option<C>
    where
        C: Copy,
    {
        self.waiter(addr).peek_finished()
    }

    /// Takes the completion stored at `addr` only if `take` accepts it; returns what was taken.
    pub fn take_finished_if(&self, addr: BusAddr, take: impl FnOnce(&C) -> bool) -> Option<C> {
        self.waiter(addr).take_finished_if(take)
    }

    /// Takes a completion stored at `addr`, if any, and `None` for an address that is not on the queue or whose
    /// waiter is taken by someone else (unlike `get_finished`, never panics).
    pub fn try_get_finished(&self, addr: BusAddr) -> Option<C> {
        let slot = self.inner.data.get(&addr)?;
        if slot.taken.load(Ordering::Acquire) {
            return None;
        }
        slot.get_finished()
    }

    fn waiter(&self, addr: BusAddr) -> &FinishedData<C> {
        let slot = self.inner.data.get(&addr).unwrap();
        if slot.taken.load(Ordering::Acquire) {
            panic!("waiter called after take_waiter");
        }

        slot
    }

    pub fn register_cx(&self, addr: BusAddr, cx: &mut core::task::Context<'_>) {
        self.waiter(addr).register(cx.waker());
    }

    pub fn take_waiter(&self, addr: BusAddr) -> TWaiter<C> {
        let data = self.inner.data.get(&addr).unwrap();
        if data.taken.swap(true, Ordering::AcqRel) {
            panic!("take_waiter called multiple times for the same addr");
        }
        TWaiter {
            finished: data.clone(),
        }
    }
}

pub struct TWaiter<C> {
    finished: Arc<FinishedData<C>>,
}

impl<C> TWaiter<C> {
    /// Takes the completion if it has arrived, without a task context.
    pub fn try_take(&self) -> Option<C> {
        self.finished.get_finished()
    }
}

// LucinaOS: the slot can be waited on again once this waiter is gone. A ring reuses its slots after one lap, and
// without this the command ring's 512th command panicked in `take_waiter`.
impl<C> Drop for TWaiter<C> {
    fn drop(&mut self) {
        self.finished.taken.store(false, Ordering::Release);
    }
}

impl<C> Future for TWaiter<C> {
    type Output = C;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        if let Some(res) = this.finished.get_finished() {
            return Poll::Ready(res);
        }
        this.finished.register(cx.waker());
        match this.finished.get_finished() {
            Some(res) => Poll::Ready(res),
            None => Poll::Pending,
        }
    }
}

impl<C> FinishedData<C> {
    pub fn register(&self, waker: &core::task::Waker) {
        self.waker.register(waker);
    }

    fn clear(&self) {
        loop {
            match self.state.load(Ordering::Acquire) {
                SLOT_EMPTY => return,
                SLOT_READY => {
                    if self
                        .state
                        .compare_exchange(
                            SLOT_READY,
                            SLOT_READING,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        unsafe {
                            (*self.data.get()).take();
                        }
                        self.state.store(SLOT_EMPTY, Ordering::Release);
                        return;
                    }
                }
                SLOT_WRITING | SLOT_READING => spin_loop(),
                _ => {
                    self.state.store(SLOT_EMPTY, Ordering::Release);
                    return;
                }
            }
        }
    }

    pub fn set_finished(&self, value: C) {
        if self
            .state
            .compare_exchange(
                SLOT_EMPTY,
                SLOT_WRITING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            if take_queue_log_budget() {
                warn!("usb queue: dropping duplicate completion for busy slot");
            }
            return;
        }

        unsafe {
            *self.data.get() = Some(value);
        }
        self.state.store(SLOT_READY, Ordering::Release);
        self.waker.wake();
    }

    /// Runs `f` on the stored completion, if one is ready; the slot is empty afterwards if `f` took it.
    fn with_ready<R>(&self, f: impl FnOnce(&mut Option<C>) -> R) -> Option<R> {
        self.state
            .compare_exchange(
                SLOT_READY,
                SLOT_READING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .ok()?;
        // SAFETY: SLOT_READING is held by this thread only, as in `get_finished`.
        let slot = unsafe { &mut *self.data.get() };
        let result = f(slot);
        let state = if slot.is_some() { SLOT_READY } else { SLOT_EMPTY };
        self.state.store(state, Ordering::Release);
        Some(result)
    }

    fn take_finished_if(&self, take: impl FnOnce(&C) -> bool) -> Option<C> {
        self.with_ready(|slot| {
            if slot.as_ref().is_some_and(take) {
                slot.take()
            } else {
                None
            }
        })
        .flatten()
    }

    fn peek_finished(&self) -> Option<C>
    where
        C: Copy,
    {
        self.with_ready(|slot| *slot).flatten()
    }

    pub fn get_finished(&self) -> Option<C> {
        if self
            .state
            .compare_exchange(
                SLOT_READY,
                SLOT_READING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return None;
        }
        let value = unsafe { (*self.data.get()).take() };
        self.state.store(SLOT_EMPTY, Ordering::Release);
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slot_can_be_waited_on_again_after_its_waiter_is_dropped() {
        let addr = BusAddr::from(0x1000u64);
        let finished = Finished::<u32>::new([addr].into_iter());
        for lap in 0..3 {
            finished.clear_finished(addr);
            let waiter = finished.take_waiter(addr);
            assert_eq!(waiter.try_take(), None);
            finished.set_finished(addr, lap);
            assert_eq!(waiter.try_take(), Some(lap));
        }
    }
}
