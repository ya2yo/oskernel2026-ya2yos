use log::warn;

use crate::mm::copy_to_user;
use crate::task::current_task;
use crate::utils::{SysResult, SyscallRet};

// Policies
const MPOL_DEFAULT: i32 = 0;
const MPOL_PREFERRED: i32 = 1;
const MPOL_BIND: i32 = 2;
const MPOL_INTERLEAVE: i32 = 3;
const MPOL_LOCAL: i32 = 4;
const MPOL_PREFERRED_MANY: i32 = 5;

/// 参考 https://man7.org/linux/man-pages/man2/umask.2.html
///
/// 设置进程的文件模式创建掩码为 `mask & 0777`，返回旧的掩码。
/// 该值不会被 CLONE_FS 或 fork 后的 unshare 重置；
/// 子进程继承父进程的 umask。
pub fn sys_umask(mask: u32) -> SyscallRet {
    let task = current_task().unwrap();
    let proc_inner = &task.process;
    let old_umask = proc_inner.fs_info.set_umask(mask);
    Ok(old_umask as usize)
}

/// https://man7.org/linux/man-pages/man2/personality.2.html
///
/// 设置或读取进程执行域（personality）。
///
/// - 若 `persona == 0xffffffff`，返回当前 personality 而不修改。
/// - 否则，将 personality 设为 `persona`，返回之前的 personality。
///
/// 当前内核仅支持 PER_LINUX (0)。
pub fn sys_personality(persona: u32) -> SyscallRet {
    const READ_ONLY: u32 = 0xffffffff;

    let task = current_task().unwrap();
    let old = if persona == READ_ONLY {
        task.process.personality()
    } else {
        task.process.set_personality(persona)
    };

    Ok(old as usize)
}

// https://man7.org/linux/man-pages/man2/get_mempolicy.2.html
pub fn sys_get_mempolicy(
    mode: usize,
    _nodemask: usize,
    _maxnode: usize,
    _addr: usize,
    _flags: usize,
) -> SyscallRet {
    // Write MPOL_DEFAULT (0) into the user's mode pointer if non-null.
    if mode != 0 {
        let task = current_task().unwrap();
        let proc_inner = &task.process;
        let memory_set = proc_inner.memory_set_arc();
        let mpol_val: i32 = 0;
        copy_to_user(&memory_set, mode, unsafe {
            core::slice::from_raw_parts(
                &mpol_val as *const i32 as *const u8,
                core::mem::size_of::<i32>(),
            )
        })?;
    }
    Ok(0)
}
/// https://www.man7.org/linux/man-pages/man2/set_mempolicy.2.html
pub fn sys_set_mempolicy(mode: i32, nodemask: usize, maxnode: usize) -> SyscallRet {
    warn!(
        "[sys_set_mempolicy] mode={}, nodemask={}, maxnode={}",
        mode, nodemask, maxnode
    );
    Ok(0)
}
