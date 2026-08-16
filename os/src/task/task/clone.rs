//! Process and thread creation.
//!
//! `clone_process` deliberately separates parent-state collection from child
//! construction. The boundary is part of its deadlock-avoidance contract: no
//! parent task lock is held while allocating a memory set or publishing the
//! child to process and scheduler-visible state.

use super::super::{scheduler::SchedEntity, tid_to_task, RseqState, TaskContext, TidHandle};
use super::{RobustListHead, TaskControlBlock, TaskControlBlockInner, TaskStatus};
#[cfg(feature = "perf")]
use crate::arch::time::get_ticks;
#[cfg(feature = "fault-diagnostics")]
use crate::signal::SignalFrameTrace;
use crate::{
    fs::{create_proc_dir, FSInfo, FdTable},
    mm::{copy_to_user_val, MapAreaType, MemorySet, MemorySetInner},
    signal::{SigSet, SigTable, SignalStack, SIG_MAX_NUM},
    sync::RemoteTlbMutex,
    syscall::MmapFlags,
    task::{kernel_stack::KernelStackOnHeap, CloneFlags, Process},
    timer::{TimeData, Timer},
    utils::SysErrNo,
};
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicUsize};
use futures_util::task::AtomicWaker;
use log::debug;
use spin::Mutex;

impl TaskControlBlock {
    /// Create a process or thread according to Linux clone flags.
    ///
    /// The operation is split in two phases: collect the parent state while
    /// holding only short-lived parent locks, then create and publish the
    /// child after those locks have been released.
    pub fn clone_process(
        self: &Arc<TaskControlBlock>,
        flags: CloneFlags,
        exit_signal: i32,
        stack: usize,
        parent_tid: *mut u32,
        tls: usize,
        child_tid: *mut u32,
    ) -> Result<Arc<TaskControlBlock>, SysErrNo> {
        #[cfg(feature = "perf")]
        let clone_process_begin = get_ticks();
        let tid_handle = TidHandle::alloc().unwrap();
        let kernel_stack = KernelStackOnHeap::new();
        let kernel_stack_top = kernel_stack.top();
        debug!("TCB::new kstack top = {:#x}", kernel_stack_top);

        // Phase 1: snapshot parent process metadata and task state. The exit
        // path takes ProcessMeta before TaskControlBlockInner, so do not take
        // ProcessMeta while holding the latter here.
        let (
            child_memory_set_arc,
            child_fs_info,
            child_fd_table,
            child_sig_table,
            child_pid,
            child_ppid,
            child_timer,
            child_sig_mask,
            child_alt_signal_stack,
            clear_child_tid,
            parent_memory_set_arc,
            parent_trap_cx,
            parent_heappoint,
            parent_heapbottom,
            parent_user_id,
            parent_euid,
            parent_suid,
            parent_rgid,
            parent_egid,
            parent_sgid,
            parent_capabilities,
            parent_nice,
            parent_rseq,
            parent_no_new_privs,
            parent_seccomp_state,
            parent_mce_kill_policy,
            parent_timer_slack_ns,
            parent_comm,
            parent_pgid,
            parent_sid,
        );
        let parent_pid;
        {
            let parent_meta = self.process.meta_lock();
            parent_pid = parent_meta.parent_pid;
            parent_pgid = parent_meta.pgid;
            parent_sid = parent_meta.sid;
            parent_comm = parent_meta.comm.clone();
        }
        #[cfg(feature = "perf")]
        crate::utils::perf::record_clone_bootstrap_duration(
            get_ticks().saturating_sub(clone_process_begin),
        );

        // Snapshot the resource-slot Arc while holding TaskControlBlockInner.
        // This does not acquire a MemorySet-internal lock.
        #[cfg(feature = "perf")]
        let parent_state_begin = get_ticks();
        {
            let parent_inner = self.inner.lock();
            parent_memory_set_arc = self.process.memory_set_arc();

            clear_child_tid = if flags.contains(CloneFlags::CLONE_CHILD_CLEARTID) {
                child_tid as usize
            } else {
                0
            };

            if flags.contains(CloneFlags::CLONE_THREAD) {
                child_pid = self.pid();
                child_ppid = parent_pid;
                child_timer = Arc::clone(&parent_inner.timer);
                child_sig_mask = parent_inner.sig_mask;
            } else {
                child_pid = tid_handle.0;
                child_ppid = if flags.contains(CloneFlags::CLONE_PARENT) {
                    parent_pid
                } else {
                    self.pid()
                };
                child_timer = Arc::new(Timer::new());
                child_sig_mask = parent_inner.sig_mask;
            }
            // Linux clears the alternate stack for clone(CLONE_VM) threads,
            // except the CLONE_VM | CLONE_VFORK exec hand-off case.
            child_alt_signal_stack = if flags.contains(CloneFlags::CLONE_VM)
                && !flags.contains(CloneFlags::CLONE_VFORK)
            {
                SignalStack::disabled()
            } else {
                parent_inner.alt_signal_stack
            };

            parent_trap_cx = *parent_inner.trap_cx();
            parent_heappoint = parent_inner.user_heappoint;
            parent_heapbottom = parent_inner.user_heapbottom;
            parent_user_id = parent_inner.user_id;
            parent_euid = parent_inner.effective_uid;
            parent_suid = parent_inner.saved_uid;
            parent_rgid = parent_inner.real_gid;
            parent_egid = parent_inner.effective_gid;
            parent_sgid = parent_inner.saved_gid;
            parent_capabilities = parent_inner.capabilities;
            parent_nice = parent_inner.nice;
            parent_no_new_privs = parent_inner.no_new_privs;
            parent_seccomp_state = parent_inner.seccomp_state.clone();
            parent_mce_kill_policy = parent_inner.mce_kill_policy;
            parent_timer_slack_ns = parent_inner.timer_slack_ns;
            // Linux inherits rseq on fork but clears it for CLONE_VM, whose
            // child gets a distinct thread-local rseq ABI area.
            parent_rseq = if flags.contains(CloneFlags::CLONE_VM) {
                RseqState::default()
            } else {
                parent_inner.rseq
            };
        }
        #[cfg(feature = "perf")]
        crate::utils::perf::record_clone_parent_state_duration(
            get_ticks().saturating_sub(parent_state_begin),
        );

        #[cfg(feature = "perf")]
        let address_space_start = get_ticks();
        // Do not hold TaskControlBlockInner while taking MemorySet's write
        // lock. Pre-faulting a shared mapping can enter ext4 and block.
        child_memory_set_arc = if flags.contains(CloneFlags::CLONE_VM) {
            Arc::clone(&parent_memory_set_arc)
        } else {
            Arc::new(MemorySet::new(MemorySetInner::from_existed_user(
                &parent_memory_set_arc,
            )))
        };
        #[cfg(feature = "perf")]
        crate::utils::perf::record_clone_address_space_duration(
            get_ticks().saturating_sub(address_space_start),
        );

        child_fs_info = if flags.contains(CloneFlags::CLONE_FS) {
            Arc::clone(&self.process.fs_info)
        } else {
            Arc::new(FSInfo::from_another(&self.process.fs_info))
        };
        child_fd_table = if flags.contains(CloneFlags::CLONE_FILES) {
            Arc::clone(&self.process.fd_table)
        } else {
            Arc::new(FdTable::from_another(&self.process.fd_table))
        };
        child_sig_table = if flags.contains(CloneFlags::CLONE_SIGHAND) {
            self.process.sig_table_arc()
        } else if flags.contains(CloneFlags::CLONE_CLEAR_SIGHAND) {
            Arc::new(Mutex::new(SigTable::new()))
        } else {
            Arc::new(Mutex::new(
                self.process
                    .with_sigtable(|sigtable| SigTable::from_another(sigtable)),
            ))
        };

        // CLONE_PARENT_SETTID accesses user memory, so it remains outside the
        // parent PCB lock.
        if flags.contains(CloneFlags::CLONE_PARENT_SETTID) {
            copy_to_user_val(&*parent_memory_set_arc, parent_tid, &(tid_handle.0 as u32))?;
        }

        // Process::new() registers the parent relationship and takes
        // ProcessMeta, which is safe only after the parent task lock is gone.
        #[cfg(feature = "perf")]
        let process_create_begin = get_ticks();
        let process_arc = if flags.contains(CloneFlags::CLONE_THREAD) {
            self.process.clone()
        } else if flags.contains(CloneFlags::CLONE_VM) && !flags.contains(CloneFlags::CLONE_VFORK) {
            // A regular CLONE_VM child starts on the parent's hart for cache
            // locality. The task remains movable across all online harts.
            Process::new_on_hart(
                child_memory_set_arc.clone(),
                child_sig_table.clone(),
                child_fd_table,
                child_fs_info,
                child_pid,
                child_ppid,
                parent_pgid,
                parent_sid,
                self.process.home_hart(),
            )
        } else {
            Process::new(
                child_memory_set_arc.clone(),
                child_sig_table.clone(),
                child_fd_table,
                child_fs_info,
                child_pid,
                child_ppid,
                parent_pgid,
                parent_sid,
            )
        };
        #[cfg(feature = "perf")]
        crate::utils::perf::record_clone_process_create_duration(
            get_ticks().saturating_sub(process_create_begin),
        );

        // Phase 2: construct the child without a parent task lock.
        #[cfg(feature = "perf")]
        let task_setup_begin = get_ticks();
        process_arc.meta_lock().comm = parent_comm;

        let (child_cpu_affinity, child_scheduled_hart) = if flags.contains(CloneFlags::CLONE_THREAD)
        {
            let affinity = self.cpu_affinity();
            (
                affinity,
                Self::choose_hart(affinity, self.scheduled_hart().wrapping_add(1)),
            )
        } else {
            let home_hart = process_arc.home_hart();
            (Self::default_cpu_affinity(home_hart), home_hart)
        };

        let child = Arc::new(TaskControlBlock {
            tid: tid_handle,
            kernel_stack,
            process: process_arc,
            cpu_affinity: AtomicUsize::new(child_cpu_affinity),
            scheduled_hart: AtomicUsize::new(child_scheduled_hart),
            on_cpu: AtomicBool::new(false),
            #[cfg(feature = "perf")]
            ext4_resource_lock_counts: [const { AtomicUsize::new(0) }; 9],
            interrupted: AtomicBool::new(false),
            interrupt_waker: AtomicWaker::new(),
            // First enqueue places the child in its destination hart's
            // min_vruntime coordinate system.
            sched_entity: SchedEntity::new(),
            inner: RemoteTlbMutex::new(TaskControlBlockInner {
                trap_cx_ppn: 0.into(),
                trap_cx_bottom: 0,
                task_cx: TaskContext::goto_trap_return(kernel_stack_top),
                task_status: TaskStatus::Ready,
                time_data: TimeData::new(),
                user_heappoint: parent_heappoint,
                user_heapbottom: parent_heapbottom,
                clear_child_tid,
                vfork_wait_child: 0,
                present_page_fault_retry: None,
                #[cfg(feature = "perf")]
                vfork_published_at: 0,
                #[cfg(feature = "perf")]
                vfork_exec_started_at: 0,
                #[cfg(feature = "perf")]
                vfork_parent_ready_at: 0,
                sig_mask: child_sig_mask,
                sigsuspend_restore_mask: None,
                alt_signal_stack: child_alt_signal_stack,
                #[cfg(feature = "fault-diagnostics")]
                signal_frame_trace: SignalFrameTrace::new(),
                sig_pending: SigSet::empty(),
                sig_pending_info: [None; SIG_MAX_NUM + 1],
                exec_teardown_kill: false,
                rseq: parent_rseq,
                rseq_pending: parent_rseq != RseqState::default(),
                timer: child_timer,
                robust_list: RobustListHead::default(),
                user_id: parent_user_id,
                effective_uid: parent_euid,
                saved_uid: parent_suid,
                real_gid: parent_rgid,
                effective_gid: parent_egid,
                saved_gid: parent_sgid,
                capabilities: parent_capabilities,
                pdeath_signal: 0,
                no_new_privs: parent_no_new_privs,
                seccomp_state: parent_seccomp_state,
                mce_kill_policy: parent_mce_kill_policy,
                timer_slack_ns: parent_timer_slack_ns,
                futex_pa: 0,
                futex_key: 0,
                futex_timedout: false,
                sig_eintr: false,
                sigtimedwait_timedout: false,
                nice: parent_nice,
            }),
        });

        {
            let mut child_meta = child.process.meta_lock();
            child_meta.tasks.retain(|weak| weak.upgrade().is_some());
            child_meta.tasks.push(Arc::downgrade(&child));
        }

        let mut child_inner = child.inner_lock();
        if flags.contains(CloneFlags::CLONE_VM) {
            if stack != 0 {
                child.alloc_trap_context_only(&mut child_inner);
            } else {
                child.alloc_user_res(&mut child_inner);
            }
            *child_inner.trap_cx() = parent_trap_cx;
            child_inner.trap_cx().set_a0(0);
        } else {
            // fork: clone the user stack into the child address space.
            child.alloc_user_res(&mut child_inner);
            *child_inner.trap_cx() = parent_trap_cx;

            let child_mm = child.process.memory_set_arc();
            let child_stack_bottom = child_mm
                .get_ref()
                .areas
                .iter()
                .find(|area| {
                    area.area_type == MapAreaType::Stack
                        && !area.mmap_flags.contains(MmapFlags::MAP_STACK)
                })
                .map(|area| area.vpn_range.start())
                .expect("fork: child has no Stack area");
            child_mm.lazy_clone_area(child_stack_bottom, &parent_memory_set_arc);
            child_inner.trap_cx().set_a0(0);
        }

        let trap_cx = child_inner.trap_cx();
        trap_cx.kernel_stack = kernel_stack_top;
        if stack != 0 {
            trap_cx.set_sp(stack);
        }
        if flags.contains(CloneFlags::CLONE_SETTLS) {
            trap_cx.set_tp(tls);
        }
        drop(child_inner);

        if flags.contains(CloneFlags::CLONE_CHILD_SETTID) {
            let child_mem = child.process.memory_set_arc();
            copy_to_user_val(&*child_mem, child_tid, &(child.tid() as u32))?;
        }

        if !flags.contains(CloneFlags::CLONE_THREAD) {
            let mut child_meta = child.process.meta_lock();
            child_meta.exit_signal = exit_signal;
            debug!(
                "[clone_process] fork pid={}, flags={:?}, exit_signal={}",
                child_pid, flags, child_meta.exit_signal
            );
        }
        #[cfg(feature = "perf")]
        crate::utils::perf::record_clone_task_setup_duration(
            get_ticks().saturating_sub(task_setup_begin),
        );

        if !flags.contains(CloneFlags::CLONE_THREAD) {
            #[cfg(feature = "perf")]
            let procfs_start = get_ticks();
            let _ = create_proc_dir(child_pid);
            #[cfg(feature = "perf")]
            crate::utils::perf::record_clone_procfs_register_duration(
                get_ticks().saturating_sub(procfs_start),
            );
        }

        // Publish only after the child is fully initialised. A vfork parent is
        // marked blocked before its child can become visible to the scheduler.
        #[cfg(feature = "perf")]
        let publish_begin = get_ticks();
        {
            let mut parent_inner = self.inner_lock();
            if flags.contains(CloneFlags::CLONE_VFORK) {
                parent_inner.vfork_wait_child = child.tid();
                #[cfg(feature = "perf")]
                {
                    parent_inner.vfork_parent_ready_at = 0;
                }
                parent_inner.task_status = TaskStatus::VforkBlocked;
            }
        }
        #[cfg(feature = "perf")]
        if flags.contains(CloneFlags::CLONE_VFORK) {
            child.inner_lock().vfork_published_at = get_ticks();
        }
        tid_to_task::insert(child.tid(), &child);
        if !flags.contains(CloneFlags::CLONE_THREAD) {
            if flags.contains(CloneFlags::CLONE_FILES) {
                child.process.fd_table.acquire_owner();
            }
            if flags.contains(CloneFlags::CLONE_FS) {
                child.process.fs_info.acquire_owner();
            }
        }
        #[cfg(feature = "perf")]
        crate::utils::perf::record_clone_publish_duration(
            get_ticks().saturating_sub(publish_begin),
        );
        #[cfg(feature = "perf")]
        crate::utils::perf::record_clone_process_total_duration(
            get_ticks().saturating_sub(clone_process_begin),
        );
        Ok(child.clone())
    }
}
