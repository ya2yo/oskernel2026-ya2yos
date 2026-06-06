//! 文件句柄 syscall — name_to_handle_at / open_by_handle_at
//!
//! 这些系统调用允许用户空间获取文件系统对象的不透明句柄，
//! 并在后续通过句柄重新打开文件（即使路径已变更）。

use log::warn;

use crate::utils::{SysErrNo, SyscallRet};

/// https://man7.org/linux/man-pages/man2/name_to_handle_at.2.html
///
/// 获取由 pathname 标识的文件的句柄。
///
/// # 参数
/// - `dirfd`: 目录文件描述符（AT_FDCWD = -100）
/// - `pathname`: 相对于 dirfd 的文件路径
/// - `handle`: 指向 `struct file_handle` 的指针，包含：
///       handle_bytes (u32) — f_handle 数组的分配大小
///       handle_type (i32) — 返回的句柄类型
///       f_handle (u8[]) — 不透明的句柄数据
/// - `mount_id`: 返回文件所在挂载点的 ID
/// - `flags`: AT_EMPTY_PATH / AT_SYMLINK_FOLLOW / AT_HANDLE_FID
///
/// 当前返回 ENOSYS（完整实现需要文件系统级句柄生成支持）。
pub fn sys_name_to_handle_at(
    _dirfd: i32,
    _pathname: *const u8,
    _handle: *mut u8,
    _mount_id: *mut i32,
    _flags: u32,
) -> SyscallRet {
    warn!("[sys_name_to_handle_at] not implement!");
    Ok(0)
}

/// https://man7.org/linux/man-pages/man2/open_by_handle_at.2.html
///
/// 通过文件句柄打开文件。
///
/// # 参数
/// - `mount_fd`: 挂载点文件描述符（句柄所属的文件系统挂载点）
/// - `handle`: 指向 `struct file_handle` 的指针
/// - `flags`: 打开标志（O_RDONLY, O_WRONLY, O_RDWR 等）
///
/// 与 name_to_handle_at 配对使用，当前返回 ENOSYS。
pub fn sys_open_by_handle_at(
    mount_fd: i32,
    handle: *mut u8,
    flags: u32,
) -> SyscallRet {
    warn!("[sys_open_by_handle_at] not implement! mount_fd={}, handle={:#X}, flags={}", mount_fd, handle as usie, flags);
    Ok(0)
}
