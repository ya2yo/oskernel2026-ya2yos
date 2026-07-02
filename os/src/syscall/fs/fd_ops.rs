use core::{future::poll_fn, task::Poll};

use super::fcntl::*;
use super::file_lock::{self, Flock};
use crate::fs::{
    FileClass, FileDescriptor, FsIndex, OpenFlags, TmpFile, map_dynamic_link_file, open, open_fifo,
    refresh_proc_stat, refresh_proc_status, superblock_root_inode,
};
use crate::mm::{copy_from_user, copy_to_user, if_bad_address, translate::read_user_cstr};
use crate::syscall::{Syscall, options::FcntlCmd};
use crate::task::{Process, block_on, current_task, interruptible};
use crate::utils::{SysErrNo, SyscallRet};
use alloc::{
    format,
    string::{String, ToString},
    sync::Arc,
    vec,
    vec::Vec,
};
use linux_raw_sys::general::open_how;
use log::{debug, error, warn};

/// https://man7.org/linux/man-pages/man2/flock.2.html
///
/// 对 fd 指定的文件应用或释放 advisory lock。
///
/// # 参数
/// - `fd`: 打开的文件描述符
/// - `op`: 操作类型：
///   - `LOCK_SH` (1): 共享锁（多个持有者可共存）
///   - `LOCK_EX` (2): 排他锁（独占）
///   - `LOCK_NB` (4): 非阻塞（与 LOCK_SH/LOCK_EX 按位或）
///   - `LOCK_UN` (8): 解锁
///
/// # 阻塞语义
/// - 未设置 `LOCK_NB` 时，若锁冲突则阻塞等待直到锁可用或被信号中断
/// - 设置 `LOCK_NB` 时，锁冲突立即返回 `EAGAIN`
pub fn sys_flock(fd: i32, op: i32) -> SyscallRet {
    let valid_mask = LOCK_SH | LOCK_EX | LOCK_NB | LOCK_UN;
    if op & !valid_mask != 0 {
        return Err(SysErrNo::EINVAL);
    }

    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let proc_inner = &task.process;
    let fd_desc = proc_inner.fd_table.get(fd as usize)?;

    // flock 仅适用于普通文件（OSFile），非普通文件返回 EINVAL
    let osfile = fd_desc.file()?;

    let inode_path = osfile.inode.path();
    let file_ptr = Arc::as_ptr(&osfile) as usize;

    // --- 解锁 ---
    if (op & LOCK_UN) != 0 {
        file_lock::flock_unlock(&inode_path, file_ptr);
        return Ok(0);
    }

    // --- 加锁 ---
    let lock_type = op & (LOCK_SH | LOCK_EX);
    if lock_type != LOCK_SH && lock_type != LOCK_EX {
        return Err(SysErrNo::EINVAL);
    }

    let nonblock = (op & LOCK_NB) != 0;

    if nonblock {
        return file_lock::flock_try_lock(&inode_path, file_ptr, lock_type).map(|_| 0);
    }

    // 阻塞等待（可被信号中断）
    // 参照 waitpid 的 block_on + interruptible + poll_fn 模式
    drop(fd_desc);
    drop(task);
    let path = inode_path; // String，移入闭包
    block_on(interruptible(poll_fn(move |cx| {
        match file_lock::flock_try_lock(&path, file_ptr, lock_type) {
            Ok(()) => Poll::Ready(Ok(0)),
            Err(SysErrNo::EAGAIN) => {
                // 注册 waker，当其他进程释放锁时会被唤醒
                file_lock::flock_register_waker(&path, cx.waker());
                Poll::Pending
            }
            Err(e) => Poll::Ready(Err(e)),
        }
    })))?
}

fn dup_fd(old_fd: usize, cloexec: bool) -> SyscallRet {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let proc_inner = &task.process;
    let mut new_desc = proc_inner.fd_table.get(old_fd)?;
    if cloexec {
        new_desc.set_cloexec();
    } else {
        new_desc.unset_cloexec();
    }
    let new_fd = proc_inner.fd_table.alloc_fd()?;
    if let Err(e) = proc_inner.fd_table.set(new_fd, new_desc) {
        proc_inner.fd_table.take(new_fd);
        return Err(e);
    }
    proc_inner.fs_info.dup_fd_path(old_fd, new_fd);
    Ok(new_fd)
}

/// 参考 https://man7.org/linux/man-pages/man2/dup.2.html
pub fn sys_dup(fd: usize) -> SyscallRet {
    // debug!("[sys_dup]: fd is {fd}");
    dup_fd(fd, false)
}

/// 参考 https://man7.org/linux/man-pages/man2/dup3.2.html
pub fn sys_dup3(old: usize, new: usize, flags: u32) -> SyscallRet {
    let task = current_task().unwrap();
    let proc_inner = &task.process;

    // debug!(
    //     "[sys_dup3] : oldfd is {}, newfd is {}, flags is {}",
    //     old, new, flags
    // );
    if old == new {
        return Err(SysErrNo::EINVAL);
    }
    if old >= proc_inner.fd_table.len() || new >= proc_inner.fd_table.get_soft_limit() {
        return Err(SysErrNo::EMFILE); // 添加文件描述符耗尽检查
    }

    if old >= proc_inner.fd_table.len()
        || (old as isize) < 0
        || (new as isize) < 0
        || new >= proc_inner.fd_table.get_soft_limit()
    {
        error!("lots of");
        return Err(SysErrNo::EBADF);
    }
    // 检查文件描述符表是否已满
    if proc_inner.fd_table.try_get(old).is_none() {
        return Err(SysErrNo::EINVAL);
    }

    if proc_inner.fd_table.len() <= new {
        proc_inner.fd_table.resize(new + 1)?;
    }

    let mut file = proc_inner.fd_table.get(old)?;
    if flags == 0x800000 || flags == 0x80000 {
        //flags包含O_CLOEXEC,为新的fd设置该标志，否则不设置
        file.set_cloexec();
    } else {
        file.unset_cloexec();
    }
    proc_inner.fd_table.set(new, file);
    Ok(new)
}

/// 参考 https://man7.org/linux/man-pages/man2/fcntl.2.html
pub fn sys_fcntl(fd: usize, cmd: usize, arg: usize) -> SyscallRet {
    let task = current_task().unwrap();
    let proc_inner = &task.process;

    // debug!("[sys_fcntl] fd is {}, cmd is {}, arg is {}", fd, cmd, arg);

    if fd >= proc_inner.fd_table.len() || (fd as isize) < 0 {
        return Err(SysErrNo::EBADF);
    }

    if proc_inner.fd_table.try_get(fd).is_none() {
        return Err(SysErrNo::EINVAL);
    }

    let cmd = FcntlCmd::from_bits(cmd).ok_or(SysErrNo::EINVAL)?;

    match cmd {
        FcntlCmd::F_DUPFD => {
            let file = proc_inner.fd_table.get(fd)?;
            let fd_new = proc_inner.fd_table.alloc_fd_larger_than(arg)?;
            proc_inner.fd_table.set(fd_new, file);
            proc_inner.fs_info.dup_fd_path(fd, fd_new);
            return Ok(fd_new);
        }
        FcntlCmd::F_DUPFD_CLOEXEC => {
            let mut file = proc_inner.fd_table.get(fd)?;
            file.set_cloexec();
            let fd_new = proc_inner.fd_table.alloc_fd_larger_than(arg)?;
            proc_inner.fd_table.set(fd_new, file);
            proc_inner.fs_info.dup_fd_path(fd, fd_new);
            return Ok(fd_new);
        }
        FcntlCmd::F_GETFD => {
            return if proc_inner.fd_table.get(fd)?.cloexec() {
                Ok(1)
            } else {
                Ok(0)
            };
        }
        FcntlCmd::F_SETFD => {
            if arg & FD_CLOEXEC as usize == 0 {
                proc_inner.fd_table.unset_cloexec(fd);
            } else {
                proc_inner.fd_table.set_cloexec(fd);
            }
        }
        FcntlCmd::F_GETFL => {
            let file = proc_inner.fd_table.get(fd)?;
            let mut res = OpenFlags::O_RDWR.bits() as usize;
            if file.non_block() {
                res |= OpenFlags::O_NONBLOCK.bits() as usize;
            }
            return Ok(res);
        }
        FcntlCmd::F_SETFL => {
            let file = proc_inner.fd_table.get(fd)?;
            let flags = OpenFlags::from_bits_truncate(arg as u32);
            if flags.contains(OpenFlags::O_NONBLOCK) {
                proc_inner.fd_table.set_nonblock(fd)?;
                file.any().set_nonblocking(true)?;
            } else {
                proc_inner.fd_table.unset_nonblock(fd)?;
                file.any().set_nonblocking(false)?;
            }
        }

        // -------------------------------------------------------------------
        // 文件记录锁（F_GETLK / F_SETLK / F_SETLKW）
        // 按 inode 路径在全局锁表中管理 POSIX advisory record lock
        // -------------------------------------------------------------------
        FcntlCmd::F_GETLK | FcntlCmd::F_GETLK64 => {
            let (inode_path, file_size) = {
                let file = proc_inner.fd_table.get(fd)?;
                let osfile = file.file()?;
                (osfile.inode.path(), osfile.inode.size() as i64)
            };

            let memory_set = proc_inner.memory_set_arc();
            let mut flock_bytes = [0u8; 32];
            copy_from_user(&memory_set, arg, &mut flock_bytes)?;
            let mut flock = Flock::from_bytes(&flock_bytes).ok_or(SysErrNo::EINVAL)?;

            file_lock::getlk(&inode_path, &mut flock, file_size)?;

            let result_bytes = flock.to_bytes();
            copy_to_user(&memory_set, arg, &result_bytes)?;
            return Ok(0);
        }
        FcntlCmd::F_SETLK | FcntlCmd::F_SETLK64 => {
            let (inode_path, file_size) = {
                let file = proc_inner.fd_table.get(fd)?;
                let osfile = file.file()?;
                (osfile.inode.path(), osfile.inode.size() as i64)
            };

            let memory_set = proc_inner.memory_set_arc();
            let mut flock_bytes = [0u8; 32];
            copy_from_user(&memory_set, arg, &mut flock_bytes)?;
            let flock = Flock::from_bytes(&flock_bytes).ok_or(SysErrNo::EINVAL)?;

            file_lock::setlk(&inode_path, &flock, file_size)?;
            return Ok(0);
        }
        FcntlCmd::F_SETLKW | FcntlCmd::F_SETLKW64 => {
            // F_SETLKW 应阻塞等待直到锁可用；当前简化为非阻塞行为
            let (inode_path, file_size) = {
                let file = proc_inner.fd_table.get(fd)?;
                let osfile = file.file()?;
                (osfile.inode.path(), osfile.inode.size() as i64)
            };

            let memory_set = proc_inner.memory_set_arc();
            let mut flock_bytes = [0u8; 32];
            copy_from_user(&memory_set, arg, &mut flock_bytes)?;
            let flock = Flock::from_bytes(&flock_bytes).ok_or(SysErrNo::EINVAL)?;

            file_lock::setlk(&inode_path, &flock, file_size)?;
            return Ok(0);
        }

        // -------------------------------------------------------------------
        // OFD（Open File Description）锁 — 简化委托给 POSIX 锁逻辑
        // -------------------------------------------------------------------
        FcntlCmd::F_OFD_GETLK => {
            let (inode_path, file_size) = {
                let file = proc_inner.fd_table.get(fd)?;
                let osfile = file.file()?;
                (osfile.inode.path(), osfile.inode.size() as i64)
            };

            let memory_set = proc_inner.memory_set_arc();
            let mut flock_bytes = [0u8; 32];
            copy_from_user(&memory_set, arg, &mut flock_bytes)?;
            let mut flock = Flock::from_bytes(&flock_bytes).ok_or(SysErrNo::EINVAL)?;

            file_lock::getlk(&inode_path, &mut flock, file_size)?;

            let result_bytes = flock.to_bytes();
            copy_to_user(&memory_set, arg, &result_bytes)?;
            return Ok(0);
        }
        FcntlCmd::F_OFD_SETLK | FcntlCmd::F_OFD_SETLKW => {
            let (inode_path, file_size) = {
                let file = proc_inner.fd_table.get(fd)?;
                let osfile = file.file()?;
                (osfile.inode.path(), osfile.inode.size() as i64)
            };

            let memory_set = proc_inner.memory_set_arc();
            let mut flock_bytes = [0u8; 32];
            copy_from_user(&memory_set, arg, &mut flock_bytes)?;
            let flock = Flock::from_bytes(&flock_bytes).ok_or(SysErrNo::EINVAL)?;

            file_lock::setlk(&inode_path, &flock, file_size)?;
            return Ok(0);
        }

        // -------------------------------------------------------------------
        // 文件 owner / 信号（主要用于套接字）
        // -------------------------------------------------------------------
        FcntlCmd::F_GETOWN => {
            // 返回接收 SIGIO/SIGURG 的进程 ID；-1 表示无 owner
            return Ok((-1i32) as usize);
        }
        FcntlCmd::F_SETOWN => {
            // 设置 owner，当前静默成功（仅对 socket fd 有意义）
        }
        FcntlCmd::F_SETSIG => {
            // 设置信号，静默成功
        }
        FcntlCmd::F_GETSIG => {
            // 0 表示默认行为（SIGIO）
            return Ok(0);
        }
        FcntlCmd::F_SETOWN_EX | FcntlCmd::F_GETOWN_EX => {
            // 扩展 owner 类型（TID/PID/PGRP），暂不支持
            return Err(SysErrNo::EINVAL);
        }
        // 文件租约（file lease）-
        FcntlCmd::F_SETLEASE => {
            // 简化实现：始终返回 EAGAIN（无冲突打开时可成功，但当前不做跟踪）
            // 正确实现需要跟踪每个文件的所有打开 fd，此处作为 stub
            return Err(SysErrNo::EAGAIN);
        }
        FcntlCmd::F_GETLEASE => {
            // 返回当前租约类型，F_UNLCK 表示无租约
            return Ok(F_UNLCK as usize);
        }
        // 目录变动通知
        FcntlCmd::F_NOTIFY => {
            return Err(SysErrNo::EINVAL);
        }
        // DUPFD_QUERY — 查询 F_DUPFD 将分配的 fd 编号（不实际分配）
        FcntlCmd::F_DUPFD_QUERY => {
            let fd_table = &proc_inner.fd_table;
            let soft_limit = fd_table.get_soft_limit();
            if arg >= soft_limit {
                return Err(SysErrNo::EINVAL);
            }
            for candidate in arg..soft_limit {
                if fd_table.try_get(candidate).is_none() {
                    return Ok(candidate);
                }
            }
            return Err(SysErrNo::EMFILE);
        }

        // -------------------------------------------------------------------
        // pipe 大小
        // -------------------------------------------------------------------
        FcntlCmd::F_SETPIPE_SZ => {
            // 暂不支持动态调整 pipe 大小
            return Err(SysErrNo::EINVAL);
        }
        FcntlCmd::F_GETPIPE_SZ => {
            // 默认 Linux pipe 容量为 16 页（64 KiB）
            return Ok(65536);
        }

        _ => {
            return Err(SysErrNo::EINVAL);
        }
    }
    Ok(0)
}

fn parse_proc_pid_file(path: &str, name: &str) -> Option<usize> {
    let rest = path.strip_prefix("/proc/")?;
    let pid = rest.strip_suffix(name)?;
    let pid = pid.strip_suffix('/')?;
    pid.parse::<usize>().ok()
}

/// 参考 https://man7.org/linux/man-pages/man2/openat.2.html
pub fn sys_openat(dirfd: isize, path: *const u8, flags: u32, mode: u32) -> SyscallRet {
    debug!(
        "[sys_openat] dirfd={}, path={:x}, flags={:x}, mode={}",
        dirfd, path as u64, flags, mode
    );
    if path as usize == 0 {
        return Err(SysErrNo::ENOENT);
    }

    let task = current_task().unwrap();
    let proc_inner = &task.process;
    let memory_set = proc_inner.memory_set_arc();
    let fd_table = proc_inner.fd_table.clone();
    let fs_info = proc_inner.fs_info.clone();
    let path = read_user_cstr(&*memory_set, path)?;
    drop(memory_set);

    let mut flags = OpenFlags::from_bits(flags).unwrap();

    let mut abs_path = proc_inner.get_abs_path(dirfd, &path)?;
    debug!(
        "[sys_openat] path is {}, flags is {:?}, mode is {:o}",
        &abs_path, flags, mode
    );

    if flags.contains(OpenFlags::O_TMPFILE) {
        // O_TMPFILE takes a directory path but returns an unnamed regular file
        // fd. The file must not be inserted into the directory until a later
        // linkat("/proc/self/fd/<fd>", ...) materializes it.
        if flags.bits() & OpenFlags::O_ACCMODE.bits() == OpenFlags::O_RDONLY.bits() {
            return Err(SysErrNo::EINVAL);
        }
        // Resolve the directory without calling high-level open(): sys_openat
        // already holds process state, and open() can re-enter those locks.
        let dir_inode = if FsIndex::has_inode(&abs_path) {
            FsIndex::find_inode_idx(&abs_path).ok_or(SysErrNo::ENOENT)?
        } else {
            let inode = superblock_root_inode().find(&abs_path, OpenFlags::O_DIRECTORY, 0)?;
            FsIndex::insert_inode_idx(&abs_path, inode.clone());
            inode
        };
        if !dir_inode.types().is_dir() {
            return Err(SysErrNo::ENOTDIR);
        }

        let (readable, writable) = flags.read_write();
        let task_inner = task.inner_lock();
        let uid = task_inner.effective_uid;
        let gid = task_inner.effective_gid;
        drop(task_inner);

        let effective_mode = mode & !fs_info.get_umask();
        flags.remove(OpenFlags::O_TMPFILE);
        flags.remove(OpenFlags::O_DIRECTORY);
        let file = FileClass::Abs(TmpFile::new(readable, writable, effective_mode, uid, gid));
        let new_fd = fd_table.alloc_fd()?;
        fd_table.set(new_fd, FileDescriptor::new(flags, file));
        // Record a procfd target string for readlink(/proc/self/fd/<fd>). The
        // "(deleted)" suffix reflects that the tmpfile currently has no name.
        fs_info.insert(
            format!("{}/#tmpfile-{} (deleted)", abs_path, new_fd),
            new_fd,
        );
        return Ok(new_fd);
    }

    if abs_path == "/proc/self/stat" {
        abs_path = format!("/proc/{}/stat", task.pid());
    }
    if abs_path == "/proc/self/maps" {
        abs_path = format!("/proc/{}/maps", task.pid());
    }
    if abs_path == "/proc/self/status" {
        let proc_inner = &task.process;
        let memory_set = proc_inner.memory_set_arc();
        refresh_proc_status(task.pid(), task.ppid(), &memory_set)?;
        abs_path = format!("/proc/{}/status", task.pid());
    }
    if let Some(pid) = parse_proc_pid_file(&abs_path, "stat") {
        if let Some(process) = Process::get_process_arc_by_pid(pid) {
            let ppid = process.ppid();
            let state = if process.all_tasks_exited() { 'Z' } else { 'S' };
            let proc_inner = &process;
            let memory_set = proc_inner.memory_set_arc();
            refresh_proc_stat(pid, ppid, state, &memory_set)?;
        }
    }
    if let Some(pid) = parse_proc_pid_file(&abs_path, "status") {
        if let Some(process) = Process::get_process_arc_by_pid(pid) {
            let proc_inner = &process;
            let memory_set = proc_inner.memory_set_arc();
            refresh_proc_status(pid, process.ppid(), &memory_set)?;
        }
    }

    // 动态库路径重定向：将动态链接器请求的标准路径映射到实际文件位置
    let abs_path = map_dynamic_link_file(&abs_path).to_string();

    let inode = open(&abs_path, flags, mode)?;
    let inode = match inode {
        FileClass::File(osfile) => {
            let inode_type =
                FsIndex::special_node_type(&abs_path).unwrap_or_else(|| osfile.inode.types());
            if inode_type.is_fifo() {
                FileClass::Abs(open_fifo(&abs_path, flags)?)
            } else {
                FileClass::File(osfile)
            }
        }
        other => other,
    };
    let new_fd = fd_table.alloc_fd()?;
    fd_table.set(new_fd, FileDescriptor::new(flags, inode));

    fs_info.insert(abs_path, new_fd);
    return Ok(new_fd);
}

/// 参考 https://man7.org/linux/man-pages/man2/close.2.html
pub fn sys_close(fd: usize) -> SyscallRet {
    let task = current_task().unwrap();
    let inner = &task.process; // 拿到锁就不用调用get_fd_table了
    let fd_table = inner.fd_table.clone();
    // debug!("[sys_close] fd is {}", fd);

    if (fd as isize) < 0 || fd >= fd_table.len() {
        return Err(SysErrNo::EBADF);
    }

    // 在关闭前获取 inode 路径，用于 FsIndex 缓存淘汰
    let inode_path = fd_table
        .try_get(fd)
        .and_then(|desc| desc.file().ok())
        .map(|osfile| osfile.inode.path());

    if fd_table.try_get(fd).is_none() {
        return Ok(0);
    }

    fd_table.take(fd);
    inner.fs_info.remove(fd);

    // 若该 inode 仅被全局缓存持有，则淘汰以释放堆内存
    if let Some(path) = inode_path {
        if !path.is_empty() && !path.starts_with("/proc") {
            if let Some(inode) = FsIndex::find_inode_idx(&path) {
                // FsIndex 持有一份引用，find_inode_idx 返回的 clone 是第二份
                if Arc::strong_count(&inode) <= 2 {
                    FsIndex::remove_inode_idx(&path);
                }
            }
        }
    }

    Ok(0)
}

bitflags! {
    struct CloseRangeFlags: u32{
        const UNSHARE = 1 << 1;
        const CLOEXEC = 1 << 2;
    }
}

/// 参考 https://man7.org/linux/man-pages/man2/close_range.2.html
pub fn sys_close_range(first: u32, last: u32, flags: u32) -> SyscallRet {
    if first > last {
        return Err(SysErrNo::EINVAL);
    }
    let flags = CloseRangeFlags::from_bits(flags).ok_or(SysErrNo::EINVAL)?;
    debug!(
        "[sys_close_range] first={}, last={}, flags={:?}",
        first, last, flags
    );

    // TODO: UNSHARE flag support
    // UNSHARE (1 << 1): Copy-on-write on all file descriptors in the range
    // This requires copying the fd_table
    if flags.contains(CloseRangeFlags::UNSHARE) {
        // TODO: Implement UNSHARE functionality
        // This should create a copy-on-write snapshot of the fd_table for the range
        // For now, we'll skip this flag
        return Err(SysErrNo::EINVAL);
    }

    // CLOEXEC (1 << 2): Close all file descriptors in the range on exec
    if flags.contains(CloseRangeFlags::CLOEXEC) {
        let task = current_task().unwrap();
        let proc_inner = &task.process;
        for fd in first..=last {
            if fd as usize >= proc_inner.fd_table.len() {
                continue;
            }
            // Try to get the file descriptor, ignore errors
            if let Some(mut desc) = proc_inner.fd_table.try_get(fd as usize) {
                desc.set_cloexec();
            }
        }
    } else {
        // Close all file descriptors in the range
        let task = current_task().unwrap();
        let proc_inner = &task.process;

        for fd in first..=last {
            if fd as usize >= proc_inner.fd_table.len() {
                continue;
            }

            // Get inode path for FsIndex cache eviction before closing
            let inode_path = proc_inner
                .fd_table
                .try_get(fd as usize)
                .and_then(|desc| desc.file().ok())
                .map(|osfile| osfile.inode.path());

            // Remove from fd_table
            if let Some(_) = proc_inner.fd_table.take(fd as usize) {
                // Remove from fs_info
                proc_inner.fs_info.remove(fd as usize);
            }

            // Evict inode from FsIndex cache if it's no longer referenced
            if let Some(path) = inode_path {
                if !path.is_empty() && !path.starts_with("/proc") {
                    if let Some(inode) = FsIndex::find_inode_idx(&path) {
                        // FsIndex holds one reference, find_inode_idx returns clone as second reference
                        if Arc::strong_count(&inode) <= 2 {
                            FsIndex::remove_inode_idx(&path);
                        }
                    }
                }
            }
        }
    }

    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/openat2.2.html
///
/// openat 的扩展版本，通过 struct open_how 传递 flags / mode / resolve。
///
/// # 参数
/// - `dirfd`: 目录文件描述符，AT_FDCWD 表示当前工作目录
/// - `path`: 要打开的路径（用户空间指针）
/// - `how`: 指向 struct open_how 的指针
/// - `usize`: sizeof(struct open_how)，应 >= 24
///
/// struct open_how { __u64 flags; __u64 mode; __u64 resolve; }
///
/// 当前实现忽略 resolve 字段，直接委托给 sys_openat。
pub fn sys_openat2(
    dirfd: isize,
    path: *const u8,
    how: *const open_how,
    usize: usize,
) -> SyscallRet {
    debug!(
        "[sys_openat2] dirfd={}, path={:x}, how={:x}, usize={}",
        dirfd, path as usize, how as usize, usize
    );

    // EINVAL: usize 必须至少为 sizeof(open_how)
    if usize < core::mem::size_of::<open_how>() {
        return Err(SysErrNo::EINVAL);
    }

    // EFAULT: how 必须有效
    if how.is_null() || (how as isize) <= 0 || if_bad_address(how as usize) {
        return Err(SysErrNo::EFAULT);
    }

    let task = current_task().unwrap();
    let proc_inner = &task.process;
    let memory_set = proc_inner.memory_set_arc();

    // 从用户空间读取 open_how 结构
    let mut open_how_val = open_how {
        flags: 0,
        mode: 0,
        resolve: 0,
    };
    copy_from_user(&memory_set, how as usize, unsafe {
        core::slice::from_raw_parts_mut(
            &mut open_how_val as *mut open_how as *mut u8,
            core::mem::size_of::<open_how>(),
        )
    })?;

    // 释放锁，委托给 sys_openat（它会重新获取自己的锁）
    drop(memory_set);
    drop(task);

    debug!(
        "[sys_openat2] flags=0x{:x}, mode=0{:o}, resolve=0x{:x}",
        open_how_val.flags, open_how_val.mode, open_how_val.resolve
    );

    // 如果调用者明确要求了尚不支持的 resolve 特性，返回 EOPNOTSUPP
    // resolve != 0 时，检查是否仅包含已知标志
    if open_how_val.resolve != 0 {
        // RESOLVE_CACHED (32) 是可接受的（仅用于 vfs 缓存提示）
        if open_how_val.resolve & !32u64 != 0 {
            warn!(
                "[sys_openat2] unsupported resolve flags: 0x{:x}",
                open_how_val.resolve
            );
            // 对于不支持的严格 resolve 标志，返回 EINVAL 而不是静默忽略
            return Err(SysErrNo::EINVAL);
        }
    }

    // 委托给 sys_openat
    sys_openat(
        dirfd,
        path,
        open_how_val.flags as u32,
        open_how_val.mode as u32,
    )
}
