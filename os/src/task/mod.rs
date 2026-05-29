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
#[cfg(feature = "net")]
mod future;
mod kernel_stack;
mod manager;
mod process;
mod processor;
mod switch;
mod sysinfo;
mod task;
mod tid;

pub use crate::arch::context::TaskContext;
use crate::{
    arch::{cpu::hart_id, memory_layout::USER_STACK_SIZE},
    fs::{open, remove_proc_dir_and_file, OpenFlags, NONE_MODE},
    mm::{activate_kernel_space, copy_to_user, get_data, put_data, VirtAddr},
    signal::{send_signal_to_thread_group, SigSet},
    task::{kernel_stack::KernelStackOnHeap, processor::abandon},
};
use alloc::{boxed::Box, sync::Arc, vec::Vec};
pub use aux::*;
pub use futex::*;
#[cfg(feature = "net")]
pub use future::*;
use log::{debug, error};
pub use manager::*;
pub use process::*;
pub use process::*;
pub use processor::{
    current_task, current_token, current_trap_cx, run_tasks, schedule, take_current_task,
    Processor, PROCESSORS,
};
use spin::Lazy;
use switch::__abandon;
pub use sysinfo::Sysinfo;
pub use task::*;
pub use tid::TidHandle;
/// 初始进程的pid
pub const INITPROC_PID: usize = 1;

/// Suspend the current 'Running' task and run the next task in task list.
pub fn suspend_current_and_run_next() {
    let task = current_task().unwrap();
    // debug!(
    //     "[suspend_current_and_run_next] strong_count = {}",
    //     Arc::strong_count(&task)
    // );
    let mut task_inner = task.inner_lock();
    let exited = {
        let proc_inner = task.process.inner_lock();
        let sig_table = proc_inner.get_locked_sigtable();
        sig_table.is_exited()
    };

    if exited {
        let exit_code = task.process.inner_lock().get_locked_sigtable().exit_code();
        drop(task_inner);
        drop(task);
        exit_current_and_run_next(exit_code);
    } else {
        let task_cx_ptr = &mut task_inner.task_cx as *mut TaskContext;
        // Change status to Ready
        task_inner.task_status = TaskStatus::Ready;
        // ---- release current PCB
        drop(task_inner);
        drop(task);
        // jump to scheduling cycle
        schedule(task_cx_ptr);
    }
}

pub fn block_current_and_run_next() {
    debug!("[block_current_and_run_next()] BEGIN!");
    let task = take_current_task().unwrap();
    // debug!("current strong_count: {}", Arc::strong_count(&task));
    let mut task_inner = task.inner_lock();
    let task_cx_ptr = &mut task_inner.task_cx as *mut TaskContext;
    task_inner.task_status = TaskStatus::Blocked;
    drop(task_inner);
    drop(task);
    // error!("schedule() BEGIN!");
    schedule(task_cx_ptr);
}

pub fn schedule_blocked_current(task_cx_ptr: *mut TaskContext) {
    // 等待队列已经把当前任务置为 Blocked，这里只负责切回调度器。
    let task = take_current_task().unwrap();
    drop(task);
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
    debug!("[exit_current_group_and_run_next] exit_code: {}", exit_code);
    let task = current_task().unwrap();
    let task_inner = task.inner_lock();
    let mut exit_code = exit_code;
    let process = task.process.inner_lock();
    let sigtable = process.get_locked_sigtable();

    for bro_tasks in &task.process.meta_lock().tasks {
        if let Some(alive_t) = bro_tasks.upgrade() {
            if alive_t.tid() == task.tid() {
                continue;
            }
            let mut alive_inner = alive_t.inner_lock();
            if alive_inner.task_status == TaskStatus::Blocked {
                alive_inner.task_status = TaskStatus::Ready;
                ready_queue::add_task(&alive_t);
            }
            drop(alive_inner);
        }
    }
    if sigtable.not_exited() {
        // 第一个调用的线程
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
    debug!("[exit_current_and_run_next] enter!");
    let curr_task = take_current_task().unwrap();
    let count = Arc::strong_count(&curr_task);
    if count > 2 {
        // 一份是进程调度器里面的，一份是tid2task里面的, 大于二直接死循环，不如直接panic
        error!("WRONG STRONG COUNT!!!");
        panic!(
            "Someone take a reference to the TCB!, strong_count = {}",
            count
        );
    }
    let curr_proc = curr_task.process.inner_lock();
    let memory_set = curr_proc.get_locked_memory_set_read();
    let mut curr_task_inner = curr_task.inner_lock();
    // debug!(
    //     "[sys_exit] exit_current_and_run_next() -- thread {} exit, exit_code = {}",
    //     curr_task.tid(),
    //     exit_code
    // );

    // CLONE_CHILD_CLEARTID
    if curr_task_inner.clear_child_tid != 0 {
        let memory_set = curr_proc.get_locked_memory_set_read();
        // put_data(token, curr_task_inner.clear_child_tid as *mut u32, 0);
        copy_to_user(&memory_set, curr_task_inner.clear_child_tid as usize, &[0]);
        // 唤醒等待在 child_tid 的进程
        let pa = memory_set
            .translate_va(VirtAddr::from(curr_task_inner.clear_child_tid))
            .unwrap()
            .0;
        futex_wake_up(pa, 1); // 唤醒在 clear_child_tid 等待的线程
    }
    // 释放futex
    handle_futex_when_exit(
        &curr_task_inner.robust_list,
        curr_proc.get_locked_memory_set_read().token(),
        curr_task.pid(),
    );
    // debug!("exit_current_and_run_next: futex released");
    // 无论如何一个轻量级进程都会是一个线程
    // 释放线程相关资源

    if curr_task_inner.user_stack_top != 0 {
        memory_set.remove_area_with_start_vpn(
            VirtAddr::from(curr_task_inner.user_stack_top - USER_STACK_SIZE).floor(),
        );
    }
    memory_set.remove_area_with_start_vpn(VirtAddr::from(curr_task_inner.trap_cx_bottom).floor());
    curr_task_inner.task_status = TaskStatus::Zombie;
    drop(curr_task_inner);

    // 一个进程的所有线程都退出了,此时回收资源
    {
        let bro_tasks = curr_task.process.meta_lock().tasks.clone();
        let bro_tasks: Vec<Arc<TaskControlBlock>> = bro_tasks
            .into_iter()
            .filter_map(|weak| weak.upgrade()) // 自动过滤无效引用
            .collect();
        // bro_tasks.iter().for_each(|t: &Arc<TaskControlBlock>|debug!("My bro_task is {}, status is {:?}.", t.tid(), t.inner_lock().task_status));
        if bro_tasks
            .iter()
            .all(|bro_task| bro_task.inner_lock().is_zombie())
        {
            memory_set.recycle_data_pages();
            curr_proc.fd_table.clear();
            curr_proc.fs_info.clear();

            let sigtable = curr_proc.get_locked_sigtable();
            if !sigtable.is_exited() {
                sigtable.set_exit_code(exit_code);
            }
            curr_task.process.exit_and_reparent();
            remove_proc_dir_and_file(curr_task.pid());
            send_signal_to_thread_group(curr_task.ppid(), SigSet::SIGCHLD);
            // 唤醒在 waitpid 上等待的父进程
            if let Some(parent) = Process::get_process_arc_by_pid(curr_task.ppid()) {
                parent.meta_lock().child_exit_event.wake();
            }
        }
    }
    // 安全地切换内核栈
    let tid = curr_task.tid();
    drop(memory_set);
    drop(curr_proc);
    drop(curr_task);
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

    tid_to_task::insert(INITPROC.tid(), &INITPROC);
}
///Init PROCESSORS
pub fn init() {
    processor::processors_init();
}
