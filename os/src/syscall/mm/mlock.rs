//! mlock/munlock/mlockall/munlockall/mlock2 系统调用实现
//!
//! 本内核无 swap 机制，所有已分配页面始终驻留在物理内存中，
//! 因此 mlock 系列本质上是 no-op + 参数校验。
//!
//! 参考:
//! - <https://www.man7.org/linux/man-pages/man2/mlock.2.html>
//! - <https://www.man7.org/linux/man-pages/man2/mlock2.2.html>
//! - <https://www.man7.org/linux/man-pages/man2/mlockall.2.html>

use crate::{
    mm::if_bad_address,
    utils::{SysErrNo, SyscallRet},
};

// ---- mlockall / mlock2 标志位 ----
/// mlockall: 锁定当前所有映射页
const MCL_CURRENT: i32 = 1;
/// mlockall: 锁定未来所有映射页
const MCL_FUTURE: i32 = 2;
/// mlockall / mlock2: 仅当缺页时才锁定（本内核无 swap，等价于普通 mlock）
const MCL_ONFAULT: i32 = 4;
/// mlock2 专用参数名（值与 MCL_ONFAULT 相同）
const MLOCK_ONFAULT: i32 = 1;

// ---- 辅助函数 ----

/// 校验用户地址范围 [addr, addr+len) 是否适合 no-op mlock。
fn validate_addr_range(addr: usize, len: usize) -> Result<(), SysErrNo> {
    // 长度为 0 是合法的 no-op
    if len == 0 {
        return Ok(());
    }

    let end = addr.checked_add(len).ok_or(SysErrNo::EINVAL)?;
    if if_bad_address(addr) || if_bad_address(end - 1) {
        return Err(SysErrNo::EFAULT);
    }

    Ok(())
}

// ---- 公开 syscall 函数 ----

/// 参考 <https://www.man7.org/linux/man-pages/man2/mlock.2.html>
///
/// 锁定地址空间中的页面，阻止被交换出去。
/// 本内核无 swap，仅做参数校验。
pub fn sys_mlock(addr: usize, len: usize) -> SyscallRet {
    validate_addr_range(addr, len)?;
    Ok(0)
}

/// 参考 <https://www.man7.org/linux/man-pages/man2/munlock.2.html>
///
/// 解锁地址空间中的页面。
pub fn sys_munlock(addr: usize, len: usize) -> SyscallRet {
    validate_addr_range(addr, len)?;
    Ok(0)
}

/// 参考 <https://www.man7.org/linux/man-pages/man2/mlock2.2.html>
///
/// 带 flags 的 mlock 变体。当前仅 MLOCK_ONFAULT(1) 有效。
pub fn sys_mlock2(addr: usize, len: usize, flags: i32) -> SyscallRet {
    // flags 必须为 0 或 MLOCK_ONFAULT
    if flags != 0 && flags != MLOCK_ONFAULT {
        return Err(SysErrNo::EINVAL);
    }
    validate_addr_range(addr, len)?;
    Ok(0)
}

/// 参考 <https://www.man7.org/linux/man-pages/man2/mlockall.2.html>
///
/// 锁定进程全部当前/未来映射页。
pub fn sys_mlockall(flags: i32) -> SyscallRet {
    // MCL_CURRENT | MCL_FUTURE | MCL_ONFAULT 有效组合
    let valid_mask = MCL_CURRENT | MCL_FUTURE | MCL_ONFAULT;
    if flags & !valid_mask != 0 || flags == 0 {
        return Err(SysErrNo::EINVAL);
    }
    Ok(0)
}

/// 参考 <https://www.man7.org/linux/man-pages/man2/munlockall.2.html>
///
/// 解锁进程全部页面。
pub fn sys_munlockall() -> SyscallRet {
    Ok(0)
}
