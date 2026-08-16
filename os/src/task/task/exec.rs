//! `execve` image replacement and initial user-stack construction.
//!
//! This module owns all transient state used while replacing a task image.
//! Keeping it outside the task control-block definition makes the commit
//! sequence and its lock-order requirements easier to audit.

use super::{RobustListHead, TaskControlBlock, TaskStatus};
#[cfg(feature = "perf")]
use crate::arch::time::get_ticks;
#[cfg(feature = "fault-diagnostics")]
use crate::signal::SignalFrameTrace;
use crate::{
    arch::{
        context::TrapContext,
        memory_layout::{
            PAGE_SIZE, PRE_ALLOC_PAGES, USER_STACK_SIZE, USER_STACK_TOP, USER_TRAP_CONTEXT_TOP,
        },
        page_table::PageTable,
    },
    fs::OSFile,
    mm::{
        copy_to_user, copy_to_user_val, MapAreaType, MapPermission, MemorySet, MemorySetInner,
        PhysPageNum, VirtAddr,
    },
    signal::{SigSet, SigTable, SignalStack},
    task::{futex::futex_wake_up, Process, RseqState},
    utils::SysErrNo,
};
use alloc::{string::String, sync::Arc, vec::Vec};
use core::mem::size_of;
use log::{debug, error};

use super::super::aux::{Aux, AuxType};

fn task_comm_from_argv0(argv0: &[u8]) -> String {
    let mut name = argv0;
    while name.last() == Some(&b'/') {
        name = &name[..name.len() - 1];
    }
    let name = name.rsplit(|byte| *byte == b'/').next().unwrap_or(name);
    let mut comm = String::new();
    if let Ok(name) = core::str::from_utf8(name) {
        for ch in name.chars().take(16) {
            comm.push(ch);
        }
    }
    if comm.is_empty() {
        String::from("?")
    } else {
        comm
    }
}

const EXEC_STACK_LAYOUT_SLACK: usize = 64;

fn checked_exec_stack_add(total: &mut usize, bytes: usize) -> Result<(), SysErrNo> {
    *total = total.checked_add(bytes).ok_or(SysErrNo::E2BIG)?;
    Ok(())
}

/// Reject an exec image whose initial argv/envp stack cannot fit before the
/// address space is replaced. The slack covers the random bytes and all
/// alignment/padding steps in `prepare_exec_stack` below.
fn validate_exec_stack_layout(
    argv: &[Vec<u8>],
    env: &[Vec<u8>],
    elf_auxv_count: usize,
) -> Result<(), SysErrNo> {
    let mut required = 0;
    for value in argv.iter().chain(env.iter()) {
        checked_exec_stack_add(
            &mut required,
            value.len().checked_add(1).ok_or(SysErrNo::E2BIG)?,
        )?;
    }

    // argv/envp each have a trailing NULL, and argc occupies one word.
    let pointer_words = argv
        .len()
        .checked_add(env.len())
        .and_then(|count| count.checked_add(3))
        .ok_or(SysErrNo::E2BIG)?;
    checked_exec_stack_add(
        &mut required,
        pointer_words
            .checked_mul(size_of::<usize>())
            .ok_or(SysErrNo::E2BIG)?,
    )?;

    // exec appends AT_RANDOM, AT_EXECFN, and AT_NULL to the ELF auxiliary vector.
    let aux_entries = elf_auxv_count.checked_add(3).ok_or(SysErrNo::E2BIG)?;
    checked_exec_stack_add(
        &mut required,
        aux_entries
            .checked_mul(size_of::<Aux>())
            .ok_or(SysErrNo::E2BIG)?,
    )?;
    checked_exec_stack_add(&mut required, EXEC_STACK_LAYOUT_SLACK)?;

    if required > USER_STACK_SIZE {
        return Err(SysErrNo::E2BIG);
    }
    Ok(())
}

fn checked_exec_stack_sub(user_sp: &mut usize, bytes: usize) -> Result<usize, SysErrNo> {
    *user_sp = user_sp.checked_sub(bytes).ok_or(SysErrNo::E2BIG)?;
    Ok(*user_sp)
}

pub(super) fn alloc_user_res_in_memory_set(
    memory_set: &MemorySet,
) -> Result<(usize, usize, PhysPageNum), SysErrNo> {
    memory_set.with_frame_preserving_mut(|ms| {
        let (u_bottom, u_top) = ms.lazy_insert_framed_area_with_hint(
            USER_STACK_TOP,
            USER_STACK_SIZE,
            MapPermission::R | MapPermission::W | MapPermission::U,
            MapAreaType::Stack,
        );
        let (trap_cx_bottom, _) = ms.insert_framed_area_with_hint(
            USER_TRAP_CONTEXT_TOP,
            PAGE_SIZE,
            MapPermission::R | MapPermission::W,
            MapAreaType::Trap,
        );
        let trap_cx_ppn = ms
            .translate(VirtAddr::from(trap_cx_bottom).floor())
            .ok_or(SysErrNo::ENOMEM)?;

        let stack_range = (
            VirtAddr::from(u_bottom).floor(),
            VirtAddr::from(u_top).floor(),
        );
        let area_idx = ms
            .areas
            .iter()
            .position(|area| area.vpn_range.range() == stack_range)
            .ok_or(SysErrNo::ENOMEM)?;
        let stack_end = ms.areas[area_idx].vpn_range.end().0;
        let (page_table, areas) = (&mut ms.page_table, &mut ms.areas);
        let area = &mut areas[area_idx];
        for i in 1..=PRE_ALLOC_PAGES {
            let vpn = (stack_end - i).into();
            if page_table.translate(vpn).is_none() && area.map_one(page_table, vpn).is_none() {
                return Err(SysErrNo::ENOMEM);
            }
        }

        Ok((u_top, trap_cx_bottom, trap_cx_ppn))
    })
}

fn prepare_exec_stack(
    memory_set: &MemorySet,
    ustack_top: usize,
    argv: &[Vec<u8>],
    env: &[Vec<u8>],
    auxv: &mut Vec<Aux>,
) -> Result<(usize, usize, usize), SysErrNo> {
    let mut envp = Vec::new();
    envp.try_reserve(env.len().checked_add(1).ok_or(SysErrNo::E2BIG)?)
        .map_err(|_| SysErrNo::ENOMEM)?;
    let mut argvp = Vec::new();
    argvp
        .try_reserve(argv.len().checked_add(1).ok_or(SysErrNo::E2BIG)?)
        .map_err(|_| SysErrNo::ENOMEM)?;
    auxv.try_reserve(3).map_err(|_| SysErrNo::ENOMEM)?;

    let mut user_sp = ustack_top;
    for value in env {
        let value_len = value.len().checked_add(1).ok_or(SysErrNo::E2BIG)?;
        let value_sp = checked_exec_stack_sub(&mut user_sp, value_len)?;
        envp.push(value_sp);
        copy_to_user(memory_set, value_sp, value)?;
        copy_to_user(
            memory_set,
            value_sp.checked_add(value.len()).ok_or(SysErrNo::E2BIG)?,
            &[0],
        )?;
    }
    envp.push(0);
    user_sp -= user_sp % size_of::<usize>();

    for value in argv {
        let value_len = value.len().checked_add(1).ok_or(SysErrNo::E2BIG)?;
        let value_sp = checked_exec_stack_sub(&mut user_sp, value_len)?;
        argvp.push(value_sp);
        copy_to_user(memory_set, value_sp, value)?;
        copy_to_user(
            memory_set,
            value_sp.checked_add(value.len()).ok_or(SysErrNo::E2BIG)?,
            &[0],
        )?;
    }
    user_sp -= user_sp % size_of::<usize>();
    argvp.push(0);

    let random_sp = checked_exec_stack_sub(&mut user_sp, 16)?;
    let mut random = [0u8; 15];
    for (index, byte) in random.iter_mut().enumerate() {
        *byte = index as u8;
    }
    copy_to_user(memory_set, random_sp, &random)?;
    user_sp -= user_sp % 16;

    let execfn = *argvp.first().ok_or(SysErrNo::E2BIG)?;
    auxv.push(Aux::new(AuxType::RANDOM, random_sp));
    auxv.push(Aux::new(AuxType::EXECFN, execfn));
    auxv.push(Aux::new(AuxType::NULL, 0));

    let initial_stack_words = 1 + argvp.len() + envp.len();
    if initial_stack_words % 2 != 0 {
        checked_exec_stack_sub(&mut user_sp, size_of::<usize>())?;
    }
    for aux in auxv.iter().rev() {
        let aux_sp = checked_exec_stack_sub(&mut user_sp, size_of::<Aux>())?;
        copy_to_user_val(memory_set, aux_sp as *mut usize, &(aux.aux_type as usize))?;
        copy_to_user_val(
            memory_set,
            (aux_sp + size_of::<usize>()) as *mut usize,
            &aux.value,
        )?;
    }

    let envp_bytes = envp
        .len()
        .checked_mul(size_of::<usize>())
        .ok_or(SysErrNo::E2BIG)?;
    let envp_base = checked_exec_stack_sub(&mut user_sp, envp_bytes)?;
    for (index, value) in envp.iter().enumerate() {
        copy_to_user_val(
            memory_set,
            (envp_base + index * size_of::<usize>()) as *mut usize,
            value,
        )?;
    }

    let argvp_bytes = argvp
        .len()
        .checked_mul(size_of::<usize>())
        .ok_or(SysErrNo::E2BIG)?;
    let argv_base = checked_exec_stack_sub(&mut user_sp, argvp_bytes)?;
    for (index, value) in argvp.iter().enumerate() {
        copy_to_user_val(
            memory_set,
            (argv_base + index * size_of::<usize>()) as *mut usize,
            value,
        )?;
    }

    let argc_sp = checked_exec_stack_sub(&mut user_sp, size_of::<usize>())?;
    copy_to_user_val(memory_set, argc_sp as *mut usize, &argv.len())?;
    debug_assert_eq!(argc_sp % 16, 0);
    Ok((argc_sp, argv_base, envp_base))
}

impl TaskControlBlock {
    /// Replace this task group's user image with a freshly loaded ELF image.
    pub fn exec(
        &self,
        elf_data: &[u8],
        executable_file: &Arc<OSFile>,
        argv: &[Vec<u8>],
        env: &[Vec<u8>],
    ) -> Result<(), SysErrNo> {
        // User stack, high to low: env strings, argv strings, auxv, envp,
        // argv, then argc.
        debug!("exec: goto from_elf");
        #[cfg(feature = "perf")]
        let from_elf_begin = get_ticks();
        let (memory_set, user_hp, entry_point, mut auxv) =
            MemorySetInner::from_elf_file(elf_data, executable_file).map_err(|_| {
                error!("exec: OOM during ELF load");
                SysErrNo::ENOMEM
            })?;
        #[cfg(feature = "perf")]
        crate::utils::perf::record_exec_from_elf_duration(
            get_ticks().saturating_sub(from_elf_begin),
        );
        validate_exec_stack_layout(argv, env, auxv.len())?;

        debug!("exec: return from from_elf");
        #[cfg(feature = "perf")]
        let stack_begin = get_ticks();
        let memory_set = MemorySet::new(memory_set);
        let (ustack_top, trap_cx_bottom, trap_cx_ppn) = alloc_user_res_in_memory_set(&memory_set)?;
        let (user_sp, argv_base, envp_base) =
            prepare_exec_stack(&memory_set, ustack_top, argv, env, &mut auxv)?;
        let mut trap_cx =
            TrapContext::app_init_context(entry_point, user_sp, self.kernel_stack.top());
        trap_cx.set_a0(argv.len());
        trap_cx.set_a1(argv_base);
        trap_cx.set_a2(envp_base);
        let new_comm = argv.first().map(|argv0| task_comm_from_argv0(argv0));
        #[cfg(feature = "perf")]
        crate::utils::perf::record_exec_stack_duration(get_ticks().saturating_sub(stack_begin));

        #[cfg(feature = "perf")]
        let commit_begin = get_ticks();
        // execve replaces a process-wide address space. No sibling may keep
        // an old trap context or user stack once that replacement happens.
        crate::task::kill_other_threads_before_exec(self);
        // The SIGKILLs used to collapse sibling threads are an internal exec
        // detail, not a termination of the replacement program.
        self.process.meta_lock().termination_signal = None;

        // Snapshot the parent task list before taking this task's inner lock.
        // The lock order is ProcessMeta -> TaskControlBlockInner; retaining
        // the metadata guard while waking a parent task would otherwise let a
        // concurrent scheduler path form an AB-BA cycle.
        let ppid = self.ppid();
        let parent_tasks = Process::get_process_arc_by_pid(ppid)
            .map(|parent_proc| parent_proc.meta_lock().tasks.clone());
        let mut wake_parent_tasks = Vec::new();
        wake_parent_tasks
            .try_reserve(parent_tasks.as_ref().map_or(0, Vec::len))
            .map_err(|_| SysErrNo::ENOMEM)?;

        let mut task_inner = self.inner_lock();
        task_inner.time_data.clear();
        #[cfg(feature = "perf")]
        let vfork_exec_started_at = task_inner.vfork_exec_started_at;

        debug!(
            "task_inner.clear_child_tid={:#x}",
            task_inner.clear_child_tid
        );

        // `clear_child_tid` points into the old address space, so clear and
        // wake it before publishing the new MemorySet.
        if task_inner.clear_child_tid != 0 {
            let old_memory_set = self.process.memory_set_arc();
            let _ = copy_to_user(&old_memory_set, task_inner.clear_child_tid, &[0u8; 4]);
            if let Some(pa) =
                old_memory_set.translate_va(VirtAddr::from(task_inner.clear_child_tid))
            {
                futex_wake_up(pa.0, 1);
            }
            drop(old_memory_set);
            task_inner.clear_child_tid = 0;
        }

        // Activate before replacing the process slot. Otherwise the old root
        // page can be recycled while the current hart still uses it.
        memory_set.activate();
        self.process
            .change_memory_set_and_sigtable(memory_set, SigTable::new());

        task_inner.sig_mask = SigSet::empty();
        task_inner.sigsuspend_restore_mask = None;
        task_inner.alt_signal_stack = SignalStack::disabled();
        #[cfg(feature = "fault-diagnostics")]
        {
            task_inner.signal_frame_trace = SignalFrameTrace::new();
        }
        task_inner.sig_pending = SigSet::empty();
        task_inner.sig_pending_info = [None; crate::signal::SIG_MAX_NUM + 1];
        task_inner.exec_teardown_kill = false;
        // robust_list is an address in the old image. Keeping it across exec
        // would make an early signal handler dereference stale user memory.
        task_inner.robust_list = RobustListHead::default();
        // rseq retains a pointer into the replaced user image.
        task_inner.rseq = RseqState::default();
        task_inner.rseq_pending = true;
        self.process.fd_table.close_on_exec();
        task_inner.trap_cx_ppn = trap_cx_ppn;
        task_inner.trap_cx_bottom = trap_cx_bottom;
        *task_inner.trap_cx() = trap_cx;
        task_inner.user_heappoint = user_hp;
        task_inner.user_heapbottom = user_hp;
        drop(task_inner);
        if let Some(new_comm) = new_comm {
            self.process.meta_lock().comm = new_comm;
        }

        // vfork(2) releases its parent only after the child no longer uses
        // the shared address space. The new page table and trap context above
        // are fully installed at this point.
        #[cfg(feature = "perf")]
        let vfork_parent_ready_at = get_ticks();
        #[cfg(feature = "perf")]
        let mut released_vfork_parent = false;
        if let Some(parent_tasks) = parent_tasks {
            for task_weak in &parent_tasks {
                if let Some(t) = task_weak.upgrade() {
                    let mut parent_inner = t.inner_lock();
                    if parent_inner.vfork_wait_child == self.tid()
                        && parent_inner.task_status == TaskStatus::VforkBlocked
                    {
                        parent_inner.vfork_wait_child = 0;
                        #[cfg(feature = "perf")]
                        {
                            parent_inner.vfork_parent_ready_at = vfork_parent_ready_at;
                            released_vfork_parent = true;
                        }
                        parent_inner.task_status = TaskStatus::Ready;
                        drop(parent_inner);
                        wake_parent_tasks.push(t);
                    }
                }
            }
        }
        for parent_task in wake_parent_tasks {
            crate::task::ready_queue::add_task(&parent_task);
        }
        #[cfg(feature = "perf")]
        if released_vfork_parent {
            crate::utils::perf::record_vfork_release_exec();
            if vfork_exec_started_at != 0 {
                crate::utils::perf::record_vfork_exec_to_parent_ready_duration(
                    vfork_parent_ready_at.saturating_sub(vfork_exec_started_at),
                );
            }
        }
        #[cfg(feature = "perf")]
        crate::utils::perf::record_exec_commit_duration(get_ticks().saturating_sub(commit_begin));
        Ok(())
    }
}
