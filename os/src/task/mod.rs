//! Task management implementation
//!
//! Everything about task management, like starting and switching tasks is
//! implemented here.
//!
//! A single global instance of [`TaskManager`] called `TASK_MANAGER` controls
//! all the tasks in the whole operating system.
//!
//! A single global instance of [`Processor`] called `PROCESSOR` monitors running
//! task(s) for each core.
//!
//! A single global instance of [`PidAllocator`] called `PID_ALLOCATOR` allocates
//! pid for user apps.
//!
//! Be careful when you see `__switch` ASM function in `switch.S`. Control flow around this function
//! might not be what you expect.

#[allow(clippy::module_inception)]
#[allow(rustdoc::private_intra_doc_links)]
mod aux;
mod futex;
mod kernel_stack;
mod manager;
mod processor;
mod switch;
mod sysinfo;
mod task;
mod tid;
mod future;

pub use future::sleep_until;
pub use crate::arch::context::TaskContext;
use crate::{
    arch::cpu::hart_id,
    arch::memory_layout::USER_STACK_SIZE,
    fs::{open, remove_proc_dir_and_file, OpenFlags, NONE_MODE},
    mm::{activate_kernel_space, get_data, put_data, VirtAddr},
    signal::{send_signal_to_thread_group, SigSet},
    task::{kernel_stack::KernelStackOnHeap, processor::abandon},
};
use alloc::{boxed::Box, sync::Arc, vec::Vec};
pub use futex::*;
use log::{debug, error};
pub use manager::*;
use spin::Lazy;
use switch::__abandon;
pub use sysinfo::Sysinfo;
pub use task::{Process, RobustList, TaskControlBlock, TaskStatus, TaskRef, WeakTaskRef};

pub use aux::*;
pub use processor::{
    current_task, current_token, current_trap_cx, run_tasks, schedule, take_current_task,
    Processor, PROCESSORS,
};
pub use tid::TidHandle;
/// Suspend the current 'Running' task and run the next task in task list.
pub fn suspend_current_and_run_next() {
    let task = current_task().unwrap();
    let mut task_inner = task.inner_lock();
    let task_cx_ptr = &mut task_inner.task_cx as *mut TaskContext;
    // Change status to Ready
    task_inner.task_status = TaskStatus::Ready;
    // ---- release current PCB
    drop(task_inner);
    drop(task);
    // jump to scheduling cycle
    schedule(task_cx_ptr);
}

pub fn block_current_and_run_next() {
    // error!("block_current_and_run_next() BEGIN!");
    let task = take_current_task().unwrap();
    let mut task_inner = task.inner_lock();
    let task_cx_ptr = &mut task_inner.task_cx as *mut TaskContext;
    task_inner.task_status = TaskStatus::Blocked;
    drop(task_inner);
    drop(task);
    // error!("schedule() BEGIN!");
    schedule(task_cx_ptr);
}

// pub fn stop_current_and_run_next() {
//     let task = take_current_task().unwrap();
//     let mut task_inner = task.inner_lock();
//     let task_cx_ptr = &mut task_inner.task_cx as *mut TaskContext;
//     task_inner.task_status = TaskStatus::Stopped;
//     drop(task_inner);
//     // drop(task);
//     stop_task(task);
//     schedule(task_cx_ptr);
// }

/// pid of usertests app in make run TEST=1
pub const IDLE_PID: usize = 0;

/// 杀死当前线程组的所有线程
pub fn exit_current_group_and_run_next(exit_code: i32) {
    let task = current_task().unwrap();
    let task_inner = task.inner_lock();
    let mut exit_code = exit_code;
    let process = task.process.inner_lock();
    let sigtable = process.get_locked_sigtable();

    if sigtable.not_exited() {
        //设置进程的SIGNAL_GROUP_EXIT标志并把终止代号放到current->signal->group_exit_code字段
        sigtable.set_exit_code(exit_code);
        let pid = task.pid();
        drop(sigtable);
        drop(process);
        drop(task_inner);
        drop(task);
        send_signal_to_thread_group(pid, SigSet::SIGKILL);
    } else {
        exit_code = sigtable.exit_code();
        drop(sigtable);
        drop(process);
        drop(task_inner);
        drop(task);
    }

    exit_current_and_run_next(exit_code);
}

pub fn exit_current_and_run_next(exit_code: i32) {
    let task = take_current_task().unwrap();
    let process = task.process.inner_lock();
    let memory_set = process.get_locked_memory_set();
    let mut inner = task.inner_lock();
    debug!(
        "[sys_exit] exit_current_and_run_next() -- thread {} exit, exit_code = {}",
        task.tid(),
        exit_code
    );

    // CLONE_CHILD_CLEARTID
    if inner.clear_child_tid != 0 {
        let token = process.get_locked_memory_set().token();
        put_data(token, inner.clear_child_tid as *mut u32, 0);
        // 唤醒等待在 child_tid 的进程
        let pa = memory_set
            .translate_va(VirtAddr::from(inner.clear_child_tid))
            .unwrap()
            .0;
        futex_wake_up(pa, 1);
    }
    // 释放futex
    handle_futex_when_exit(
        &inner.robust_list,
        process.get_locked_memory_set().token(),
        task.pid(),
    );
    debug!("exit_current_and_run_next: futex released");
    // 无论如何一个轻量级进程都会是一个线程
    // 释放线程相关资源

    if inner.user_stack_top != 0 {
        memory_set.remove_area_with_start_vpn(
            VirtAddr::from(inner.user_stack_top - USER_STACK_SIZE).floor(),
        );
    }
    memory_set.remove_area_with_start_vpn(VirtAddr::from(inner.trap_cx_bottom).floor());
    inner.task_status = TaskStatus::Zombie;

    drop(inner);

    // 一个进程的所有线程都退出了,此时回收资源
    {
        let tasks = task.process.meta_lock().tasks.clone();
        let tasks: Vec<Arc<TaskControlBlock>> = tasks
            .into_iter()
            .filter_map(|weak| weak.upgrade()) // 自动过滤无效引用
            .collect();
        if tasks.iter().all(|task| task.inner_lock().is_zombie()) {
            send_signal_to_thread_group(task.ppid(), SigSet::SIGCHLD);
            let task_inner = task.inner_lock();

            memory_set.recycle_data_pages();
            task_inner.fd_table.clear();
            task_inner.fs_info.lock().clear();

            let sigtable = process.get_locked_sigtable();
            if !sigtable.is_exited() {
                sigtable.set_exit_code(exit_code);
            }
            remove_proc_dir_and_file(task.pid());
            // wakeup_parent(task.ppid());  // 此功能似乎无用，删去
        }
    }
    // 安全地切换内核栈
    // 复制原子指针，避免释放页
    let tid = task.tid();
    drop(memory_set);
    drop(process);
    drop(task);
    // 启用内核页表，避免task的页表释放后控制流使用不存在的页表
    activate_kernel_space();
    // 将tid传给IDLE控制流，它会负责释放这个线程
    abandon(tid);
}

///Globle process that init user shell
pub static INITPROC: Lazy<Arc<TaskControlBlock>> = Lazy::new(|| {
    let initproc = open("/initproc", OpenFlags::O_RDONLY, NONE_MODE)
        .expect("open initproc error!")
        .file()
        .expect("initproc can not be abs file!");
    let elf_data = initproc.inode.read_all().unwrap();
    let res = TaskControlBlock::new(&elf_data);
    res
});
///Add init process to the manager
pub fn add_initproc() {
    ready_queue::add_task(&INITPROC);

    tid_to_task::insert(0, &INITPROC);
}
///Init PROCESSORS
pub fn init() {
    processor::processors_init();
}
