//! 异步 Future 支持模块
//! 提供了在内核空间执行 Future 的基础架构，包括 Waker 实现和 block_on 执行器
use alloc::{sync::Arc, task::Wake};
use core::{
    fmt,
    future::poll_fn,
    pin::pin,
    task::{Context, Poll, Waker},
};

use crate::utils::SysErrNo;
use kernel_guard::NoPreemptIrqSave;
use kspin::SpinNoIrq;
use super::{WeakTaskRef, TaskRef};

mod poll;
pub use poll::*;

mod time;
pub use time::*;

/// 内核任务唤醒器
/// 关联了一个具体的内核任务，当 Future 就绪时，通过它唤醒对应的任务。
struct Waker {
    /// 目标任务的弱引用，防止循环引用导致任务无法释放
    task: WeakTaskRef,
    /// 唤醒状态标志，使用带自旋锁的 bool 保证多核安全
    woke: SpinNoIrq<bool>,
}

impl Waker {
    /// 为指定的任务创建一个新的 Waker
    fn new(task: &TaskRef) -> Arc<Self> {
        Arc::new(Waker {
            task: Arc::downgrade(task),
            woke: SpinNoIrq::new(false),
        })
    }
}

impl Wake for Waker {
    /// 消耗型唤醒,转移所有权
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    /// 引用型唤醒，不转移所有权
    /// 这是唤醒逻辑的核心：将任务从阻塞态移回就绪态。
    fn wake_by_ref(self: &Arc<Self>) {
        // 尝试将弱引用升级为强引用
        if let Some(task) = self.task.upgrade() {
            // 获取任务所属的运行队列   
            let mut rq = select_run_queue::<NoPreemptIrqSave>(&task);
            // 标记已唤醒
            *self.woke.lock() = true;
            // 调用调度器接口，取消任务的阻塞状态
            rq.unblock_task(task, false);
        }
    }
}

/// 阻塞当前任务直到给定的 Future 执行完成。
///
/// 这是内核中的“同步转异步”桥梁。它会不断轮询 Future，
/// 如果 Future 返回 Pending，则会将当前任务挂起（休眠）。
/// 
/// 注意：此函数不处理中断，通常不建议在需要响应信号的用户态任务中直接使用。
pub fn block_on<F: IntoFuture>(f: F) -> F::Output {
    // 将 Future 固定在栈上（Pinning）
    let mut fut = pin!(f.into_future());
    // 获取当前正在运行的任务
    let curr = current();
    // 保持对当前任务的强引用，确保在阻塞期间任务对象不被销毁
    let task = curr.clone();
    // 创建 Waker 并包装成标准库的 Context
    let waker = AxWaker::new(&task);
    let woke = &waker.woke;
    let waker = Waker::from(waker.clone());
    let mut cx = Context::from_waker(&waker);

    loop {
        // 重置唤醒标志
        *woke.lock() = false;
        // 尝试轮询 Future
        match fut.as_mut().poll(&mut cx) {
            Poll::Pending => {
                // 如果 Future 还没准备好，准备阻塞当前任务
                let mut rq = current_run_queue::<NoPreemptIrqSave>();
                let woke = woke.lock();
                if !*woke {
                    // 如果在 poll 之后、进入此逻辑前没有发生唤醒，则真正进入阻塞调度
                    // 传入锁保护的变量是为了在释放锁的同时进行上下文切换
                    rq.blocked_resched(woke);
                } else {
                     // 如果在执行过程中已经被唤醒
                    // 则释放锁并主动让出 CPU，稍后再次尝试
                    drop(woke);
                    crate::yield_now();
                }
            }
            // Future 已完成，返回其结果
            Poll::Ready(output) => break output,
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

impl From<Interrupted> for AxError {
    fn from(_: Interrupted) -> Self {
        AxError::Interrupted
    }
}

/// 封装一个 Future，使其可以被内核中断。
///
/// 在每次轮询内部 Future 之前，都会先检查当前任务是否有挂起的中断。
/// 如果有中断，则直接返回 `Err(Interrupted)`。
pub async fn interruptible<F: IntoFuture>(f: F) -> Result<F::Output, Interrupted> {
    let mut f = pin!(f.into_future());
    let curr = current();
    poll_fn(|cx| {
        // 首先检查当前任务的中断状态
        if curr.poll_interrupt(cx).is_ready() {
            return Poll::Ready(Err(Interrupted));
        }
        // 如果没有中断，则轮询原本的 Future
        f.as_mut().poll(cx).map(Ok)
    })
    .await
}
