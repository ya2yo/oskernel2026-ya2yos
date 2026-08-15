//! 基于虚拟运行时间（`vruntime`）的简化 CFS 调度策略。
//!
//! 所有 Hart 共享一个最小堆就绪队列，从 `(vruntime, tid)` 最小的任务开始
//! 选择。任务实际保存的 `vruntime` 会按 Linux `nice` 权重进行归一化：
//! 权重越高，实际运行相同时间后增加的 `vruntime` 越少，因此越容易再次被
//! 选中。
//!
//! 调度循环在取走当前任务时先完成运行时间记账，再把仍然可运行的任务
//! 放回共享队列。新任务入队时会被放置到全局 `min_vruntime` 坐标系中。
//! 取任务时按线程 affinity 过滤，允许任意满足 affinity 的 Hart 从同一队列
//! 获取任务。队列只保存 `Weak<TaskControlBlock>`，任务状态由任务自身和
//! `on_rq` 原子标志共同确认；因此任务退出或出现过期队列项时，取任务路径
//! 可以直接丢弃该项而不延长任务生命周期。

use super::TaskControlBlock;
use crate::arch::time::get_ticks;
use crate::task::TaskStatus;
use alloc::{
    collections::BinaryHeap,
    sync::{Arc, Weak},
};
use core::{
    cmp::Ordering as CmpOrdering,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};
use log::warn;
use spin::{Lazy, Mutex};

/// `nice == 0` 任务的基准权重，用于换算虚拟运行时间。
const NICE_0_LOAD: u64 = 1024;
/// 表示当前没有有效运行起始时间的哨兵值。
const NOT_RUNNING: u64 = u64::MAX;

/// Linux CFS 的 `prio_to_weight` 表，索引对应 `nice + 20`，覆盖 `-20..19`。
const NICE_WEIGHTS: [u32; 40] = [
    88761, 71755, 56483, 46273, 36291, 29154, 23254, 18705, 14949, 11916, 9548, 7620, 6100, 4904,
    3906, 3121, 2501, 1991, 1586, 1277, 1024, 820, 655, 526, 423, 335, 272, 215, 172, 137, 110, 87,
    70, 56, 45, 36, 29, 23, 18, 15,
];

/// 与任务关联的 CFS 调度实体。
///
/// 这些字段使用原子类型，是因为任务的唤醒、入队和运行状态可能由不同
/// Hart 观察或更新。真正保护就绪队列堆结构的锁则位于 [`CfsRunQueue`]。
pub(crate) struct SchedEntity {
    /// 任务累计的虚拟运行时间；调度时优先选择值较小的实体。
    vruntime: AtomicU64,
    /// 最近一次被选中运行的硬件 tick；`NOT_RUNNING` 表示无需记账。
    exec_start: AtomicU64,
    /// 任务是否已经被某个就绪队列项占用，防止重复入队。
    on_rq: AtomicBool,
}

impl SchedEntity {
    /// 创建一个尚未运行、也尚未进入就绪队列的调度实体。
    pub const fn new() -> Self {
        Self {
            vruntime: AtomicU64::new(0),
            exec_start: AtomicU64::new(NOT_RUNNING),
            on_rq: AtomicBool::new(false),
        }
    }

    /// 以 Acquire 顺序读取当前虚拟运行时间。
    fn vruntime(&self) -> u64 {
        self.vruntime.load(Ordering::Acquire)
    }

    /// 将实体放入指定就绪队列的 `min_vruntime` 坐标系。
    ///
    /// 该操作只会增大实体的 `vruntime`，避免刚创建或长期阻塞的任务因
    /// 携带过小的旧值而一次性抢占过多 CPU 时间。CAS 循环同时允许唤醒
    /// 路径与运行时间记账路径安全地竞争更新这个值。
    fn place_at(&self, min_vruntime: u64) -> u64 {
        let mut current = self.vruntime();
        while current < min_vruntime {
            match self.vruntime.compare_exchange_weak(
                current,
                min_vruntime,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return min_vruntime,
                Err(actual) => current = actual,
            }
        }
        current
    }

    /// 清除就绪队列占用标志。
    ///
    /// 取任务路径会在检查任务状态前清除此标志，以便并发唤醒者在发现
    /// 任务不再由当前队列项负责时，可以发布新的队列项。
    fn mark_dequeued(&self) {
        self.on_rq.store(false, Ordering::Release);
    }

    /// 标记实体开始运行，并记录本次运行片段的起始 tick。
    fn mark_running(&self) {
        self.mark_dequeued();
        self.exec_start.store(get_ticks() as u64, Ordering::Release);
    }

    /// 按任务的 `nice` 值将最近一次运行片段折算为虚拟运行时间。
    ///
    /// 换算公式为 `delta_exec * NICE_0_LOAD / weight`。使用 `u128` 执行
    /// 中间乘法，避免 `u64` 乘法溢出；若结果超出范围则饱和到 `u64::MAX`。
    /// 没有有效运行片段时，此函数不修改实体状态。
    fn account_runtime(&self, nice: i32) {
        let start = self.exec_start.swap(NOT_RUNNING, Ordering::AcqRel);
        if start == NOT_RUNNING {
            return;
        }
        let delta_exec = (get_ticks() as u64).wrapping_sub(start);
        let weight = NICE_WEIGHTS[(nice.clamp(-20, 19) + 20) as usize] as u64;
        let delta_vruntime = ((delta_exec as u128 * NICE_0_LOAD as u128) / weight as u128)
            .min(u64::MAX as u128) as u64;
        self.vruntime.fetch_add(delta_vruntime, Ordering::AcqRel);
    }

    /// 尝试原子地声明一个待入队位置，成功时返回 `true`。
    ///
    /// 一个任务在堆中最多保留一个有效占位，因此重复唤醒不会产生重复
    /// 队列项。该标志在取任务路径完成状态检查后，或任务开始运行时清除。
    fn try_mark_queued(&self) -> bool {
        self.on_rq
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}

/// CFS 就绪队列的排序键：先比较虚拟运行时间，再用 TID 稳定打破平局。
type QueueKey = (u64, usize);

/// 就绪堆中的一项。
///
/// 任务使用弱引用保存，避免调度器队列成为任务生命周期的额外所有者。
struct CfsEntry {
    /// 用于堆排序的 `(vruntime, tid)`。
    key: QueueKey,
    /// 可能已经失效的任务引用；取任务时通过 `upgrade` 验证。
    task: Weak<TaskControlBlock>,
}

impl PartialEq for CfsEntry {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}

impl Eq for CfsEntry {}

impl PartialOrd for CfsEntry {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}

impl Ord for CfsEntry {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        // BinaryHeap 是最大堆；反转键顺序后，peek/pop 暴露最小的
        // (vruntime, tid)，从而得到 CFS 所需的最小堆行为。
        other.key.cmp(&self.key)
    }
}

/// 所有 Hart 共享的 CFS 就绪队列。
struct CfsRunQueue {
    /// 以最小 `(vruntime, tid)` 为队首的任务堆。
    tasks: BinaryHeap<CfsEntry>,
    /// 共享队列使用的单调非递减虚拟运行时间基准。
    min_vruntime: u64,
}

impl CfsRunQueue {
    /// 创建一个空的共享就绪队列。
    fn new() -> Self {
        Self {
            tasks: BinaryHeap::new(),
            min_vruntime: 0,
        }
    }

    /// 根据当前调度进展推进 `min_vruntime`，且保证其不会回退。
    ///
    /// 队列非空时使用队首实体与当前值中较小者作为候选值；队列为空时
    /// 直接使用当前值。这样既能跟随已知的最左实体，也不会因为任务退出
    /// 或重新入队而让时间基准倒退。
    fn advance_min_vruntime(&mut self, current: u64) {
        let candidate = self
            .tasks
            .peek()
            .map(|leftmost| leftmost.key.0.min(current))
            .unwrap_or(current);
        self.min_vruntime = self.min_vruntime.max(candidate);
    }
}

/// 所有 Hart 共享的 CFS 就绪队列。
///
/// 共享地址空间线程可以在不同 Hart 上运行，因此不能再把任务按
/// `scheduled_hart` 固定到某一个队列。队列锁只保护堆和 `min_vruntime`；
/// 任务状态、affinity 与调度实体仍由各自的原子字段或任务锁保护。
static READY_QUEUE: Lazy<Mutex<CfsRunQueue>> = Lazy::new(|| Mutex::new(CfsRunQueue::new()));

/// 将可运行任务加入共享 CFS 队列。
///
/// 入队前通过 `on_rq` 去重，并把任务的 `vruntime` 提升到队列当前的
/// `min_vruntime`，然后以 `(vruntime, tid)` 作为堆键。任务本体只以弱引用
/// 放入队列；调用方仍负责持有任务的有效 `Arc`。
/// 
/// 返回值：新队列的长度
pub(super) fn add_task(task: &Arc<TaskControlBlock>) -> Option<usize> {
    let tid = task.tid();
    let mut queue = READY_QUEUE.lock();
    // The queue lock serializes this check with fetch_task's transition to
    // Running.  Checking status before taking the queue lock leaves a window
    // where another hart can select the same task while it is being enqueued.
    let status = task.inner_lock().task_status;
    // 唤醒可以在旧 Hart 完成上下文切出前把睡眠态改回 Ready；此时只记录
    // 状态变化，由 finish_task 对应路径清除 on_cpu 后再负责入队。
    if status != TaskStatus::Ready || task.is_on_cpu() {
        return None;
    }
    if !task.sched_entity.try_mark_queued() {
        // finish_task-side requeue and a concurrent wakeup may both arrive
        // after on_cpu is cleared. They represent one runnable task, so merge
        // the duplicate exactly like Linux's already-on-rq wakeup path.
        return None;
    }
    let vruntime = task.sched_entity.place_at(queue.min_vruntime);
    queue.tasks.push(CfsEntry {
        key: (vruntime, tid),
        task: Arc::downgrade(task),
    });
    Some(queue.tasks.len())
}

/// 从共享 CFS 队列中取出指定 Hart 可以运行的下一个任务。
///
/// 堆项可能因任务退出、阻塞或重复调度而过期，因此函数会循环丢弃无效
/// 弱引用和非 `Ready` 任务。暂时不满足 affinity 的任务会保留在堆中，
/// 继续寻找当前 Hart 可执行的最小 `vruntime` 任务。
pub(super) fn fetch_task(hartid: usize) -> Option<Arc<TaskControlBlock>> {
    let mut queue = READY_QUEUE.lock();
    let queued = queue.tasks.len();
    for _ in 0..queued {
        let Some(entry) = queue.tasks.pop() else {
            break;
        };
        let Some(task) = entry.task.upgrade() else {
            warn!("fetch task got a dropped task");
            continue;
        };
        let can_run = task.can_run_on(hartid);
        let status = {
            let mut inner = task.inner_lock();
            let status = inner.task_status;
            if status == TaskStatus::Ready && can_run {
                // Reserve the task before releasing the queue lock.  A wakeup
                // on another hart can otherwise observe Ready after
                // mark_dequeued() and dispatch this TCB concurrently.
                inner.task_status = TaskStatus::Running;
                task.sched_entity.mark_dequeued();
            } else if status != TaskStatus::Ready {
                // Keep the queue claim and task-state observation ordered so a
                // concurrent waker can publish a replacement entry safely.
                task.sched_entity.mark_dequeued();
            }
            status
        };
        if status != TaskStatus::Ready {
            warn!(
                "fetch_task: discard stale CFS entry tid={}, status={:?}",
                task.tid(),
                status
            );
            continue;
        }
        if !can_run {
            // Keep the queue membership claim while the entry waits for an
            // allowed Hart. Rebuilding the weak entry drops the old heap item
            // without changing task lifetime ownership.
            queue.tasks.push(CfsEntry {
                key: entry.key,
                task: Arc::downgrade(&task),
            });
            continue;
        }
        queue.advance_min_vruntime(entry.key.0);
        return Some(task);
    }
    None
}

/// 返回共享就绪堆项数量。
///
/// 该值包含尚未被清理的过期项，适合用于调度器负载观察，不代表严格的
/// 可运行任务数量。
pub(super) fn ready_procs_num() -> usize {
    READY_QUEUE.lock().tasks.len()
}

/// 返回指定 Hart 是否存在至少一个待竞争的就绪堆项。
///
/// 该查询用于决定定时器抢占和 `yield` 是否需要进入调度路径；无效弱引
/// 用会在真正取任务时清理。
pub(super) fn has_ready_for_hart(hartid: usize) -> bool {
    READY_QUEUE.lock().tasks.iter().any(|entry| {
        entry
            .task
            .upgrade()
            .map(|task| task.can_run_on(hartid))
            .unwrap_or(false)
    })
}

/// 在调度器将任务状态发布为 `Running` 后开始新的运行时间片记账。
pub(super) fn mark_running(task: &Arc<TaskControlBlock>) {
    task.sched_entity.mark_running();
}

/// 结算当前任务刚结束的运行片段，并推进实际执行 hart 的时间基准。
///
/// 调用方应在任务离开 processor、重新进入就绪队列之前调用此函数；随后
/// `add_task` 会使用更新后的 `vruntime` 参与下一轮选择。
pub(super) fn account_current(task: &Arc<TaskControlBlock>) {
    let nice = task.inner_lock().nice;
    task.sched_entity.account_runtime(nice);

    READY_QUEUE
        .lock()
        .advance_min_vruntime(task.sched_entity.vruntime());
}
