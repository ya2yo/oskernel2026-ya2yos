//!Implementation of [`Processor`] and Intersection of control flow
use core::cell::SyncUnsafeCell;

use super::{__abandon, ready_queue, TaskContext, TaskControlBlock, TaskStatus};
use crate::arch::cpu::hart_id;

use crate::arch::context::TrapContext;
use crate::arch::page_table::get_token_from_regs;
use crate::task::Process;
use crate::{config::HART_NUM, task::switch::switch, timer::check_futex_timer};
use alloc::{boxed::Box, sync::Arc};
use log::{debug, error};
///Processor management structure
pub struct Processor {
    ///The task currently executing on the current processor
    pub current: Option<Arc<TaskControlBlock>>,
    ///The basic control flow of each core, helping to select and switch process
    pub idle_task_cx: Option<Box<TaskContext>>,
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
        }
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
        check_futex_timer();
        let processor = get_proc_by_hartid(hart_id());
        let idle_task_cx_ptr = processor.get_idle_task_cx_ptr();
        if let Some(cur_task) = take_current_task() {
            debug!("Task id: {} is running.", cur_task.tid());
            let mut cur_task_inner = cur_task.inner_lock();

            if let Some(next_task) = ready_queue::fetch_task() {
                let mut next_task_inner = next_task.inner_lock();
                let next_task_cx_ptr = &next_task_inner.task_cx as *const TaskContext;
                next_task_inner.task_status = TaskStatus::Running;
                drop(next_task_inner);
                drop(cur_task_inner);
                processor.current = Some(next_task);
                ready_queue::add_task(&cur_task);
                switch(idle_task_cx_ptr, next_task_cx_ptr);
            } else {
                cur_task_inner.task_status = TaskStatus::Running;
                let cur_task_cx_ptr = &cur_task_inner.task_cx as *const TaskContext;
                drop(cur_task_inner);
                processor.current = Some(cur_task);
                switch(idle_task_cx_ptr, cur_task_cx_ptr);
            }
        } else {
            // 第一次调度，抢占
            if let Some(task) = ready_queue::fetch_task() {
                debug!("first fetch task {}", task.pid());
                let mut task_inner = task.inner_lock();
                let next_task_cx_ptr = &task_inner.task_cx as *const TaskContext;
                task_inner.task_status = TaskStatus::Running;
                drop(task_inner);
                processor.current = Some(task);
                switch(idle_task_cx_ptr, next_task_cx_ptr);
            }
            //不切换到内核的地址空间，可能继续运行或转到别的任务
        }
    }
}
///Take the current task,leaving a None in its place
pub fn take_current_task() -> Option<Arc<TaskControlBlock>> {
    debug!("[processor]: take_current_task!");
    get_proc_by_hartid(hart_id()).take_current()
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
    debug!(
        "[schedule] processor pid = {} , tid = {}",
        processor.current().unwrap().pid(),
        processor.current().unwrap().tid()
    );
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
