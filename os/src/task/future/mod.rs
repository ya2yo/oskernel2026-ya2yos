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
    task::{
        current_task, exit_current_if_group_exited_or_killed, ready_queue, schedule,
        take_current_task, TaskContext, TaskStatus,
    },
    utils::SysErrNo,
};
use kernel_guard::NoPreemptIrqSave;
use spin::Mutex;

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
    woke: Mutex<bool>,
}

impl MyWaker {
    /// 为指定的任务创建一个新的 Waker
    fn new(task: &TaskRef) -> Arc<Self> {
        Arc::new(MyWaker {
            task: Arc::downgrade(task),
            woke: Mutex::new(false),
        })
    }

    /// Atomically consume a pending wakeup or publish the current task as
    /// blocked before handing control back to the scheduler.
    ///
    /// `wake_by_ref` takes `woke` before the task inner lock as well. Keeping
    /// that order here closes the SMP race where a remote hart woke a Running
    /// task between `Poll::Pending` and `block_current_and_run_next`; the task
    /// would then become Blocked after the waker had decided not to enqueue it.
    fn block_current_if_not_woken(&self) {
        let mut woke = self.woke.lock();
        if *woke {
            *woke = false;
            return;
        }

        let task = take_current_task().unwrap();
        let task_cx_ptr = {
            let mut inner = task.inner_lock();
            inner.task_status = TaskStatus::Blocked;
            &mut inner.task_cx as *mut TaskContext
        };
        drop(task);
        drop(woke);
        schedule(task_cx_ptr);
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
            // Hold the wake flag lock through the task-state decision. The
            // blocking side takes the same locks in the same order, so a wake
            // cannot be consumed before the task publishes Blocked.
            let mut woke = self.woke.lock();
            *woke = true;
            // 只把真正睡眠的任务放回 ready queue。poll/register 过程中可能
            // 同步 wake 当前 Running 任务；若把 Running 任务也入队，会造成
            // 同一 TCB 被重复调度并触发 inner_lock 重入。
            let mut inner = task.inner_lock();
            let should_ready = inner.task_status == TaskStatus::Blocked;
            if should_ready {
                inner.task_status = TaskStatus::Ready;
            }
            drop(inner);
            drop(woke);
            if should_ready {
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
        // Keep SIGKILL and exit_group handling consistent with cooperative
        // scheduling, including the waitpid-visible signal termination cause.
        exit_current_if_group_exited_or_killed();

        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(output) => return output,
            Poll::Pending => {
                waker_inner.block_current_if_not_woken();
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
    let result = poll_fn(move |cx| {
        if let Some(task) = curr.upgrade() {
            if task.poll_interrupt(cx).is_ready() {
                return Poll::Ready(Err(Interrupted));
            }
        } else {
            return Poll::Ready(Err(Interrupted));
        }
        f.as_mut().poll(cx).map(Ok)
    })
    .await;
    current_task().unwrap().clear_interrupt_waiter();
    result
}
