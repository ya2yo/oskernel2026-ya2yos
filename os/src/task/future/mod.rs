//! 异步 Future 支持模块
//! 提供了在内核空间执行 Future 的基础架构，包括 Waker 实现和 block_on 执行器
use alloc::{
    sync::Arc,
    task::{self, Wake},
};
use core::{
    fmt,
    future::poll_fn,
    pin::pin,
    task::{Context, Poll, Waker},
};
use log::debug;

use super::{TaskRef, WeakTaskRef};
use crate::{
    signal::SigSet,
    task::{
        block_current_and_run_next, current_task, exit_current_and_run_next, ready_queue, schedule,
        TaskContext, TaskStatus,
    },
    utils::SysErrNo,
};
use kernel_guard::NoPreemptIrqSave;
use kspin::SpinNoIrq;

mod poll;
pub use poll::*;

mod time;
pub use time::*;

/// 内核任务唤醒器
/// 关联了一个具体的内核任务，当 Future 就绪时，通过它唤醒对应的任务。
struct MyWaker {
    /// 目标任务的弱引用，防止循环引用导致任务无法释放
    task: WeakTaskRef,
    /// 唤醒状态标志，使用带自旋锁的 bool 保证多核安全
    woke: SpinNoIrq<bool>,
}

impl MyWaker {
    /// 为指定的任务创建一个新的 Waker
    fn new(task: &TaskRef) -> Arc<Self> {
        Arc::new(MyWaker {
            task: Arc::downgrade(task),
            woke: SpinNoIrq::new(false),
        })
    }
}

impl Wake for MyWaker {
    /// 消耗型唤醒,转移所有权
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    /// 引用型唤醒，不转移所有权
    /// 这是唤醒逻辑的核心：将任务从阻塞态移回就绪态。
    fn wake_by_ref(self: &Arc<Self>) {
        // 尝试将弱引用升级为强引用
        if let Some(task) = self.task.upgrade() {
            // 标记已唤醒
            *self.woke.lock() = true;
            // 调用调度器接口，取消任务的阻塞状态
            let mut inner = task.inner_lock();
            if inner.task_status != TaskStatus::Ready {
                inner.task_status = TaskStatus::Ready;
                drop(inner);
                ready_queue::add_task(&task);
            }
        }
    }
}

/// 阻塞当前任务直到给定的 Future 执行完成。
///
/// 这是内核中的“同步转异步”桥梁。它会不断轮询 Future，
/// 如果 Future 返回 Pending，则会将当前任务挂起（休眠）。
///
/// 注意：此函数不处理中断，通常不建议在需要响应信号的用户态任务中直接使用。
pub fn block_on<F: core::future::Future>(f: F) -> F::Output {
    let mut fut = pin!(f);
    let waker_inner = MyWaker::new(&current_task().unwrap());
    let waker = Waker::from(waker_inner.clone());
    let mut cx = Context::from_waker(&waker);

    loop {
        // 每轮单独作用域：检查完信号就释放 clone，避免带着 Arc 调用 block_current
        {
            let task = current_task().unwrap();
            // debug!("[block_on] strong count: {}", Arc::strong_count(&task));
            if task.inner_lock().sig_pending.contains(SigSet::SIGKILL) {
                drop(task);
                exit_current_and_run_next(137);
                unreachable!();
            }
        }

        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(output) => return output,
            Poll::Pending => {
                let mut is_woke = waker_inner.woke.lock();
                if !*is_woke {
                    drop(is_woke);
                    let task = current_task().unwrap();
                    // debug!(
                    //     "[block_on] Pending strong_count = {}",
                    //     Arc::strong_count(&task)
                    // ); // 这里怎么比上面多一个
                    drop(task);
                    block_current_and_run_next();
                } else {
                    *is_woke = false;
                    drop(is_woke);
                    let cur = current_task().unwrap();
                    let task_cx_ptr = {
                        let mut inner = cur.inner_lock();
                        &mut inner.task_cx as *mut TaskContext
                    };
                    // debug!("strong_count = {}", Arc::strong_count(&cur));
                    drop(cur);
                    schedule(task_cx_ptr);
                }
            }
        }
    }
}

/// Error returned by [`interruptible`].
#[derive(Debug, PartialEq, Eq)]
pub struct Interrupted;

impl fmt::Display for Interrupted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "interrupted")
    }
}

impl core::error::Error for Interrupted {}

impl From<Interrupted> for SysErrNo {
    fn from(_: Interrupted) -> Self {
        SysErrNo::EINTR
    }
}

/// 封装一个 Future，使其可以被内核中断。
///
/// 在每次轮询内部 Future 之前，都会先检查当前任务是否有挂起的中断。
/// 如果有中断，则直接返回 `Err(Interrupted)`。
/// 来自 Cursor, 定位到这里强引用导致 block_on 阻塞期间多占一份 TCB 强引用。
/// 使用 `Weak` 而非长期持有 `Arc`：避免在 `block_on` 阻塞期间多占一份 TCB 强引用。
pub async fn interruptible<F: core::future::Future>(f: F) -> Result<F::Output, Interrupted> {
    let mut f = pin!(f);
    let curr = Arc::downgrade(&current_task().unwrap());
    poll_fn(move |cx| {
        if let Some(task) = curr.upgrade() {
            if task.poll_interrupt(cx).is_ready() {
                return Poll::Ready(Err(Interrupted));
            }
        } else {
            return Poll::Ready(Err(Interrupted));
        }
        f.as_mut().poll(cx).map(Ok)
    })
    .await
}
