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
//!
//! # Lock ordering
//!
//! Task/process paths commonly touch PCB metadata, per-thread state, address
//! spaces, signal tables, fd tables, filesystem context, futex queues, and
//! scheduler queues. Code that acquires these locks out of order is considered a
//! bug even if it has not yet reproduced as a deadlock.
//!
//! Resource slots such as `ResourceSlot<MemorySet>` protect only the current
//! `Arc<T>` pointer stored in a process. They are intentionally short-lived:
//! clone the `Arc` with the process accessor, then let the slot lock drop before
//! acquiring any lock inside `T`. Do not expose or hold a resource-slot guard
//! across user-memory access, filesystem/network I/O, signal delivery,
//! scheduling, wakeups, or another resource lock.
//!
//! Normal lock acquisition order:
//!
//! 1. Global tables and scheduler queues.
//! 2. `ProcessMeta`.
//! 3. `TaskControlBlockInner`.
//! 4. Instant resource-slot `get` / `replace` only; never hold it across the
//!    next layers.
//! 5. The remote-TLB `UPDATE_LOCK`, only when the next resource lock is a
//!    `MemorySet` write lock.
//! 6. Resource-internal locks: `MemorySet`, `SigTable`, `FdTable`, `FSInfo`.
//! 7. Child-resource locks such as inode, socket, pipe, futex bucket, and device
//!    locks.
//!
//! Additional rules:
//!
//! - Do not access user memory while holding `ProcessMeta`, `TaskControlBlockInner`,
//!   fd table, signal table, scheduler queue, or futex queue locks. Clone the
//!   needed `Arc`, copy arguments/results, then acquire other locks.
//! - Do not hold `MemorySet` internals while entering filesystem, network,
//!   scheduler, futex, or signal-delivery paths.
//! - Never acquire the remote-TLB `UPDATE_LOCK` while holding any `MemorySet`
//!   read or write guard. MM writers acquire `UPDATE_LOCK` first and then one
//!   `MemorySet`; cross-address-space operations must snapshot one side and
//!   release it before locking the other.
//! - If multiple processes or tasks must be locked at the same time, lock by
//!   increasing pid/tid. Prefer cloning `Arc`s or copying scalar state and
//!   releasing the first lock instead of holding multiple locks.
//! - `SigTable` stores signal actions only. Thread-group exit state and wait
//!   state belong to `ProcessMeta`, not to a shared signal-action table.

#[allow(clippy::module_inception)]
#[allow(rustdoc::private_intra_doc_links)]
mod acct;
mod aux;
mod clone_flags;
mod futex;
#[cfg(feature = "net")]
mod future;
mod kernel_stack;
mod manager;
mod process;
mod processor;
mod rseq;
mod scheduler;
mod seccomp;
mod switch;
mod sysinfo;
mod task;
mod tid;

pub use crate::arch::context::TaskContext;
use crate::{
    arch::cpu::hart_id,
    drivers::cancel_disk_waiter,
    fs::{cancel_ext4_op_waiter, open, OpenFlags, NONE_MODE},
    mm::{
        activate_kernel_space, cancel_cma_lock_owner, copy_to_user, copy_to_user_val, MapAreaType,
        VirtAddr,
    },
    signal::{send_exec_teardown_kill, send_signal_to_thread_group, SigSet},
    syscall::fs::file_lock,
    task::acct::write_process_acct_record,
    task::{kernel_stack::KernelStackOnHeap, processor::abandon},
};
pub(crate) use acct::set_process_acct_file;
use alloc::{boxed::Box, sync::Arc, vec::Vec};
pub use aux::*;
pub use clone_flags::CloneFlags;
pub use futex::*;
#[cfg(feature = "net")]
pub use future::*;
use log::{debug, warn};
pub use manager::*;
pub use process::*;
pub use process::*;
pub(crate) use processor::notify_harts_of_runnable_task;
pub use processor::{
    current_task, current_token, current_trap_cx, run_tasks, schedule, take_current_task,
    Processor, PROCESSORS,
};
pub(crate) use rseq::RseqState;
pub use scheduler::ready_queue;
pub use seccomp::{SeccompAction, SeccompState, SockFilter, SECCOMP_FILTER_MAX_INSNS};
use spin::Lazy;
use switch::__abandon;
pub use sysinfo::Sysinfo;
pub use task::*;
pub use tid::TidHandle;
/// 初始进程的pid
pub const INITPROC_PID: usize = 1;

/// Suspend the current 'Running' task and run the next task in task list.
pub fn suspend_current_and_run_next() {
    // debug!("[suspend_current_and_run_next]!");
    exit_current_if_group_exited_or_killed();
    let task = current_task().unwrap();
    // debug!(
    //     "[suspend_current_and_run_next] strong_count = {}",
    //     Arc::strong_count(&task)
    // );
    let mut task_inner = task.inner_lock();

    let task_cx_ptr = &mut task_inner.task_cx as *mut TaskContext;
    // Change status to Ready, unless blocked by VFORK
    // (VFORK parents stay blocked until child exits or execs).
    if task_inner.task_status != TaskStatus::VforkBlocked {
        task_inner.task_status = TaskStatus::Ready;
    }
    // ---- release current PCB
    drop(task_inner);
    drop(task);
    // jump to scheduling cycle
    schedule(task_cx_ptr);
    // A fatal signal can arrive after this task saves its context and before
    // it is selected again.  Recheck after schedule() returns so cooperative
    // wait loops do not need a second yield to consume SIGKILL.
    exit_current_if_group_exited_or_killed();
}

/// Preempt the current task only when another task is queued for this hart.
/// Timer interrupts otherwise return directly to the interrupted task; this
/// avoids switching through the idle context when there is no scheduling
/// decision to make.  Blocking and sleep paths continue to use
/// `suspend_current_and_run_next` because they must yield even with an empty
/// run queue.
pub fn preempt_current_and_run_next() {
    exit_current_if_group_exited_or_killed();
    let task = current_task().unwrap();
    if task.scheduled_hart() != hart_id() {
        drop(task);
        suspend_current_and_run_next();
        return;
    }
    if !ready_queue::has_ready_for_hart(task.scheduled_hart()) {
        return;
    }
    drop(task);
    suspend_current_and_run_next();
}

/// Yield only when another task is ready for the current hart.
///
/// `sched_yield()` is allowed to return immediately when there is no eligible
/// competitor. Avoiding the ready-queue round trip matters for userspace
/// polling loops that use yield as a backoff hint.
pub fn yield_current_and_run_next() {
    let task = current_task().unwrap();
    if task.scheduled_hart() != hart_id() {
        drop(task);
        suspend_current_and_run_next();
        return;
    }
    if !ready_queue::has_ready_for_hart(task.scheduled_hart()) {
        return;
    }
    drop(task);
    suspend_current_and_run_next();
}

/// Leave the current hart after an affinity change selected another one.
///
/// Called from the RISC-V software-interrupt path.  `schedule()` returns only
/// once this task has been selected on its new placement hart, so the pending
/// user trap can safely complete there.
pub(crate) fn migrate_current_if_needed() {
    let task = current_task().unwrap();
    if task.scheduled_hart() == hart_id() {
        return;
    }
    drop(task);
    suspend_current_and_run_next();
}

/// Exit the current task for a process-wide exit or an unmaskable SIGKILL.
///
/// Keep ProcessMeta and TaskControlBlockInner lock scopes disjoint and drop
/// the current task reference before the normal exit path takes ownership.
pub(crate) fn exit_current_if_group_exited_or_killed() {
    let task = current_task().unwrap();
    let (sigkill_pending, exec_teardown_kill) = {
        let task_inner = task.inner_lock();
        (
            task_inner.sig_pending.contains(SigSet::SIGKILL),
            task_inner.exec_teardown_kill,
        )
    };
    let group_exit_code = {
        let process_meta = task.process.meta_lock();
        process_meta.group_exit_code
    };
    drop(task);

    if let Some(exit_code) = group_exit_code {
        exit_current_and_run_next(exit_code);
    }
    if sigkill_pending {
        // execve marks only its own sibling-cleanup SIGKILL as thread-local.
        // All regular SIGKILL deliveries, including strict seccomp, must
        // initiate the same process-wide termination as other fatal signals.
        if exec_teardown_kill {
            exit_current_and_run_next(137);
        } else {
            exit_current_group_and_run_next(137);
        }
    }
}

pub fn block_current_and_run_next() {
    debug!("[block_current_and_run_next()] BEGIN!");
    let task = current_task().unwrap();
    let mut task_inner = task.inner_lock();
    // A signal may have arrived after the caller's last pending check but
    // before it reached this lock. Keep the current task running in that case.
    if !task_inner
        .sig_pending
        .difference(task_inner.sig_mask)
        .is_empty()
    {
        drop(task_inner);
        drop(task);
        return;
    }
    let task_cx_ptr = &mut task_inner.task_cx as *mut TaskContext;
    task_inner.task_status = TaskStatus::Blocked;
    drop(task_inner);
    drop(task);
    // Keep Processor::current and on_cpu published until switch() has saved
    // this task's context. The scheduler clears on_cpu on the idle stack.
    schedule(task_cx_ptr);
}

pub fn stop_current_and_run_next() {
    debug!("[stop_current_and_run_next()] BEGIN!");
    let task = current_task().unwrap();
    let mut task_inner = task.inner_lock();
    let task_cx_ptr = &mut task_inner.task_cx as *mut TaskContext;
    task_inner.task_status = TaskStatus::Stopped;
    drop(task_inner);
    drop(task);
    schedule(task_cx_ptr);
}

pub fn schedule_blocked_current(task_cx_ptr: *mut TaskContext) {
    // 等待队列已经发布睡眠态。保留 current/on_cpu，直到 switch 完整保存
    // 当前上下文；这是 Linux prepare_task()/finish_task() 的同类约束。
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

/// Collapse a thread group to its execve caller before replacing the shared
/// process image.
///
/// Each sibling must leave through `exit_current_and_run_next()` so it can
/// clear child TIDs, release robust futexes, remove its trap context, and let
/// the switch path reclaim its kernel stack.  Keep only tids across the yield:
/// retaining sibling `Arc`s here would delay that teardown.
pub(crate) fn kill_other_threads_before_exec(current: &TaskControlBlock) {
    let current_tid = current.tid();

    loop {
        let sibling_tids: Vec<usize> = {
            let meta = current.process.meta_lock();
            meta.tasks
                .iter()
                .filter_map(|task| {
                    let task = task.upgrade()?;
                    (task.tid() != current_tid).then_some(task.tid())
                })
                .collect()
        };

        if sibling_tids.is_empty() {
            return;
        }

        for tid in sibling_tids {
            send_exec_teardown_kill(tid);
        }

        // Threads that share one address space are pinned to this process's
        // home hart. Yield without holding metadata so each sibling can take
        // SIGKILL and finish its normal exit path in the old address space.
        suspend_current_and_run_next();
    }
}

/// 杀死当前线程组的所有线程
pub fn exit_current_group_and_run_next(exit_code: i32) {
    debug!("[exit_current_group_and_run_next] exit_code: {}", exit_code);
    let task = current_task().unwrap();
    let mut exit_code = exit_code;

    // Snapshot the task list before taking any sibling TaskControlBlockInner
    // lock. Holding the current task lock while acquiring ProcessMeta reverses
    // the normal ProcessMeta -> TaskControlBlockInner order used by exit/wait.
    let sibling_tasks = task.process.meta_lock().tasks.clone();
    for task_weak in &sibling_tasks {
        if let Some(alive_t) = task_weak.upgrade() {
            if alive_t.tid() == task.tid() {
                continue;
            }
            let mut alive_inner = alive_t.inner_lock();
            if alive_inner.task_status == TaskStatus::Blocked {
                let futex_key = alive_inner.futex_key;
                let futex_pa = alive_inner.futex_pa;
                alive_inner.task_status = TaskStatus::Ready;
                alive_inner.futex_key = 0;
                alive_inner.futex_pa = 0;
                drop(alive_inner);
                // 从 futex 等待队列中移除，防止后续 futex_wake/handle_timer
                // 根据已清理的 futex 字段错误地再次将本任务加入就绪队列
                if futex_key != 0 {
                    let mut waitq = FUTEX_QUEUE_BITMAP.lock();
                    if let Some(queue) = waitq.get_mut(&futex_pa) {
                        if let Some(idx) = queue.iter().position(|x| x.futex_key == futex_key) {
                            queue.remove(idx);
                        }
                    }
                }
                ready_queue::add_task(&alive_t);
            } else {
                drop(alive_inner);
            }
        }
    }
    if task.process.set_group_exit_code_once(exit_code) {
        // 第一个调用的线程
        // 设置线程组退出标志并保存终止代号。
        let pid = task.pid();
        drop(task);
        send_signal_to_thread_group(pid, SigSet::SIGKILL);
    } else {
        exit_code = task.process.group_exit_code();
        drop(task);
    }
    exit_current_and_run_next(exit_code);
}

pub fn exit_current_and_run_next(exit_code: i32) {
    debug!("[exit_current_and_run_next] enter!");
    let curr_task = take_current_task().unwrap();
    // This exit path abandons the kernel stack instead of unwinding it. Clear
    // every task-owned lock/waiter that would otherwise survive its guard.
    cancel_cma_lock_owner(curr_task.tid());
    cancel_ext4_op_waiter(curr_task.tid());
    cancel_disk_waiter(curr_task.tid());
    let count = Arc::strong_count(&curr_task);
    // The current scheduler reference, the global TID table, and this local
    // reference normally account for three strong references under SMP.
    if count > 3 {
        warn!(
            "[exit_current_and_run_next] tid {} exits with extra TCB refs, strong_count = {}",
            curr_task.tid(),
            count
        );
    }
    let curr_proc = &curr_task.process;
    let memory_set = curr_proc.memory_set_arc();
    let fd_table = Arc::clone(&curr_proc.fd_table);
    let fs_info = Arc::clone(&curr_proc.fs_info);
    // Snapshot fields needed by teardown, then release the TCB lock before
    // touching user memory or the address space.  MemorySet updates may wait
    // for remote TLB acknowledgements while interrupts are disabled; keeping
    // this lock held would let a sibling's SIGKILL exit path form an AB-BA
    // cycle on the two harts.
    let (clear_child_tid, robust_list, trap_cx_bottom) = {
        let task_inner = curr_task.inner_lock();
        (
            task_inner.clear_child_tid,
            task_inner.robust_list,
            task_inner.trap_cx_bottom,
        )
    };
    #[cfg(feature = "perf")]
    let vfork_published_at = curr_task.inner_lock().vfork_published_at;
    // debug!(
    //     "[sys_exit] exit_current_and_run_next() -- thread {} exit, exit_code = {}",
    //     curr_task.tid(),
    //     exit_code
    // );

    // CLONE_CHILD_CLEARTID
    if clear_child_tid != 0 {
        let _ = copy_to_user_val(&memory_set, clear_child_tid as *mut u32, &0u32);
        // 唤醒等待在 child_tid 的进程
        // 线程的 clear_child_tid 可能已被用户态 munmap 释放，
        // translate_va 会返回 None，此时跳过 futex_wake 即可。
        if let Some(pa) = memory_set.translate_va(VirtAddr::from(clear_child_tid)) {
            futex_wake_up(pa.0, 1);
        }
    }
    // 释放futex (必须用 tid 而非 pid，因为 futex word 低 30 位存的是 TID)
    {
        handle_futex_when_exit(&robust_list, &memory_set, curr_task.tid());
    }
    // debug!("exit_current_and_run_next: futex released");

    // VFORK: wake up parent if it was suspended waiting for this child
    #[cfg(feature = "perf")]
    let mut vfork_exit_release_at = None;
    if let Some(parent) = Process::get_process_arc_by_pid(curr_task.ppid()) {
        // Do not keep ProcessMeta locked while acquiring a parent task's
        // inner lock; copy the weak list first so the lock scope is explicit.
        let parent_tasks = parent.meta_lock().tasks.clone();
        for task_weak in &parent_tasks {
            if let Some(t) = task_weak.upgrade() {
                let mut parent_inner = t.inner_lock();
                if parent_inner.vfork_wait_child == curr_task.tid() {
                    parent_inner.vfork_wait_child = 0;
                    if parent_inner.task_status == TaskStatus::VforkBlocked {
                        #[cfg(feature = "perf")]
                        {
                            let parent_ready_at = crate::arch::time::get_ticks();
                            parent_inner.vfork_parent_ready_at = parent_ready_at;
                            vfork_exit_release_at = Some(parent_ready_at);
                        }
                        parent_inner.task_status = TaskStatus::Ready;
                        drop(parent_inner);
                        ready_queue::add_task(&t);
                    }
                }
            }
        }
    }
    #[cfg(feature = "perf")]
    if let Some(parent_ready_at) = vfork_exit_release_at {
        crate::utils::perf::record_vfork_release_exit();
        if vfork_published_at != 0 {
            crate::utils::perf::record_vfork_child_to_exit_duration(
                parent_ready_at.saturating_sub(vfork_published_at),
            );
        }
    }

    // 无论如何一个轻量级进程都会是一个线程
    // 释放线程相关资源
    memory_set.remove_area_with_start_vpn(VirtAddr::from(trap_cx_bottom).floor());
    {
        let mut task_inner = curr_task.inner_lock();
        task_inner.task_status = TaskStatus::Zombie;
    }
    let curr_tid = curr_task.tid();

    curr_task.process.meta_lock().tasks.retain(|weak| {
        weak.upgrade()
            .map(|task| task.tid() != curr_tid)
            .unwrap_or(false)
    });

    // 唤醒被阻塞的兄弟线程，防止它们因等待本线程清理资源而永久死锁
    {
        let bro_tasks = curr_task.process.meta_lock().tasks.clone();
        let bro_tasks: Vec<Arc<TaskControlBlock>> = bro_tasks
            .into_iter()
            .filter_map(|weak| weak.upgrade())
            .collect();
        for bro_task in &bro_tasks {
            if bro_task.tid() != curr_tid {
                let mut bro_inner = bro_task.inner_lock();
                if bro_inner.task_status == TaskStatus::Blocked {
                    let futex_key = bro_inner.futex_key;
                    let futex_pa = bro_inner.futex_pa;
                    bro_inner.task_status = TaskStatus::Ready;
                    bro_inner.futex_key = 0;
                    bro_inner.futex_pa = 0;
                    drop(bro_inner);
                    // 从 futex 等待队列中移除
                    if futex_key != 0 {
                        let mut waitq = FUTEX_QUEUE_BITMAP.lock();
                        if let Some(queue) = waitq.get_mut(&futex_pa) {
                            if let Some(idx) = queue.iter().position(|x| x.futex_key == futex_key) {
                                queue.remove(idx);
                            }
                        }
                    }
                    ready_queue::add_task(bro_task);
                }
            }
        }
    }

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
            debug!(
                "[exit] pid {}: all tasks zombie, calling exit_and_reparent",
                curr_task.pid()
            );
            let mut usage = ProcessUsage {
                maxrss: memory_set.resident_size_kb(),
                ..ProcessUsage::default()
            };
            let time_data = curr_task.inner_lock().time_data.clone();
            usage.utime += time_data.utime;
            usage.stime += time_data.stime;
            usage.cutime += time_data.cutime;
            usage.cstime += time_data.cstime;
            usage.cmaxrss = usage.cmaxrss.max(time_data.cmaxrss);
            for task in &bro_tasks {
                let time_data = task.inner_lock().time_data.clone();
                usage.utime += time_data.utime;
                usage.stime += time_data.stime;
                usage.cutime += time_data.cutime;
                usage.cstime += time_data.cstime;
                usage.cmaxrss = usage.cmaxrss.max(time_data.cmaxrss);
            }
            write_process_acct_record(&curr_task, exit_code, &usage);
            curr_task.process.meta_lock().usage = usage;
            // The process page table is still active on this hart.  Reclaiming
            // its page-table frames first leaves satp pointing at a freed root
            // page and lets subsequent allocations corrupt the live address
            // translation state.  This task cannot return to user mode after
            // becoming a zombie, so switch to the kernel page table before the
            // eager address-space teardown.
            activate_kernel_space();
            if Arc::strong_count(&memory_set) == 2 {
                if let Err(error) = memory_set.recycle_data_pages() {
                    warn!(
                        "process {} shared mmap writeback failed during teardown: {:?}",
                        curr_task.pid(),
                        error
                    );
                }
            }
            file_lock::release_posix_locks_by_owner(curr_task.pid() as i32);
            file_lock::release_file_leases_by_owner(curr_task.pid() as i32);
            // CLONE_FILES / CLONE_FS share these resources across processes.
            // A zombie still keeps an Arc through its Process metadata, so
            // Arc::strong_count cannot determine whether a live process
            // remains.  The resource owner counts are decremented exactly
            // once when each process group exits.  VFS lookup caches are
            // global and capacity-bounded, so they intentionally survive a
            // short-lived compiler worker and remain reusable by the next one.
            fd_table.release_owner();
            fs_info.release_owner();

            curr_task.process.set_group_exit_code_once(exit_code);
            curr_task.process.exit_and_reparent();
            // 仅当创建时指定了 SIGCHLD 才通知父进程（对应 Linux exit_signal）
            let exit_signal = curr_task.process.meta_lock().exit_signal;
            if exit_signal >= 0 {
                send_signal_to_thread_group(
                    curr_task.ppid(),
                    SigSet::from_sig(exit_signal as usize),
                );
            }
            // 唤醒在 waitpid 上等待的父进程（无论是否有 exit_signal，父进程都可能通过 __WALL 等待）
            if let Some(parent) = Process::get_process_arc_by_pid(curr_task.ppid()) {
                parent.meta_lock().child_exit_event.wake();
            }
        } else {
            debug!(
                "[exit] pid {}: NOT all tasks zombie, bro_tasks count: {}",
                curr_task.pid(),
                bro_tasks.len()
            );
        }
    }
    // 安全地切换内核栈
    let tid = curr_task.tid();
    drop(memory_set);
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
