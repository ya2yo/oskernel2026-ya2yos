//! 来源于StarryOS(https://github.com/Starry-OS/StarryOS)
//! 和clone实现有关
use log::debug;

use crate::{
    signal::SIG_MAX_NUM,
    task::{current_task, ready_queue, suspend_current_and_run_next, CloneFlags},
    utils::{SysErrNo, SyscallRet},
};

const CSIGNAL: u64 = 0xff;

fn parse_clone_flags(raw_flags: usize) -> Result<(CloneFlags, i32), SysErrNo> {
    let exit_signal = (raw_flags as u64) & CSIGNAL;
    if exit_signal as usize > SIG_MAX_NUM {
        return Err(SysErrNo::EINVAL);
    }

    let flags = CloneFlags::from_bits((raw_flags as u64) & !CSIGNAL).ok_or(SysErrNo::EINVAL)?;
    validate_clone_flags(flags)?;

    Ok((
        flags,
        if exit_signal == 0 {
            -1
        } else {
            exit_signal as i32
        },
    ))
}

fn validate_clone_flags(flags: CloneFlags) -> Result<(), SysErrNo> {
    if flags.contains(CloneFlags::CLONE_THREAD) {
        if !flags.contains(CloneFlags::CLONE_SIGHAND) || !flags.contains(CloneFlags::CLONE_VM) {
            return Err(SysErrNo::EINVAL);
        }
        // Legacy clone(2) accepts CSIGNAL bits together with CLONE_THREAD
        // and ignores the requested termination signal.  clone3(2) has a
        // separate exit_signal field and rejects this combination there.
    }

    if flags.contains(CloneFlags::CLONE_SIGHAND) && !flags.contains(CloneFlags::CLONE_VM) {
        return Err(SysErrNo::EINVAL);
    }
    if flags.contains(CloneFlags::CLONE_CLEAR_SIGHAND) && flags.contains(CloneFlags::CLONE_SIGHAND)
    {
        return Err(SysErrNo::EINVAL);
    }
    if flags.contains(CloneFlags::CLONE_PIDFD) && flags.contains(CloneFlags::CLONE_PARENT_SETTID) {
        return Err(SysErrNo::EINVAL);
    }
    if flags.intersects(
        CloneFlags::CLONE_PIDFD
            | CloneFlags::CLONE_NEWCGROUP
            | CloneFlags::CLONE_NEWIPC
            | CloneFlags::CLONE_NEWNET
            | CloneFlags::CLONE_NEWNS
            | CloneFlags::CLONE_NEWPID
            | CloneFlags::CLONE_NEWUSER
            | CloneFlags::CLONE_NEWUTS
            | CloneFlags::CLONE_INTO_CGROUP,
    ) {
        return Err(SysErrNo::EINVAL);
    }
    if flags.contains(CloneFlags::CLONE_NEWNS) && flags.contains(CloneFlags::CLONE_FS) {
        return Err(SysErrNo::EINVAL);
    }

    Ok(())
}

/// 参考 https://man7.org/linux/man-pages/man2/clone.2.html
/// void (*fn)(void* arg) 参数通过栈传递,如果stack_ptr!=0, fn=0(stack),arg=8(stack)
pub fn sys_clone(
    flags: usize,
    stack_ptr: usize,
    parent_tid_ptr: usize,
    #[cfg(target_arch = "loongarch64")] child_tid_ptr: usize,
    tls_ptr: usize,
    #[cfg(not(target_arch = "loongarch64"))] child_tid_ptr: usize,
) -> SyscallRet {
    let (flags, exit_signal) = parse_clone_flags(flags)?;
    debug!(
        "[sys_clone] flags={:?},stack:{:#x},parent_tid_ptr:{:#x},child_tid_ptr:{:#x},tls_ptr:{:#x}",
        flags, stack_ptr, parent_tid_ptr, child_tid_ptr, tls_ptr
    );
    // if current_task().unwrap().pid() == 4 {
    //     return Ok(current_task().unwrap().tid());
    // }

    let task = current_task().unwrap();
    let new_task = task.clone_process(
        flags,
        exit_signal,
        stack_ptr,
        parent_tid_ptr as *mut u32,
        tls_ptr,
        child_tid_ptr as *mut u32,
    )?;
    let new_tid = new_task.tid();
    // we do not have to move to next instruction since we have done it before
    // add new task to scheduler
    #[cfg(feature = "perf")]
    let enqueue_begin = crate::arch::time::get_ticks();
    ready_queue::add_task(&new_task);
    #[cfg(feature = "perf")]
    crate::utils::perf::record_clone_enqueue_duration(
        crate::arch::time::get_ticks().saturating_sub(enqueue_begin),
    );
    if flags.contains(CloneFlags::CLONE_VFORK) {
        // vfork(2) must not return to the parent until the child has called
        // execve() or exited. clone_process() already marked this task as
        // VforkBlocked; switch immediately so userspace cannot reclaim the
        // shared child stack before the child first runs.
        suspend_current_and_run_next();
    }
    Ok(new_tid)
}
