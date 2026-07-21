use linux_raw_sys::general::clone_args;
use log::debug;

use crate::{
    mm::copy_from_user,
    signal::SIG_MAX_NUM,
    task::{current_task, CloneFlags},
    utils::{SysErrNo, SyscallRet},
};

use super::clone::sys_clone;

const CLONE_ARGS_SIZE_VER0: usize = 64;

/// 参考 https://man7.org/linux/man-pages/man2/clone3.2.html
///
/// clone3() 是 clone() 的增强版本，通过 `clone_args` 结构体传递参数。
/// 当前实现将 clone_args 翻译为 legacy clone 参数后交由 sys_clone 处理。
pub fn sys_clone3(cl_args: *const clone_args, size: usize) -> SyscallRet {
    debug!(
        "[sys_clone3] cl_args=0x{:x}, size={}",
        cl_args as usize, size
    );

    // Validate that the userspace pointer is not null and is accessible.
    if cl_args.is_null() {
        return Err(SysErrNo::EFAULT);
    }
    if (cl_args as isize) <= 0 {
        return Err(SysErrNo::EFAULT);
    }

    // Linux accepts older clone_args versions.  Version 0 contains fields up
    // through tls (64 bytes); later fields are treated as zero if absent.
    if size < CLONE_ARGS_SIZE_VER0 {
        return Err(SysErrNo::EINVAL);
    }

    let task = current_task().unwrap();
    let proc_inner = &task.process;
    let memory_set = proc_inner.memory_set_arc();

    // Read the entire clone_args structure from userspace.
    let mut cargs: clone_args = unsafe { core::mem::zeroed() };
    let copy_len = size.min(core::mem::size_of::<clone_args>());
    copy_from_user(&memory_set, cl_args as usize, unsafe {
        core::slice::from_raw_parts_mut(&mut cargs as *mut clone_args as *mut u8, copy_len)
    })?;
    #[cfg(target_arch = "loongarch64")]
    {
        if size > core::mem::size_of::<clone_args>() {
            let extra_len = size - core::mem::size_of::<clone_args>();
            let mut extra = [0u8; 32];
            let mut copied = 0;
            while copied < extra_len {
                let chunk_len = extra.len().min(extra_len - copied);
                copy_from_user(
                    &memory_set,
                    cl_args as usize + core::mem::size_of::<clone_args>() + copied,
                    &mut extra[..chunk_len],
                )?;
                if extra[..chunk_len].iter().any(|byte| *byte != 0) {
                    return Err(SysErrNo::E2BIG);
                }
                copied += chunk_len;
            }
        }
    }

    if cargs.set_tid_size != 0 || cargs.set_tid != 0 {
        return Err(SysErrNo::EINVAL);
    }

    if cargs.flags & 0xff != 0 || cargs.exit_signal as usize > SIG_MAX_NUM {
        return Err(SysErrNo::EINVAL);
    }

    // Extract the exit_signal from clone_args and fold it into flags.
    // In clone3, exit_signal is a separate field; in legacy clone, it was
    // ORed into the low CSIGNAL bits of flags.
    let mut flags = cargs.flags;
    let exit_signal = cargs.exit_signal;
    if exit_signal != 0 {
        flags |= exit_signal;
    }
    #[cfg(target_arch = "loongarch64")]
    {
        if CloneFlags::from_bits_truncate(flags).contains(CloneFlags::CLONE_PIDFD) {
            let mut pidfd_probe = [0u8; core::mem::size_of::<u32>()];
            copy_from_user(&memory_set, cargs.pidfd as usize, &mut pidfd_probe)?;
            return Err(SysErrNo::EINVAL);
        }
    }
    let stack = if cargs.stack == 0 {
        if cargs.stack_size != 0 {
            return Err(SysErrNo::EINVAL);
        }
        0
    } else {
        if cargs.stack_size == 0 {
            return Err(SysErrNo::EINVAL);
        }
        (cargs.stack as usize)
            .checked_add(cargs.stack_size as usize)
            .ok_or(SysErrNo::EINVAL)?
    };
    drop(memory_set);
    drop(task);
    // Delegate to the existing legacy clone implementation.
    // The parameter order differs between architectures (see sys_clone
    // signature in clone.rs), so we use cfg‑gated calls.
    // pidfd/cgroup are rejected by sys_clone flag validation when requested.
    #[cfg(not(target_arch = "loongarch64"))]
    {
        sys_clone(
            flags as usize,
            stack,
            cargs.parent_tid as usize,
            cargs.tls as usize,
            cargs.child_tid as usize,
        )
    }
    #[cfg(target_arch = "loongarch64")]
    {
        sys_clone(
            flags as usize,
            stack,
            cargs.parent_tid as usize,
            cargs.child_tid as usize,
            cargs.tls as usize,
        )
    }
}
