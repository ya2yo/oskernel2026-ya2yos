//! 每个 Hart 的当前任务管理、idle 控制流和上下文切换入口。
//!
//! 每个 Hart 在 [`PROCESSORS`] 中拥有一个独立的 [`Processor`]，其中保存
//! 当前运行任务以及调度器 idle 上下文。任务主动阻塞、主动让出、被定时器
//! 抢占或退出时，会保存自己的 [`TaskContext`] 并通过 [`schedule`] 返回
//! `run_tasks` 的调度循环；调度循环选择下一个就绪任务后，再恢复该任务的
//! 上下文继续执行。
//!
//! 本模块还维护 idle Hart 的发布/唤醒协议。Hart 在即将执行架构 idle 指令
//! 前发布 [`HART_IDLE`]，其他 Hart 在目标队列加入任务后据此发送 IPI。发布
//! 与重新检查就绪队列的顺序用于避免“入队发生在检查之后、WFI 之前”造成的
//! 丢失唤醒。
use core::{
    cell::SyncUnsafeCell,
    sync::atomic::{AtomicBool, Ordering},
};

use super::{
    __abandon, check_blocked_task_timers, check_timer_events, ready_queue, TaskContext,
    TaskControlBlock, TaskStatus,
};
use crate::arch::cpu::hart_id;

use crate::arch::context::TrapContext;
use crate::arch::page_table::get_token_from_regs;
use crate::task::Process;
use crate::{
    arch::hardware::MAX_SUPPORTED_HARTS,
    task::switch::switch,
    timer::{check_futex_timer, claim_global_timer_maintenance},
};
use alloc::{boxed::Box, sync::Arc};
use log::{debug, error};
use spin::Mutex;
/// 单个 Hart 的处理器本地调度状态。
///
/// 每个 Hart 只访问 [`PROCESSORS`] 中与自身 Hart ID 对应的槽位，因此这些
/// 字段不需要额外的锁。`current` 保存调度器对当前任务的强引用，
/// `idle_task_cx` 保存调度循环的内核上下文；任务上下文与 idle 上下文之间
/// 的切换由架构相关的 [`switch`] 完成。
pub struct Processor {
    /// 当前 Hart 正在执行的任务。
    ///
    /// 该引用在任务完成上下文切出后由 [`Processor::take_current`] 移除，在选定
    /// 下一个任务并将其状态发布为 `Running` 后重新写入。阻塞路径只发布睡眠态，
    /// 调度循环在 idle 栈上清除 `on_cpu` 后才允许其他 Hart 恢复该上下文。
    pub current: Option<Arc<TaskControlBlock>>,
    /// 当前 Hart 的 idle 调度上下文。
    ///
    /// 第一次进入任务时，`run_tasks` 将自己的控制流保存到这里；任务随后
    /// 通过 [`schedule`] 切回此上下文。该 Box 在初始化后保持稳定，因而可以
    /// 将其内部地址传给底层汇编切换函数。
    pub idle_task_cx: Option<Box<TaskContext>>,
}

/// 初始化所有 Hart 的 idle 调度上下文。
///
/// 必须在各 Hart 进入 [`run_tasks`] 之前调用一次。函数只填充每个
/// `Processor` 的 `idle_task_cx`，不会创建任务或修改就绪队列。初始化完成
/// 后不得再移动或替换这些 Box，否则已经交给汇编切换路径的指针可能失效。
pub fn processors_init() {
    unsafe {
        for p in (*PROCESSORS.get()).iter_mut() {
            p.idle_task_cx = Some(Box::new(TaskContext::zero_init()));
        }
    }
}

impl Processor {
    /// 创建一个尚未绑定当前任务和 idle 上下文的处理器状态。
    ///
    /// `processors_init` 会在真正调度前为该状态补充 idle 上下文。
    pub const fn new() -> Self {
        Self {
            current: None,
            idle_task_cx: None,
        }
    }
    /// 获取当前 Hart idle 上下文的可变裸指针。
    ///
    /// 调用前必须完成 [`processors_init`]；返回的指针指向 `idle_task_cx`
    /// 内部的稳定存储，调用方必须保证对应 `Processor` 不被销毁或移动，且
    /// 不得在上下文切换期间再次借用或替换该 Box。
    fn get_idle_task_cx_ptr(&mut self) -> *mut TaskContext {
        self.idle_task_cx.as_mut().unwrap().as_mut() as *mut _
    }
    /// 取出当前任务的调度器强引用，并将槽位置为 `None`。
    ///
    /// 此方法只负责处理器本地引用的转移，不修改任务状态，也不进行运行
    /// 时间记账；面向调度器的 [`take_current_task`] 会在此基础上完成 CFS
    /// 记账。调用方负责根据任务当前状态决定重新入队、阻塞或退出。
    pub fn take_current(&mut self) -> Option<Arc<TaskControlBlock>> {
        self.current.take()
    }
    /// 克隆当前任务的调度器强引用，不改变处理器槽位。
    ///
    /// 返回的 `Arc` 使调用方可以在不持有 `Processor` 可变引用的情况下安全
    /// 读取当前任务；没有当前任务时返回 `None`。
    pub fn current(&self) -> Option<Arc<TaskControlBlock>> {
        self.current.as_ref().map(Arc::clone)
    }
}

/// 用于初始化每个 Hart 处理器槽位的空状态模板。
const EMPTY_PROCESSOR: Processor = Processor::new();

/// 每个 Hart 的处理器本地状态数组。
///
/// 这里使用 [`SyncUnsafeCell`] 而不是 Mutex，是因为调度器保证每个 Hart
/// 只读写自己的槽位；该保证依赖 Hart ID 的正确性和处理器初始化顺序，
/// 不是由类型系统自动检查的。跨 Hart 访问某个槽位时必须另行建立同步，
/// 不能把该数组当作普通共享可变数据结构使用。
pub static PROCESSORS: SyncUnsafeCell<[Processor; MAX_SUPPORTED_HARTS]> =
    SyncUnsafeCell::new([EMPTY_PROCESSOR; MAX_SUPPORTED_HARTS]);

/// Accumulated time spent in the scheduler's idle path for each Hart.
///
/// The value is updated when an idle interval ends.  A reader also includes
/// the currently open interval, so `/proc/uptime` remains monotonic while a
/// Hart is sleeping.
struct IdleAccounting {
    total_ticks: usize,
    started_at: Option<usize>,
}

static IDLE_ACCOUNTING: [Mutex<IdleAccounting>; MAX_SUPPORTED_HARTS] = [const {
    Mutex::new(IdleAccounting {
        total_ticks: 0,
        started_at: None,
    })
}; MAX_SUPPORTED_HARTS];

/// Hart 即将进入或正在执行架构 idle 指令时发布的状态。
///
/// 唤醒方只在目标值为 `true` 时发送 IPI，避免打扰已经执行有用工作的
/// Hart。该状态与就绪队列检查配合使用，不能单独作为“队列为空”的判断。
static HART_IDLE: [AtomicBool; MAX_SUPPORTED_HARTS] =
    [const { AtomicBool::new(false) }; MAX_SUPPORTED_HARTS];

/// Return the idle publication state of every Hart for a diagnostic snapshot.
///
/// This is intentionally an approximate, lock-free view.  It distinguishes a
/// quiescent ready queue from a scheduler stall without adding work to the
/// timer-preemption hot path.
pub(crate) fn idle_hart_snapshot() -> [bool; MAX_SUPPORTED_HARTS] {
    core::array::from_fn(|hartid| HART_IDLE[hartid].load(Ordering::Relaxed))
}

/// 在目标 Hart 的就绪队列加入任务后通知目标 Hart。
///
/// 目标 Hart 会在重新检查就绪队列前以 Release 顺序发布 `HART_IDLE = true`：
/// 如果入队发生在检查之前，目标 Hart 会在检查中看到任务；如果入队发生在
/// 检查之后，发送方会通过 Acquire 读取到 idle 状态并发送 IPI。这样可以关
/// 闭“入队发生在队列检查之后、WFI 之前”的丢失唤醒窗口。
///
/// `target_hart` 必须是有效的 Hart ID；调用方传入任务当前的 placement。
pub(crate) fn notify_hart_of_runnable_task(target_hart: usize) {
    let source_hart = hart_id();
    if target_hart == source_hart {
        #[cfg(feature = "perf")]
        crate::utils::perf::record_scheduler_enqueue(false, false, false);
        return;
    }

    let target_idle = HART_IDLE[target_hart].load(Ordering::Acquire);
    let _ipi_sent = target_idle && crate::arch::cpu::wake_hart(target_hart);
    #[cfg(feature = "perf")]
    crate::utils::perf::record_scheduler_enqueue(true, target_idle, _ipi_sent);
}

/// Wake idle Harts that are allowed to run a newly runnable CFS task.
///
/// The CFS queue is shared by every Hart, so a task is no longer tied to the
/// Hart that last ran it. Wake at most one idle Hart per queued task to avoid a
/// broadcast storm while still preventing runnable work from waiting behind
/// sleeping Harts.
pub(crate) fn notify_harts_of_runnable_task(cpu_mask: usize, ready_tasks: usize) {
    let source_hart = hart_id();
    let mut remote_target = false;
    let mut target_idle = false;
    let mut ipi_sent = false;
    let hart_count = crate::arch::hardware::hart_count().min(MAX_SUPPORTED_HARTS);
    let mut wake_budget = ready_tasks.min(hart_count.saturating_sub(1));
    for target_hart in 0..hart_count {
        if target_hart == source_hart || cpu_mask & (1usize << target_hart) == 0 {
            continue;
        }
        remote_target = true;
        let idle = HART_IDLE[target_hart].load(Ordering::Acquire);
        target_idle |= idle;
        if idle && wake_budget != 0 && crate::arch::cpu::wake_hart(target_hart) {
            ipi_sent = true;
            wake_budget -= 1;
        }
    }
    #[cfg(not(feature = "perf"))]
    let _ = (remote_target, target_idle, ipi_sent);
    #[cfg(feature = "perf")]
    crate::utils::perf::record_scheduler_enqueue(remote_target, target_idle, ipi_sent);
}

/// 在指定 Hart 没有本地就绪任务时进入架构 idle 状态。
///
/// 先发布 idle 状态，再检查一次就绪队列，只有队列仍为空时才执行架构
/// `idle` 指令。入队通知与该顺序配合后，即使任务在检查和 idle 指令之间
/// 到达，也会通过 IPI 或下一轮检查被观察到。返回后清除 idle 发布状态，
/// 让后续唤醒方知道该 Hart 已重新进入调度循环。
fn idle_until_runnable(hartid: usize) {
    HART_IDLE[hartid].store(true, Ordering::Release);
    if !ready_queue::has_ready_for_hart(hartid) {
        let idle_started = crate::arch::time::get_ticks();
        IDLE_ACCOUNTING[hartid].lock().started_at = Some(idle_started);
        crate::arch::cpu::idle();
        let mut accounting = IDLE_ACCOUNTING[hartid].lock();
        accounting.total_ticks = accounting
            .total_ticks
            .saturating_add(crate::arch::time::get_ticks().saturating_sub(idle_started));
        accounting.started_at = None;
    }
    HART_IDLE[hartid].store(false, Ordering::Release);
}

/// Return the total time all Harts have spent idle, including active intervals.
pub fn idle_ticks() -> usize {
    let now = crate::arch::time::get_ticks();
    let hart_count = crate::arch::hardware::hart_count().min(MAX_SUPPORTED_HARTS);
    (0..hart_count).fold(0usize, |total, hartid| {
        let accounting = IDLE_ACCOUNTING[hartid].lock();
        total.saturating_add(
            accounting.total_ticks.saturating_add(
                accounting
                    .started_at
                    .map(|started| now.saturating_sub(started))
                    .unwrap_or(0),
            ),
        )
    })
}

/// 获取指定 Hart 的处理器本地状态。
///
/// `PROCESSORS` 的无锁访问依赖调用方只在对应 Hart 上访问对应槽位；索引
/// 超出 [`MAX_SUPPORTED_HARTS`] 是内核逻辑错误，会立即 panic。当前实现的调用方均
/// 使用当前 Hart ID，并通过返回的独占引用更新本地调度状态。
fn get_proc_by_hartid(hartid: usize) -> &'static mut Processor {
    if hartid >= MAX_SUPPORTED_HARTS {
        panic!(
            "get_proc_by_hartid: fail because hartid={} is too large!",
            hartid
        )
    }
    unsafe { &mut (*PROCESSORS.get())[hartid] }
}

/// 运行当前 Hart 的主调度循环。
///
/// 该函数由 idle 控制流调用且正常不会返回。每次循环依次完成以下工作：
///
/// 1. 按时间桶执行异步计时器、阻塞任务和 futex 的定时器维护。
/// 2. 取出上一个任务，结算其运行时间；仍处于 `Ready` 或 `Running` 的任务
///    重新加入共享就绪队列，阻塞、停止或退出的任务不会重新入队。
/// 3. 从当前 Hart 的调度策略中选择下一个任务，将其状态设置为 `Running`，
///    并记录新的 CFS 运行片段起点。
/// 4. 通过 [`switch`] 从 idle 上下文切换到选中任务；如果没有任务，则发布
///    idle 状态并等待新的就绪任务或唤醒通知。
///
/// `switch` 保存 idle 控制流的返回位置后恢复任务上下文。任务之后通过
/// [`schedule`] 切回该 idle 上下文，函数便从上一次 `switch` 之后继续执行，
/// 因而调度循环本身不依赖普通函数调用返回来完成任务切换。
pub fn run_tasks() {
    loop {
        let hartid = hart_id();
        if let Some(_maintenance) = claim_global_timer_maintenance() {
            check_timer_events();
            check_blocked_task_timers();
            check_futex_timer();
        }
        let cur_task = take_current_task();
        #[cfg(feature = "perf")]
        let dispatch_begin = crate::arch::time::get_ticks();
        let _current_tid = cur_task.as_ref().map(|task| task.tid());
        let processor = get_proc_by_hartid(hartid);
        let idle_task_cx_ptr = processor.get_idle_task_cx_ptr();
        if let Some(cur_task) = cur_task {
            // Linux finish_task(prev) 的对应点：switch() 已完整保存 prev
            // 上下文，此后并发唤醒才可将它放到任意 Hart 的就绪队列。
            cur_task.mark_off_cpu();
            let mut cur_task_inner = cur_task.inner_lock();
            cur_task_inner.mark_rseq_pending();
            let runnable = matches!(
                cur_task_inner.task_status,
                TaskStatus::Ready | TaskStatus::Running
            );
            drop(cur_task_inner);
            if runnable {
                // Enqueue before selection so CFS can compare the current task
                // with every other runnable entity. For RR this preserves the
                // original behavior of appending the current task at the tail.
                ready_queue::add_task(&cur_task);
            }
        }

        if let Some(next_task) = ready_queue::fetch_task(hartid) {
            // CFS reserves Running while holding the queue lock.  An affinity
            // change can still race after that reservation, so return the
            // reservation to Ready before putting the task back.
            if !next_task.can_run_on(hartid) {
                let mut next_task_inner = next_task.inner_lock();
                next_task_inner.task_status = TaskStatus::Ready;
                drop(next_task_inner);
                ready_queue::add_task(&next_task);
                continue;
            }
            next_task.set_scheduled_hart(hartid);
            #[cfg(feature = "perf")]
            {
                crate::utils::perf::record_scheduler_selection(
                    hartid,
                    _current_tid == Some(next_task.tid()),
                );
                crate::utils::perf::record_scheduler_dispatch_duration(
                    crate::arch::time::get_ticks().saturating_sub(dispatch_begin),
                );
            }
            let mut next_task_inner = next_task.inner_lock();
            let next_task_cx_ptr = &next_task_inner.task_cx as *const TaskContext;
            next_task_inner.task_status = TaskStatus::Running;
            drop(next_task_inner);
            ready_queue::mark_running(&next_task);
            // Linux prepare_task(next) 的对应点：在恢复保存的上下文之前
            // 发布 CPU 所有权，阻止其他 Hart 同时调度同一任务。
            next_task.mark_on_cpu();
            processor.current = Some(next_task);
            switch(idle_task_cx_ptr, next_task_cx_ptr);
        } else {
            #[cfg(feature = "perf")]
            crate::utils::perf::record_idle_loop(hartid);
            idle_until_runnable(hartid);
        }
    }
}
/// 取出当前 Hart 的任务，并在任务离开处理器时结算调度运行时间。
///
/// 该函数先清空 `Processor::current`，再调用调度策略的
/// `account_current`；因此任务在重新入队或进入阻塞/退出路径前不会继续被
/// 视为当前运行任务。它不改变 `TaskStatus`，调用方必须在此之前或之后按
/// 具体路径发布正确状态。
pub fn take_current_task() -> Option<Arc<TaskControlBlock>> {
    // debug!("[processor]: take_current_task!");
    let task = get_proc_by_hartid(hart_id()).take_current();
    if let Some(task) = &task {
        // The task's satp remains installed until the scheduler switches away
        // from its saved context.  Move to the immutable kernel page table
        // before clearing the active bit; otherwise a remote address-space
        // writer may reclaim the old root while this hart is still executing
        // scheduler code with that satp.
        crate::mm::activate_kernel_space();
        task.process.memory_set_arc().deactivate_current_hart();
        ready_queue::account_current(task);
    }
    task
}
/// 克隆当前 Hart 正在运行的任务引用。
///
/// 返回的 `Arc` 只保证任务对象在调用方使用期间保持存活，不会延长任务在
/// 处理器槽位中的调度归属；没有已选中任务时返回 `None`。
pub fn current_task() -> Option<Arc<TaskControlBlock>> {
    let cur_task = get_proc_by_hartid(hart_id()).current();
    // if cur_task.is_some() {
    //     debug!("GET current_task's strong_count = {}", Arc::strong_count(&cur_task.clone().unwrap()));
    // }
    cur_task
}
/// 读取当前硬件地址转换寄存器中的地址空间 token。
///
/// 该值来自当前 Hart 正在使用的根页表寄存器，而不是从 `Processor` 或
/// `TaskControlBlock` 字段推导，因此适用于 trap 入口、页表切换和架构层
/// 代码需要查询活动地址空间的场景。
pub fn current_token() -> usize {
    // get_proc_by_hartid(hart_id()).token()
    get_token_from_regs()
}

/// 获取当前任务用户态 trap context 的可变引用。
///
/// 返回值指向当前任务地址空间中预留的 trap context。调用方必须保证当前
/// Hart 仍在执行该任务，并避免在其他路径同时修改同一 trap context；函数
/// 不会替调用方获取或持有 `TaskControlBlockInner` 锁。
pub fn current_trap_cx() -> &'static mut TrapContext {
    current_task().unwrap().inner_lock().trap_cx()
}
/// 保存当前任务上下文并切回当前 Hart 的 idle 调度上下文。
///
/// `switched_task_cx_ptr` 必须指向当前任务自己的 `TaskContext`，且调用前
/// 应释放任务内部锁、文件系统锁和其他可能阻塞或参与调度的锁。底层切换
/// 不会复制上下文，只会把当前寄存器状态写入该地址并恢复 idle 上下文；
/// 当前任务以后再次被选中时，函数才会从这里返回。
pub fn schedule(switched_task_cx_ptr: *mut TaskContext) {
    // `suspend_current_and_run_next()` can call schedule directly without
    // first detaching the processor's current slot.  Move to the kernel page
    // table before dropping the user address-space active bit, so a remote
    // writer can stop waiting for this hart without reclaiming a page table
    // still used by scheduler code.
    crate::mm::activate_kernel_space();
    if let Some(task) = current_task() {
        task.process.memory_set_arc().deactivate_current_hart();
    }
    let processor = get_proc_by_hartid(hart_id());
    // debug!(
    //     "[schedule] processor pid = {} , tid = {}",
    //     processor.current().unwrap().pid(),
    //     processor.current().unwrap().tid()
    // );
    let idle_task_cx_ptr = processor.get_idle_task_cx_ptr();

    switch(switched_task_cx_ptr, idle_task_cx_ptr);
}
/// 放弃当前任务的内核栈并切回 idle 调度上下文。
///
/// 这是任务退出路径专用的不可返回控制流。调用方必须在进入本函数前释
/// 放所有局部锁、任务引用和其他依赖当前内核栈的资源；`tid` 应是当前即将
/// 退出任务的 TID。架构汇编入口 [`__abandon`] 会恢复 idle 上下文但丢弃
/// 当前任务上下文，并把 `tid` 传回 idle 侧的 [`switch`] 包装器，以便后者
/// 从全局 TID 表中完成清理。
pub fn abandon(tid: usize) {
    let processor = get_proc_by_hartid(hart_id());

    let idle_task_cx_ptr = processor.get_idle_task_cx_ptr();
    unsafe {
        __abandon(tid, idle_task_cx_ptr);
    }
}
