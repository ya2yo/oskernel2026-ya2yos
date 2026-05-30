//! eventfd 系统调用入口。
//!
//! 参考 https://man7.org/linux/man-pages/man2/eventfd.2.html

use linux_raw_sys::general::{EFD_CLOEXEC, EFD_NONBLOCK, EFD_SEMAPHORE};

use crate::{
    fs::{EventFd, File, FileClass, FileDescriptor, OpenFlags},
    task::current_task,
    utils::{SysErrNo, SysResult, SyscallRet},
};

bitflags! {
    /// eventfd 创建标志。
    pub struct EventfdFlags: u32 {
        /// 设置 close-on-exec 标志
        const CLOEXEC = EFD_CLOEXEC;
        /// 设置非阻塞模式
        const NONBLOCK = EFD_NONBLOCK;
        /// 提供信号量语义：read 返回 1 并将计数器减 1
        const SEMAPHORE = EFD_SEMAPHORE;
    }
}

/// `eventfd2(initval, flags)` — 创建用于事件通知的文件描述符。
///
/// # 参数
/// * `initval` — 计数器初始值（u32，内部扩展为 u64）
/// * `flags` — 可选 `EFD_CLOEXEC | EFD_NONBLOCK | EFD_SEMAPHORE`
///
/// # 返回
/// * `Ok(fd)` — 新 eventfd 的文件描述符
/// * `Err(EINVAL)` — 无效标志
pub fn sys_eventfd2(initval: u32, flags: u32) -> SyscallRet {
    let eflags = EventfdFlags::from_bits(flags).ok_or(SysErrNo::EINVAL)?;

    let semaphore = eflags.contains(EventfdFlags::SEMAPHORE);
    let event_file = EventFd::new(initval as u64, semaphore);

    // 将 eventfd 标志转换为 OpenFlags
    let mut open_flags = OpenFlags::O_RDWR; // eventfd 同时支持读写
    if eflags.contains(EventfdFlags::CLOEXEC) {
        open_flags |= OpenFlags::O_CLOEXEC;
    }
    if eflags.contains(EventfdFlags::NONBLOCK) {
        open_flags |= OpenFlags::O_NONBLOCK;
        event_file.set_nonblocking(true)?;
    }

    let task = current_task().unwrap();
    let fd = task.get_fd_table().alloc_fd()?;
    task.get_fd_table().set(
        fd,
        FileDescriptor::new(open_flags, FileClass::Abs(event_file)),
    )?;

    Ok(fd)
}
