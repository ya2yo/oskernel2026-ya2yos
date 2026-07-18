use crate::timer::{get_time_spec, Timespec};
use crate::utils::SysErrNo;
use alloc::collections::BTreeMap;
use core::{
    fmt,
    future::{Future, IntoFuture},
    pin::Pin,
    task::{Context, Poll, Waker},
    time::Duration,
};
use futures_util::{select_biased, FutureExt};
use spin::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct TimerKey {
    deadline: Timespec,
    key: u64,
}

struct TimerRuntime {
    key: u64,
    wheel: BTreeMap<TimerKey, Waker>,
}

impl TimerRuntime {
    const fn new() -> Self {
        TimerRuntime {
            key: 0,
            wheel: BTreeMap::new(),
        }
    }

    fn add(&mut self, deadline: Timespec) -> Option<TimerKey> {
        if deadline <= get_time_spec() {
            return None;
        }

        let key = TimerKey {
            deadline,
            key: self.key,
        };
        self.wheel.insert(key, Waker::noop().clone());
        self.key += 1;

        Some(key)
    }

    fn poll(&mut self, key: &TimerKey, cx: &mut Context<'_>) -> Poll<()> {
        if let Some(w) = self.wheel.get_mut(key) {
            *w = cx.waker().clone();
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    }

    fn cancel(&mut self, key: &TimerKey) {
        self.wheel.remove(key);
    }

    fn wake(&mut self) {
        if self.wheel.is_empty() {
            return;
        }

        let now = get_time_spec();

        let pending = self.wheel.split_off(&TimerKey {
            deadline: now,
            key: u64::MAX,
        });

        let expired = core::mem::replace(&mut self.wheel, pending);
        for (_, w) in expired {
            w.wake();
        }
    }
}

/// All harts drive the same timer wheel. Local interrupt exclusion alone is
/// insufficient once more than one hart can poll or update it.
static TIMER_RUNTIME: Mutex<TimerRuntime> = Mutex::new(TimerRuntime::new());

#[allow(dead_code)]
pub(crate) fn check_timer_events() {
    with_current(|runtime| runtime.wake());
}

fn with_current<R>(f: impl FnOnce(&mut TimerRuntime) -> R) -> R {
    // The guard prevents local timer interrupt reentrancy; the mutex serializes
    // accesses from different harts.
    let _g = kernel_guard::NoPreemptIrqSave::new();
    f(&mut TIMER_RUNTIME.lock())
}

/// Future returned by `sleep` and `sleep_until`.
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct TimerFuture(TimerKey);

impl Future for TimerFuture {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        with_current(|r| r.poll(&self.0, cx))
    }
}

impl Drop for TimerFuture {
    fn drop(&mut self) {
        with_current(|r| r.cancel(&self.0));
    }
}

/// Waits until `deadline` is reached.
pub async fn sleep_until(deadline: Timespec) {
    let key = with_current(|r| r.add(deadline));
    if let Some(key) = key {
        TimerFuture(key).await;
    }
}

/// Error returned by [`timeout`] and [`timeout_at`].
#[derive(Debug, PartialEq, Eq)]
pub struct Elapsed(());

impl fmt::Display for Elapsed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "deadline elapsed")
    }
}

impl core::error::Error for Elapsed {}

impl From<Elapsed> for SysErrNo {
    fn from(_: Elapsed) -> Self {
        SysErrNo::ETIMEDOUT
    }
}

/// Requires a `Future` to complete before the specified duration has elapsed.
pub async fn timeout<F: IntoFuture>(
    duration: Option<Duration>,
    f: F,
) -> Result<F::Output, Elapsed> {
    timeout_at(
        duration.and_then(|x| x.checked_add(get_time_spec().into())),
        f,
    )
    .await
}

/// Requires a `Future` to complete before the specified deadline.
pub async fn timeout_at<F: IntoFuture>(
    deadline: Option<Duration>,
    f: F,
) -> Result<F::Output, Elapsed> {
    if let Some(deadline) = deadline {
        select_biased! {
            res = f.into_future().fuse() => Ok(res),
            _ = sleep_until(deadline.into()).fuse() => Err(Elapsed(())),
        }
    } else {
        Ok(f.await)
    }
}
