use crate::{
    mm::copy_to_user,
    task::current_task,
    utils::{SysErrNo, SysResult, SyscallRet},
};
use log::warn;
/// 参考 https://man7.org/linux/man-pages/man2/umask.2.html
///
/// 设置进程的文件模式创建掩码为 `mask & 0777`，返回旧的掩码。
/// 该值不会被 CLONE_FS 或 fork 后的 unshare 重置；
/// 子进程继承父进程的 umask。
pub fn sys_umask(mask: u32) -> SyscallRet {
    let task = current_task().unwrap();
    let proc_inner = task.process.inner_lock();
    let old_umask = proc_inner.fs_info.set_umask(mask);
    Ok(old_umask as usize)
}

/// MPOL_DEFAULT — 默认 NUMA 内存策略（无约束）。
const MPOL_DEFAULT: i32 = 0;

/// 参考 https://man7.org/linux/man-pages/man2/set_mempolicy.2.html
///
/// 设置调用线程的 NUMA 内存策略。
/// 由于内核没有 NUMA 支持，仅接受 mode == MPOL_DEFAULT (0)；
/// 其他模式返回 EINVAL。
pub fn sys_set_mempolicy(mode: i32, nodemask: *const u64, maxnode: u64) -> SyscallRet {
    warn!("[sys_set_mempolicy] not implement mode={}, nodemask={}, maxnode={}", mode, nodemask, maxnode);
    if mode == MPOL_DEFAULT {
        return Ok(0);
    }
    // Only MPOL_DEFAULT is supported on non-NUMA kernels.
    Err(SysErrNo::EINVAL)
}

/// 参考 https://man7.org/linux/man-pages/man2/get_mempolicy.2.html
///
/// 查询调用线程的 NUMA 内存策略。
/// 在无 NUMA 的内核上，始终报告 mode == MPOL_DEFAULT (0)。
pub fn sys_get_mempolicy(
    mode: usize,
    _nodemask: usize,
    _maxnode: usize,
    _addr: usize,
    _flags: usize,
) -> SysResult<usize> {
    // Write MPOL_DEFAULT (0) into the user's mode pointer if non-null.
    if mode != 0 {
        let task = current_task().unwrap();
        let proc_inner = task.process.inner_lock();
        let memory_set = proc_inner.get_locked_memory_set_read();
        let mpol_val: i32 = MPOL_DEFAULT;
        copy_to_user(&memory_set, mode, unsafe {
            core::slice::from_raw_parts(
                &mpol_val as *const i32 as *const u8,
                core::mem::size_of::<i32>(),
            )
        })?;
    }
    Ok(0)
}
