use linux_raw_sys::general::clone_args;
use log::debug;

use crate::{
    mm::copy_from_user,
    task::current_task,
    utils::{SysErrNo, SyscallRet},
};

use super::clone::sys_clone;

/// 参考 https://man7.org/linux/man-pages/man2/clone3.2.html
///
/// clone3() 是 clone() 的增强版本，通过 `clone_args` 结构体传递参数。
/// 当前实现将 clone_args 翻译为 legacy clone 参数后交由 sys_clone 处理。
pub fn sys_clone3(cl_args: *const clone_args, size: usize) -> SyscallRet {
    debug!("[sys_clone3] cl_args=0x{:x}, size={}", cl_args as usize, size);

    // Validate that the userspace pointer is not null and is accessible.
    if cl_args.is_null() {
        return Err(SysErrNo::EFAULT);
    }
    if (cl_args as isize) <= 0 {
        return Err(SysErrNo::EFAULT);
    }

    // The kernel uses `size` to determine which fields of clone_args
    // are valid.  At minimum, `size` must be >= sizeof(clone_args) for
    // all fields we care about, or the caller is using an older ABI
    // that we don't support.
    if size < core::mem::size_of::<clone_args>() {
        return Err(SysErrNo::EINVAL);
    }

    let task = current_task().unwrap();
    let proc_inner = task.process.inner_lock();
    let memory_set = proc_inner.get_locked_memory_set_read();

    // Read the entire clone_args structure from userspace.
    let mut cargs: clone_args = unsafe { core::mem::zeroed() };
    copy_from_user(
        &memory_set,
        cl_args as usize,
        unsafe {
            core::slice::from_raw_parts_mut(
                &mut cargs as *mut clone_args as *mut u8,
                core::mem::size_of::<clone_args>(),
            )
        },
    )?;

    // Extract the exit_signal from clone_args and fold it into flags.
    // In clone3, exit_signal is a separate field (only the low 8 bits
    // are significant); in legacy clone, it was ORed into the flags.
    let mut flags = cargs.flags;
    let exit_signal = (cargs.exit_signal & 0xff) as u64;
    if exit_signal != 0 {
        flags |= exit_signal;
    }
    drop(memory_set);
    drop(proc_inner);
    drop(task);
    // Delegate to the existing legacy clone implementation.
    // The parameter order differs between architectures (see sys_clone
    // signature in clone.rs), so we use cfg‑gated calls.
    // pidfd, set_tid, cgroup fields are silently ignored.
    #[cfg(not(target_arch = "loongarch64"))]
    {
        sys_clone(
            flags as usize,
            cargs.stack as usize,
            cargs.parent_tid as usize,
            cargs.tls as usize,
            cargs.child_tid as usize,
        )
    }
    #[cfg(target_arch = "loongarch64")]
    {
        sys_clone(
            flags as usize,
            cargs.stack as usize,
            cargs.parent_tid as usize,
            cargs.child_tid as usize,
            cargs.tls as usize,
        )
    }
}
