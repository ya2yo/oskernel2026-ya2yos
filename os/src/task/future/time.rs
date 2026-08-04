//! 基于内核单调时间的异步计时器 Future。
//!
//! 本模块使用一个由所有 Hart 共享的 `BTreeMap` 保存尚未到期的定时器。
//! `TimerKey` 先按绝对截止时间排序，再用递增序号区分相同截止时间的
//! 定时器。Future 首次被轮询时注册调用方的 `Waker`，时钟维护路径移除
//! 到期项后在释放计时器锁的前提下逐个唤醒它们。
//!
//! `Timespec` 在这里表示自系统启动以来的单调时间，而不是可以被
//! `clock_settime` 修改的墙上时间。计时器只负责唤醒等待者，具体的任务
//! 阻塞和重新入队由上层 `block_on` 与任务唤醒器完成。

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

/// 定时器在运行时表中的排序键。
///
/// 截止时间相同的定时器通过 `key` 保持严格排序，使它们可以同时存储在
/// `BTreeMap` 中；序号只用于区分条目，不改变截止时间语义。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct TimerKey {
    /// 定时器的绝对单调时间截止点。
    deadline: Timespec,
    /// 由 [`TimerRuntime`] 分配的递增序号。
    key: u64,
}

/// 共享的异步计时器运行时状态。
///
/// `wheel` 保存尚未到期的定时器及其最新 Waker。计时器注册时先放入一个
/// noop Waker，只有 Future 被实际轮询后才会替换为调用方提供的 Waker。
struct TimerRuntime {
    /// 下一个定时器使用的序号。
    key: u64,
    /// 按截止时间和序号排序的待处理定时器。
    wheel: BTreeMap<TimerKey, Waker>,
}

impl TimerRuntime {
    /// 创建一个空的计时器运行时。
    const fn new() -> Self {
        TimerRuntime {
            key: 0,
            wheel: BTreeMap::new(),
        }
    }

    /// 注册一个绝对截止时间，返回对应的键。
    ///
    /// 已经到期的截止时间不会进入表中，并返回 `None`，调用方可以直接
    /// 将对应 Future 视为完成。新条目先绑定 noop Waker，等待首次 `poll`
    /// 时再安装真实 Waker。
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

    /// 轮询一个已注册的定时器并更新其 Waker。
    ///
    /// 条目仍在表中时返回 `Pending`；条目已经被到期处理路径移除时返回
    /// `Ready`。使用每次轮询传入的 Waker，确保 Future 被不同执行器重新
    /// 注册时仍能唤醒正确的任务。
    fn poll(&mut self, key: &TimerKey, cx: &mut Context<'_>) -> Poll<()> {
        if let Some(w) = self.wheel.get_mut(key) {
            *w = cx.waker().clone();
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    }

    /// 取消一个尚未到期的定时器注册。
    ///
    /// `TimerFuture` 被丢弃时调用此方法，避免已经不再等待的 Future 保留
    /// Waker 并在之后被错误唤醒。
    fn cancel(&mut self, key: &TimerKey) {
        self.wheel.remove(key);
    }

    /// 移出所有已经到期的定时器，并保留尚未到期的条目。
    ///
    /// `BTreeMap::split_off` 按 `(deadline, u64::MAX)` 分割，使截止时间
    /// 小于或等于当前时间的条目留在返回值中。返回的 Waker 会在释放全局
    /// 计时器锁之后执行，避免唤醒路径与任务调度锁形成锁嵌套。
    fn take_expired(&mut self) -> BTreeMap<TimerKey, Waker> {
        if self.wheel.is_empty() {
            return BTreeMap::new();
        }

        let now = get_time_spec();

        let pending = self.wheel.split_off(&TimerKey {
            deadline: now,
            key: u64::MAX,
        });

        core::mem::replace(&mut self.wheel, pending)
    }
}

/// 所有 Hart 共用的异步计时器表。
///
/// 单个 Hart 的中断禁止只能防止本地重入，不能串行化其他 Hart 对计时器
/// 的注册、轮询和到期处理，因此仍需要全局自旋锁。
static TIMER_RUNTIME: Mutex<TimerRuntime> = Mutex::new(TimerRuntime::new());

#[allow(dead_code)]
/// 处理当前已经到期的异步计时器。
///
/// 该函数由调度器的定时器维护路径调用：先在计时器运行时中批量摘取
/// 到期项，再在锁外唤醒各个 Waker。这样 Waker 可以安全地获取任务、就绪
/// 队列或分配器相关锁，而不会延长计时器锁的持有时间。
pub(crate) fn check_timer_events() {
    // Wakers can acquire task, run-queue, and allocator locks. Keep the timer
    // wheel lock limited to selecting expired entries, then wake outside it.
    let expired = with_current(|runtime| runtime.take_expired());
    for (_, waker) in expired {
        waker.wake();
    }
}

/// 在受保护的共享计时器运行时上执行一次操作。
///
/// `NoPreemptIrqSave` 防止当前 Hart 在持有计时器锁时被本地定时器中断
/// 重入；`TIMER_RUNTIME` 的 Mutex 则负责 Hart 之间的互斥访问。
fn with_current<R>(f: impl FnOnce(&mut TimerRuntime) -> R) -> R {
    let _g = kernel_guard::NoPreemptIrqSave::new();
    f(&mut TIMER_RUNTIME.lock())
}

/// `sleep_until` 返回的异步计时器 Future。
///
/// Future 内部只保存计时器键；实际截止时间和 Waker 存放在共享的
/// [`TimerRuntime`] 中。Future 被轮询到期后完成，提前丢弃则取消对应注册。
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct TimerFuture(TimerKey);

impl Future for TimerFuture {
    type Output = ();

    /// 注册当前执行上下文的 Waker，或在计时器已被移出时返回完成。
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        with_current(|r| r.poll(&self.0, cx))
    }
}

impl Drop for TimerFuture {
    /// Future 未完成就被丢弃时，从共享计时器表中移除注册。
    fn drop(&mut self) {
        with_current(|r| r.cancel(&self.0));
    }
}

/// 等待直到给定的单调时间 `deadline`。
///
/// 截止时间已经到达时立即返回；否则注册一个计时器并异步等待对应的
/// Waker。这里的 `deadline` 使用 `get_time_spec()` 同一套开机后单调时间，
/// 不受墙上时间调整影响。
pub async fn sleep_until(deadline: Timespec) {
    let key = with_current(|r| r.add(deadline));
    if let Some(key) = key {
        TimerFuture(key).await;
    }
}

/// `timeout` 或 `timeout_at` 在等待超时时返回的错误。
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

/// 要求 Future 在指定的相对时长内完成。
///
/// `duration == None` 表示不设置超时，此时直接等待 Future 完成。超时截止
/// 点基于当前的单调开机时间计算，Future 先完成时返回其结果，否则返回
/// [`Elapsed`]。
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

/// 要求 Future 在指定的绝对单调截止时间前完成。
///
/// `deadline` 使用 `Timespec` 对应的 `Duration` 表示开机后的绝对时间；传入
/// `None` 表示不设置超时。Future 先完成时返回其结果，计时器先到期时返回
/// [`Elapsed`]。
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
