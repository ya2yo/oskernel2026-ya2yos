//! 定时器条件变量 (TimerCondVar) — 用于阻塞超时唤醒
//!
//! # 设计
//! 与 itimer (周期性信号) 不同，`TimerCondVar` 用于等待特定时间点后
//! 唤醒被阻塞的任务 (如 `futex(FUTEX_WAIT)` 超时)。
//!
//! # 数据结构
//! - **`TIMERS`**: 全局最小堆 (`BinaryHeap`)，按 `expire` 排序
//! - **`TimerCondVar`**: 单个定时器条目
//!   - `expire`: 超时时刻 (Timespec)
//!   - `task`: 等待任务的弱引用
//!   - `kind`: 定时器类型 (Futex / StoppedTask)
//!   - `extra_data`: 附加信息 (如 futex_key 版本号)
//!
//! # 工作流程
//! 1. `add_futex_timer()` / `add_stopped_task_timer()` 向堆中插入条目
//! 2. 每次时钟中断调用 `check_futex_timer()` 检查堆顶是否到期
//! 3. 到期条目被弹出，对应任务被唤醒

use alloc::{
    collections::BinaryHeap,
    sync::{Arc, Weak},
};
use core::cmp::Ordering;
use spin::{Lazy, Mutex};

use crate::task::{handle_timer, TaskControlBlock};

use super::{get_time_spec, Timespec};

#[derive(Debug, PartialEq, Eq)]
pub enum TimerType {
    Futex,
    StoppedTask,
}

/// 定时器条目，存入全局 TIMERS 最小堆
pub struct TimerCondVar {
    pub expire: Timespec,
    pub task: Weak<TaskControlBlock>,
    pub kind: TimerType,
    /// 附加数据 (futex_key 版本号等)
    pub extra_data: usize,
}

impl PartialEq for TimerCondVar {
    fn eq(&self, other: &Self) -> bool {
        self.expire == other.expire
    }
}
impl Eq for TimerCondVar {}

impl PartialOrd for TimerCondVar {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TimerCondVar {
    fn cmp(&self, other: &Self) -> Ordering {
        // 取反实现最小堆 (BinaryHeap 默认最大堆)
        other.expire.to_tick().cmp(&self.expire.to_tick())
    }
}

/// 全局定时器最小堆
pub static TIMERS: Lazy<Mutex<BinaryHeap<TimerCondVar>>> =
    Lazy::new(|| Mutex::new(BinaryHeap::new()));

/// 添加 futex 超时定时器
///
/// - `expire`: 超时时刻
/// - `task`: 等待 futex 的任务
/// - `futex_key`: futex 版本号 (用于区分不同次的 FUTEX_WAIT)
pub fn add_futex_timer(expire: Timespec, task: &Arc<TaskControlBlock>, futex_key: usize) {
    let mut timers = TIMERS.lock();
    timers.push(TimerCondVar {
        expire,
        task: Arc::downgrade(task),
        kind: TimerType::Futex,
        extra_data: futex_key,
    });
}

/// 添加被停止任务的超时定时器 (未实现)
pub fn add_stopped_task_timer(expire: Timespec, task: Arc<TaskControlBlock>) {
    let mut timers = TIMERS.lock();
    timers.push(TimerCondVar {
        expire,
        task: Arc::downgrade(&task),
        kind: TimerType::StoppedTask,
        extra_data: 0,
    });
}

/// 检查并唤醒所有到期的定时器 (每次时钟中断调用)
pub fn check_futex_timer() {
    let mut timers = TIMERS.lock();
    let current = get_time_spec();
    while let Some(timer) = timers.peek() {
        if timer.expire <= current {
            if let Some(task) = timer.task.upgrade() {
                if timer.kind == TimerType::Futex {
                    handle_timer(Arc::clone(&task), timer.extra_data);
                } else if timer.kind == TimerType::StoppedTask {
                    todo!()
                }
            }
            timers.pop();
        } else {
            break;
        }
    }
}