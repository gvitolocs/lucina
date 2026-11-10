use core::{ops::Deref, time::Duration};

use dma_api::{DeviceDma, DmaConstraints};

#[derive(Clone)]
pub(crate) struct Kernel {
    dma: DeviceDma,
    osal: &'static dyn KernelOp,
}

impl Kernel {
    pub fn new(dma: DeviceDma, osal: &'static dyn KernelOp) -> Self {
        Self { dma, osal }
    }

    pub fn delay(&self, duration: Duration) {
        self.osal.delay(duration)
    }
}

impl Deref for Kernel {
    type Target = DeviceDma;

    fn deref(&self) -> &Self::Target {
        &self.dma
    }
}

pub(crate) fn narrow_dma_capability(dma: &DeviceDma, hardware_mask: u64) -> DeviceDma {
    let current = dma.info().constraints();
    dma.with_constraints(DmaConstraints {
        addr_mask: current.addr_mask.min(hardware_mask),
        ..current
    })
}

pub trait KernelOp: Send + Sync {
    fn delay(&self, duration: Duration);
}

/// The delay source of the bounded waits. `Kernel` sleeps with the OS delay; the tests use a fake clock.
pub(crate) trait Sleeper {
    fn sleep(&self, duration: Duration);
}

impl Sleeper for Kernel {
    fn sleep(&self, duration: Duration) {
        self.delay(duration)
    }
}

/// A bounded wait reached its limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WaitTimeout;

/// Returns `Pending` once, then `Ready`: lets a polling executor run (the kernel's `block_on` drains the event
/// rings between two polls) without sleeping.
#[derive(Default)]
struct YieldOnce(bool);

impl core::future::Future for YieldOnce {
    type Output = ();

    fn poll(
        mut self: core::pin::Pin<&mut Self>,
        cx: &mut core::task::Context<'_>,
    ) -> core::task::Poll<()> {
        if self.0 {
            return core::task::Poll::Ready(());
        }
        self.0 = true;
        cx.waker().wake_by_ref();
        core::task::Poll::Pending
    }
}

/// Waits until `done` holds, or fails once `limit` has been slept, `step` at a time. Every round tests `done`,
/// yields once to the executor, tests again and only then sleeps, so a condition that the next event drain makes
/// true costs no sleep. The limit counts the sleeps only, so the wait in real time is at least `limit`, as
/// `readl_poll_timeout` in Linux.
pub(crate) async fn wait_until<S: Sleeper + ?Sized>(
    sleeper: &S,
    step: Duration,
    limit: Duration,
    mut done: impl FnMut() -> bool,
) -> Result<(), WaitTimeout> {
    let mut slept = Duration::ZERO;
    loop {
        if done() {
            return Ok(());
        }
        if slept >= limit {
            return Err(WaitTimeout);
        }
        YieldOnce::default().await;
        if done() {
            return Ok(());
        }
        sleeper.sleep(step);
        slept += step;
    }
}

/// Why a register handshake failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HandshakeError {
    /// The bits did not reach their value within the limit.
    Timeout,
    /// The register reads all ones: the controller is gone (Linux `xhci_handshake` returns -ENODEV).
    Gone,
}

/// Linux's `xhci_handshake`: waits until `read() & mask == value`, at most `limit`. A read of all ones ends the
/// wait at once as `Gone`.
pub(crate) async fn handshake<S: Sleeper + ?Sized>(
    sleeper: &S,
    step: Duration,
    limit: Duration,
    mut read: impl FnMut() -> u32,
    mask: u32,
    value: u32,
) -> Result<(), HandshakeError> {
    let mut last = 0;
    let waited = wait_until(sleeper, step, limit, || {
        last = read();
        last & mask == value || last == u32::MAX
    })
    .await;
    if last == u32::MAX {
        return Err(HandshakeError::Gone);
    }
    waited.map_err(|_| HandshakeError::Timeout)
}

pub(crate) struct SpinWhile<F>
where
    F: Fn() -> bool,
{
    pub condition: F,
}

impl<F> SpinWhile<F>
where
    F: Fn() -> bool,
{
    #[must_use]
    pub fn new(condition: F) -> Self {
        Self { condition }
    }
}

impl<F> core::future::Future for SpinWhile<F>
where
    F: Fn() -> bool,
{
    type Output = ();

    fn poll(
        self: core::pin::Pin<&mut Self>,
        cx: &mut core::task::Context<'_>,
    ) -> core::task::Poll<Self::Output> {
        if (self.condition)() {
            cx.waker().wake_by_ref();
            core::task::Poll::Pending
        } else {
            core::task::Poll::Ready(())
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    extern crate std;

    use core::{
        cell::Cell,
        future::Future,
        pin::pin,
        task::{Context, Poll, Waker},
        time::Duration,
    };

    use super::*;

    /// A fake clock: `sleep` only advances `now`.
    #[derive(Default)]
    pub(crate) struct FakeClock {
        pub now: Cell<Duration>,
    }

    impl Sleeper for FakeClock {
        fn sleep(&self, duration: Duration) {
            self.now.set(self.now.get() + duration);
        }
    }

    /// Polls `future` to completion with a no-op waker, running `between` after every `Pending` (the kernel's
    /// `block_on` drains events there). Returns the output and the number of `Pending` polls.
    pub(crate) fn run<F: Future>(future: F, mut between: impl FnMut()) -> (F::Output, usize) {
        let mut future = pin!(future);
        let mut cx = Context::from_waker(Waker::noop());
        let mut pending = 0;
        loop {
            if let Poll::Ready(output) = future.as_mut().poll(&mut cx) {
                return (output, pending);
            }
            pending += 1;
            assert!(pending < 1_000_000, "a bounded wait never ended");
            between();
        }
    }

    #[test]
    fn wait_until_returns_at_once_without_sleeping_when_the_condition_holds() {
        let clock = FakeClock::default();
        let (result, pending) = run(
            wait_until(
                &clock,
                Duration::from_millis(1),
                Duration::from_secs(5),
                || true,
            ),
            || {},
        );
        assert_eq!(result, Ok(()));
        assert_eq!(pending, 0);
        assert_eq!(clock.now.get(), Duration::ZERO);
    }

    #[test]
    fn wait_until_sees_what_the_executor_did_between_polls_without_sleeping() {
        // A command completion arrives with the first event drain: no sleep at all.
        let clock = FakeClock::default();
        let drained = Cell::new(false);
        let (result, _) = run(
            wait_until(
                &clock,
                Duration::from_millis(1),
                Duration::from_secs(5),
                || drained.get(),
            ),
            || drained.set(true),
        );
        assert_eq!(result, Ok(()));
        assert_eq!(clock.now.get(), Duration::ZERO);
    }

    #[test]
    fn wait_until_times_out_after_the_limit_with_a_fake_clock() {
        let clock = FakeClock::default();
        let (result, _) = run(
            wait_until(
                &clock,
                Duration::from_millis(10),
                Duration::from_millis(32),
                || false,
            ),
            || {},
        );
        assert_eq!(result, Err(WaitTimeout));
        // Sleeps at 0, 10, 20, 30 ms: the limit is reached at 40 ms, never earlier than 32 ms.
        assert_eq!(clock.now.get(), Duration::from_millis(40));
    }

    #[test]
    fn handshake_waits_for_the_masked_value() {
        let clock = FakeClock::default();
        let (result, _) = run(
            handshake(
                &clock,
                Duration::from_millis(1),
                Duration::from_millis(32),
                || {
                    if clock.now.get() >= Duration::from_millis(3) {
                        0x1
                    } else {
                        0x0
                    }
                },
                0x1,
                0x1,
            ),
            || {},
        );
        assert_eq!(result, Ok(()));
        assert_eq!(clock.now.get(), Duration::from_millis(3));
    }

    #[test]
    fn handshake_stops_at_once_on_a_register_reading_all_ones() {
        let clock = FakeClock::default();
        let (result, _) = run(
            handshake(
                &clock,
                Duration::from_millis(1),
                Duration::from_secs(10),
                || u32::MAX,
                1 << 11,
                0,
            ),
            || {},
        );
        assert_eq!(result, Err(HandshakeError::Gone));
        assert_eq!(clock.now.get(), Duration::ZERO);
    }

    #[test]
    fn handshake_times_out() {
        let clock = FakeClock::default();
        let (result, _) = run(
            handshake(
                &clock,
                Duration::from_millis(1),
                Duration::from_millis(32),
                || 0,
                1,
                1,
            ),
            || {},
        );
        assert_eq!(result, Err(HandshakeError::Timeout));
        assert!(clock.now.get() >= Duration::from_millis(32));
    }
}
