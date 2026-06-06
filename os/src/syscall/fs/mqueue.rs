//! POSIX 消息队列 (mqueue) 系统调用
//!
//! 当前为桩实现，不支持 POSIX 消息队列。

use crate::utils::{SysErrNo, SyscallRet};
use log::warn;

/// https://man7.org/linux/man-pages/man3/mq_open.3.html
/// 创建或打开一个 POSIX 消息队列。
pub fn sys_mq_open(
    _name: *const u8,
    _oflag: i32,
    _mode: u32,
    _attr: *const u8,
) -> SyscallRet {
    warn!("[sys_mq_open] not implement!");
    Err(SysErrNo::ENOSYS)
}

/// https://man7.org/linux/man-pages/man3/mq_unlink.3.html
/// 删除指定名称的消息队列。
pub fn sys_mq_unlink(_name: *const u8) -> SyscallRet {
    warn!("[sys_mq_unlink] not implement!");
    Err(SysErrNo::ENOSYS)
}

/// https://man7.org/linux/man-pages/man3/mq_timedsend.3.html
/// 向消息队列发送消息，带超时。
pub fn sys_mq_timedsend(
    _mqdes: usize,
    _msg_ptr: *const u8,
    _msg_len: usize,
    _msg_prio: u32,
    _abs_timeout: *const u8,
) -> SyscallRet {
    warn!("[sys_mq_timedsend] not implement!");
    Err(SysErrNo::ENOSYS)
}

/// https://man7.org/linux/man-pages/man3/mq_timedreceive.3.html
/// 从消息队列接收消息，带超时。
pub fn sys_mq_timedreceive(
    _mqdes: usize,
    _msg_ptr: *mut u8,
    _msg_len: usize,
    _msg_prio: *mut u32,
    _abs_timeout: *const u8,
) -> SyscallRet {
    warn!("[sys_mq_timedreceive] not implement!");
    Err(SysErrNo::ENOSYS)
}

/// https://man7.org/linux/man-pages/man3/mq_notify.3.html
/// 注册或取消消息队列的通知。
pub fn sys_mq_notify(_mqdes: usize, _notification: *const u8) -> SyscallRet {
    warn!("[sys_mq_notify] not implement!");
    Err(SysErrNo::ENOSYS)
}
