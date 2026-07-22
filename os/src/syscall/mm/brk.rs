use log::debug;

use crate::{
    arch::memory_layout::MAX_BRK_SIZE,
    task::current_task,
    utils::{SysErrNo, SyscallRet},
};

/// 参考 https://man7.org/linux/man-pages/man2/brk.2.html
pub fn sys_brk(brk_addr: usize) -> SyscallRet {
    debug!("[sys_brk] brk_addr={:#x}", brk_addr);
    let task = current_task().unwrap();
    let former_addr = task.growproc(0);
    if brk_addr == 0 {
        return Ok(former_addr);
    }
    // Reject brk growth that would exceed the per-process limit.
    // Glibc falls back to brk when mmap is restricted; without this
    // check it grows the heap to >128 MiB and exhausts CMA via page
    // faults (same pattern as the mmap probing issue).
    let heap_bottom = task.inner_lock().user_heapbottom;
    if brk_addr > former_addr && brk_addr - heap_bottom > MAX_BRK_SIZE {
        debug!(
            "[sys_brk] ENOMEM: requested={:#x}, heap_bottom={:#x}, max={:#x}",
            brk_addr, heap_bottom, MAX_BRK_SIZE
        );
        return Err(SysErrNo::ENOMEM);
    }
    let grow_size: isize = (brk_addr - former_addr) as isize;
    Ok(task.growproc(grow_size))
}
