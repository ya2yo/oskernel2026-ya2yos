//!Implementation of [`Processor`] and Intersection of control flow
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
    arch::config::HART_NUM,
    task::switch::switch,
    timer::{check_futex_timer, get_time_ms, TIMER_INTERVAL_MS},
};
use alloc::{boxed::Box, sync::Arc};
use log::{debug, error};
///Processor management structure
pub struct Processor {
    ///The task currently executing on the current processor
    pub current: Option<Arc<TaskControlBlock>>,
    ///The basic control flow of each core, helping to select and switch process
    pub idle_task_cx: Option<Box<TaskContext>>,
    /// Last 10 ms bucket in which scheduler-side timer maintenance ran.
    last_timer_maintenance_tick: usize,
}

///Init PROCESSORS
pub fn processors_init() {
    unsafe {
        for p in (*PROCESSORS.get()).iter_mut() {
            p.idle_task_cx = Some(Box::new(TaskContext::zero_init()));
        }
    }
}

impl Processor {
    ///Create an empty Processor
    pub const fn new() -> Self {
        Self {
            current: None,
            idle_task_cx: None,
            last_timer_maintenance_tick: usize::MAX,
        }
    }
    /// Timer interrupts normally drive these queues. The scheduler also has to
    /// do so after an idle wakeup, but running the same global scans on every
    /// context switch causes severe lock contention under BuildStorm.
    fn should_run_timer_maintenance(&mut self) -> bool {
        let tick = get_time_ms() / TIMER_INTERVAL_MS;
        if tick == self.last_timer_maintenance_tick {
            return false;
        }
        self.last_timer_maintenance_tick = tick;
        true
    }
    ///Get mutable reference to `idle_task_cx`
    fn get_idle_task_cx_ptr(&mut self) -> *mut TaskContext {
        self.idle_task_cx.as_mut().unwrap().as_mut() as *mut _
    }
    ///Get current task in moving semanteme
    pub fn take_current(&mut self) -> Option<Arc<TaskControlBlock>> {
        self.current.take()
    }
    ///Get current task in cloning semanteme
    pub fn current(&self) -> Option<Arc<TaskControlBlock>> {
        self.current.as_ref().map(Arc::clone)
    }
}

const EMPTY_PROCESSOR: Processor = Processor::new();
/// 不需要加锁,每个核只会访问固定的Processor
pub static PROCESSORS: SyncUnsafeCell<[Processor; HART_NUM]> =
    SyncUnsafeCell::new([EMPTY_PROCESSOR; HART_NUM]);

/// Published only around the architecture idle instruction.  Wakers use this
/// to avoid sending an IPI to a hart that is already executing useful work.
static HART_IDLE: [AtomicBool; HART_NUM] = [const { AtomicBool::new(false) }; HART_NUM];

/// Notify a remote hart after a runnable task has been enqueued for it.
///
/// The idle hart publishes `true` before rechecking its run queue, so either
/// the enqueue is observed by that recheck or the sender observes `true` and
/// wakes the hart.  This closes the enqueue-before-WFI lost-wakeup window.
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

fn idle_until_runnable(hartid: usize) {
    HART_IDLE[hartid].store(true, Ordering::Release);
    if !ready_queue::has_ready_for_hart(hartid) {
        crate::arch::cpu::idle();
    }
    HART_IDLE[hartid].store(false, Ordering::Release);
}

///attach to processors
fn get_proc_by_hartid(hartid: usize) -> &'static mut Processor {
    if hartid >= HART_NUM {
        panic!(
            "get_proc_by_hartid: fail because hartid={} is too large!",
            hartid
        )
    }
    unsafe { &mut (*PROCESSORS.get())[hartid] }
}

///The main part of process execution and scheduling
///Loop `fetch_task` to get the process that needs to run, and switch the process through `__switch`
pub fn run_tasks() {
    loop {
        let hartid = hart_id();
        if get_proc_by_hartid(hartid).should_run_timer_maintenance() {
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
            let runnable = matches!(
                cur_task.inner_lock().task_status,
                TaskStatus::Ready | TaskStatus::Running
            );
            if runnable {
                // Enqueue before selection so CFS can compare the current task
                // with every other runnable entity. For RR this preserves the
                // original behavior of appending the current task at the tail.
                ready_queue::add_task(&cur_task);
            }
        }

        if let Some(next_task) = ready_queue::fetch_task(hartid) {
            #[cfg(feature = "perf")]
            {
                crate::utils::perf::record_scheduler_selection(
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
            processor.current = Some(next_task);
            switch(idle_task_cx_ptr, next_task_cx_ptr);
        } else {
            #[cfg(feature = "perf")]
            crate::utils::perf::record_idle_loop();
            idle_until_runnable(hartid);
        }
    }
}
///Take the current task,leaving a None in its place
pub fn take_current_task() -> Option<Arc<TaskControlBlock>> {
    // debug!("[processor]: take_current_task!");
    let task = get_proc_by_hartid(hart_id()).take_current();
    if let Some(task) = &task {
        ready_queue::account_current(task);
    }
    task
}
///Get running task
pub fn current_task() -> Option<Arc<TaskControlBlock>> {
    let cur_task = get_proc_by_hartid(hart_id()).current();
    // if cur_task.is_some() {
    //     debug!("GET current_task's strong_count = {}", Arc::strong_count(&cur_task.clone().unwrap()));
    // }
    cur_task
}
///Get token of the address space of current task
pub fn current_token() -> usize {
    // get_proc_by_hartid(hart_id()).token()
    get_token_from_regs()
}

///Get the mutable reference to trap context of current task
pub fn current_trap_cx() -> &'static mut TrapContext {
    current_task().unwrap().inner_lock().trap_cx()
}
///Return to idle control flow for new scheduling
pub fn schedule(switched_task_cx_ptr: *mut TaskContext) {
    let processor = get_proc_by_hartid(hart_id());
    // debug!(
    //     "[schedule] processor pid = {} , tid = {}",
    //     processor.current().unwrap().pid(),
    //     processor.current().unwrap().tid()
    // );
    let idle_task_cx_ptr = processor.get_idle_task_cx_ptr();

    switch(switched_task_cx_ptr, idle_task_cx_ptr);
}
/// 不会返回，调用前释放局部变量
pub fn abandon(tid: usize) {
    let processor = get_proc_by_hartid(hart_id());

    let idle_task_cx_ptr = processor.get_idle_task_cx_ptr();
    unsafe {
        __abandon(tid, idle_task_cx_ptr);
    }
}
