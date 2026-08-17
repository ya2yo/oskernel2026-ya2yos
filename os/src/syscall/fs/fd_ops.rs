use core::{future::poll_fn, task::Poll};

use super::fcntl::*;
use super::file_lock;
use super::path::parse_proc_self_fd;
use crate::arch::memory_layout::PAGE_SIZE;
use crate::fs::{
    ensure_proc_dir, ensure_proc_path, notify_path_event, open, open_fifo, refresh_proc_maps,
    refresh_proc_stat, refresh_proc_status, superblock_root_inode, File, FileClass, FileDescriptor,
    FsIndex, OpenFlags, PagemapFile, TmpFile, FAN_OPEN, MNT_TABLE,
};
use crate::mm::{copy_from_user, if_bad_address, translate::read_user_cstr};
use crate::syscall::fs::has_too_long_path_component;
use crate::syscall::{FileMode, Syscall};
use crate::task::{block_on, current_task, interruptible, Process};
use crate::utils::{get_abs_path, is_abs_path, SysErrNo, SyscallRet};
use alloc::{
    format,
    string::{String, ToString},
    sync::Arc,
    vec,
    vec::Vec,
};
use linux_raw_sys::general::{
    open_how, AT_FDCWD, CAP_FSETID, LOCK_EX, LOCK_NB, LOCK_SH, LOCK_UN, O_CLOEXEC, RESOLVE_BENEATH,
    RESOLVE_CACHED, RESOLVE_IN_ROOT, RESOLVE_NO_MAGICLINKS, RESOLVE_NO_SYMLINKS, RESOLVE_NO_XDEV,
};
use log::{debug, error};

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
    let valid_mask = (LOCK_SH | LOCK_EX | LOCK_NB | LOCK_UN) as i32;
    if op & !valid_mask != 0 {
        return Err(SysErrNo::EINVAL);
    }

    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let proc = &task.process;
    let fd_desc = proc.fd_table.get(fd as usize)?;

    // flock 仅适用于普通文件（OSFile），非普通文件返回 EINVAL
    let osfile = fd_desc.file()?;

    let inode_path = osfile.inode.path();
    let file_ptr = Arc::as_ptr(&osfile) as usize;

    // --- 解锁 ---
    if (op & LOCK_UN as i32) != 0 {
        file_lock::flock_unlock(&inode_path, file_ptr);
        return Ok(0);
    }

    // --- 加锁 ---
    let lock_type = op & (LOCK_SH | LOCK_EX) as i32;
    if lock_type != LOCK_SH as i32 && lock_type != LOCK_EX as i32 {
        return Err(SysErrNo::EINVAL);
    }

    let nonblock = (op & LOCK_NB as i32) != 0;

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
    let proc = &task.process;
    let mut new_desc = proc.fd_table.get(old_fd)?;
    if cloexec {
        new_desc.set_cloexec();
    } else {
        new_desc.unset_cloexec();
    }
    let new_fd = proc.fd_table.alloc_fd()?;
    if let Err(e) = proc.fd_table.set(new_fd, new_desc) {
        proc.fd_table.take(new_fd);
        return Err(e);
    }
    proc.fs_info.dup_fd_path(old_fd, new_fd);
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
    let proc = &task.process;

    // debug!(
    //     "[sys_dup3] : oldfd is {}, newfd is {}, flags is {}",
    //     old, new, flags
    // );
    if old == new {
        return Err(SysErrNo::EINVAL);
    }
    if old >= proc.fd_table.len() || new >= proc.fd_table.get_soft_limit() {
        return Err(SysErrNo::EMFILE); // 添加文件描述符耗尽检查
    }

    if old >= proc.fd_table.len()
        || (old as isize) < 0
        || (new as isize) < 0
        || new >= proc.fd_table.get_soft_limit()
    {
        error!("lots of");
        return Err(SysErrNo::EBADF);
    }
    // 检查文件描述符表是否已满
    if proc.fd_table.try_get(old).is_none() {
        return Err(SysErrNo::EINVAL);
    }

    if proc.fd_table.len() <= new {
        proc.fd_table.resize(new + 1)?;
    }

    let mut file = proc.fd_table.get(old)?;
    if flags & !O_CLOEXEC != 0 {
        return Err(SysErrNo::EINVAL);
    }
    if flags & O_CLOEXEC != 0 {
        // flags 包含 O_CLOEXEC，为新的 fd 设置该标志。
        file.set_cloexec();
    } else {
        file.unset_cloexec();
    }
    proc.fd_table.set(new, file);
    Ok(new)
}

fn parse_proc_pid_file(path: &str, name: &str) -> Option<usize> {
    let rest = path.strip_prefix("/proc/")?;
    let pid = rest.strip_suffix(name)?;
    let pid = pid.strip_suffix('/')?;
    pid.parse::<usize>().ok()
}

/// Follow the procfs magic-link view of a reopenable anonymous file. Unlike
/// dup(2), opening `/proc/self/fd/<n>` creates a fresh open-file description
/// with its own offset while retaining the same underlying memfd seals/data.
fn open_proc_self_fd(
    fd_table: &Arc<crate::fs::FdTable>,
    fs_info: &Arc<crate::fs::FSInfo>,
    source_fd: usize,
    flags: OpenFlags,
) -> SyscallRet {
    if flags.contains(OpenFlags::O_NOFOLLOW) {
        return Err(SysErrNo::ELOOP);
    }
    if flags.contains(OpenFlags::O_CREATE) && flags.contains(OpenFlags::O_EXCL) {
        return Err(SysErrNo::EEXIST);
    }

    let source = fd_table.get(source_fd)?;
    let (readable, writable) = flags.read_write();
    let file = source
        .abs()?
        .reopen(readable, writable, flags.contains(OpenFlags::O_APPEND))?;

    if flags.contains(OpenFlags::O_DIRECTORY) {
        return Err(SysErrNo::ENOTDIR);
    }
    if !flags.contains(OpenFlags::O_PATH) && flags.contains(OpenFlags::O_TRUNC) {
        file.truncate(0)?;
    }

    let new_fd = fd_table.alloc_fd()?;
    if let Err(err) = fd_table.set(new_fd, FileDescriptor::new(flags, FileClass::Abs(file))) {
        fd_table.take(new_fd);
        return Err(err);
    }
    fs_info.dup_fd_path(source_fd, new_fd);
    Ok(new_fd)
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
    let proc = &task.process;
    let memory_set = proc.memory_set_arc();
    let path = read_user_cstr(&*memory_set, path)?;
    drop(memory_set);

    sys_openat_path(dirfd, &path, flags, mode)
}

/// `openat` 的内核路径入口。用户指针解码保留在 [`sys_openat`]；`openat2` 完成
/// 自己的 ABI 与 resolve 校验后也复用此处，避免两套打开语义发生偏差。
fn sys_openat_path(dirfd: isize, path: &str, flags: u32, mode: u32) -> SyscallRet {
    if has_too_long_path_component(&path) {
        return Err(SysErrNo::ENAMETOOLONG);
    }

    let task = current_task().unwrap();
    let proc = &task.process;
    let fd_table = proc.fd_table.clone();
    let fs_info = proc.fs_info.clone();

    let mut flags = OpenFlags::from_bits(flags).ok_or(SysErrNo::EINVAL)?;

    let mut abs_path = proc.get_abs_path(dirfd, &path)?;
    debug!(
        "[sys_openat] path is {}, flags is {:?}, mode is {:o}",
        &abs_path, flags, mode
    );

    if let Some(source_fd) = parse_proc_self_fd(&abs_path) {
        return open_proc_self_fd(&fd_table, &fs_info, source_fd, flags);
    }

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
            FsIndex::insert_inode_idx(&abs_path, inode)
        };
        if !dir_inode.types().is_dir() {
            return Err(SysErrNo::ENOTDIR);
        }
        let parent_stat = dir_inode.fstat();

        let (readable, writable) = flags.read_write();
        let task_inner = task.inner_lock();
        let uid = task_inner.effective_uid;
        let gid = task_inner.effective_gid;
        let cap = CAP_FSETID as usize;
        let has_cap_fsetid =
            task_inner.capabilities.effective[cap / 32] & (1u32 << (cap % 32)) != 0;
        drop(task_inner);

        // Linux strips S_ISGID before applying the umask.  In particular, a
        // umask that clears S_IXGRP must not turn an unprivileged setgid-file
        // request into a mandatory-locking marker after the security check.
        let setgid = FileMode::S_ISGID.bits();
        let setgid_and_group_execute = (FileMode::S_ISGID | FileMode::S_IXGRP).bits();
        let mut effective_mode = mode;
        if mode & setgid_and_group_execute == setgid_and_group_execute
            && parent_stat.st_mode & setgid != 0
            && gid != parent_stat.st_gid
            && !has_cap_fsetid
        {
            effective_mode &= !setgid;
        }
        effective_mode &= !fs_info.get_umask();
        let file_gid = if parent_stat.st_mode & setgid != 0 {
            parent_stat.st_gid
        } else {
            gid
        };
        flags.remove(OpenFlags::O_TMPFILE);
        flags.remove(OpenFlags::O_DIRECTORY);
        let file = FileClass::Abs(TmpFile::new(
            readable,
            writable,
            effective_mode,
            uid,
            file_gid,
        ));
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
    if abs_path == "/proc/self/pagemap" {
        abs_path = format!("/proc/{}/pagemap", task.pid());
    }
    if abs_path == "/proc/self/status" {
        ensure_proc_dir(task.pid())?;
        // 锁顺序: ProcessMeta(2) → TaskControlBlockInner(3) → MemorySet(5)
        // 必须先获取 meta/inner 再获取 memory_set
        let comm = task.process.meta_lock().comm.clone();
        let task_inner = task.inner_lock();
        let real_uid = task_inner.user_id as u32;
        let effective_uid = task_inner.effective_uid;
        let saved_uid = task_inner.saved_uid;
        let real_gid = task_inner.real_gid;
        let effective_gid = task_inner.effective_gid;
        let saved_gid = task_inner.saved_gid;
        drop(task_inner);
        let proc = &task.process;
        let memory_set = proc.memory_set_arc();
        refresh_proc_status(
            task.pid(),
            task.ppid(),
            &comm,
            &memory_set,
            real_uid,
            effective_uid,
            saved_uid,
            real_gid,
            effective_gid,
            saved_gid,
        )?;
        abs_path = format!("/proc/{}/status", task.pid());
    }
    if abs_path == "/proc/self/exe" {
        // `/proc/self/exe` is a procfs magic-link to the current executable.
        // `sys_openat2` has already rejected it when RESOLVE_NO_MAGICLINKS is set.
        abs_path = fs_info.get_exe();
    }
    ensure_proc_path(&abs_path)?;
    if let Some(pid) = parse_proc_pid_file(&abs_path, "stat") {
        if let Some(process) = Process::get_process_arc_by_pid(pid) {
            let ppid = process.ppid();
            let state = if process.all_tasks_exited() { 'Z' } else { 'S' };
            let proc = &process;
            let memory_set = proc.memory_set_arc();
            let comm = proc.meta_lock().comm.clone();
            refresh_proc_stat(pid, ppid, process.pgid(), state, &comm, &memory_set)?;
        }
    }
    if let Some(pid) = parse_proc_pid_file(&abs_path, "status") {
        if let Some(process) = Process::get_process_arc_by_pid(pid) {
            let proc = &process;
            // 锁顺序: ProcessMeta(2) → TaskControlBlockInner(3) → MemorySet(5)
            // 必须在获取 memory_set 之前先获取 meta/inner
            let (comm, real_uid, effective_uid, saved_uid, real_gid, effective_gid, saved_gid) = {
                let meta = proc.meta_lock();
                let comm = meta.comm.clone();
                let (real_uid, effective_uid, saved_uid, real_gid, effective_gid, saved_gid) =
                    if let Some(first_task) = meta.tasks.iter().find_map(|w| w.upgrade()) {
                        let inner = first_task.inner_lock();
                        (
                            inner.user_id as u32,
                            inner.effective_uid,
                            inner.saved_uid,
                            inner.real_gid,
                            inner.effective_gid,
                            inner.saved_gid,
                        )
                    } else {
                        (0u32, 0u32, 0u32, 0u32, 0u32, 0u32)
                    };
                (
                    comm,
                    real_uid,
                    effective_uid,
                    saved_uid,
                    real_gid,
                    effective_gid,
                    saved_gid,
                )
            };
            let memory_set = proc.memory_set_arc();
            refresh_proc_status(
                pid,
                process.ppid(),
                &comm,
                &memory_set,
                real_uid,
                effective_uid,
                saved_uid,
                real_gid,
                effective_gid,
                saved_gid,
            )?;
        }
    }
    if let Some(pid) = parse_proc_pid_file(&abs_path, "maps") {
        if let Some(process) = Process::get_process_arc_by_pid(pid) {
            let memory_set = process.memory_set_arc();
            refresh_proc_maps(pid, &memory_set)?;
        }
    }
    if let Some(pid) = parse_proc_pid_file(&abs_path, "pagemap") {
        if flags.read_write().1 {
            return Err(SysErrNo::EACCES);
        }
        if flags.contains(OpenFlags::O_CREATE) && flags.contains(OpenFlags::O_EXCL) {
            return Err(SysErrNo::EEXIST);
        }
        let process = Process::get_process_arc_by_pid(pid).ok_or(SysErrNo::ENOENT)?;
        let file = FileClass::Abs(PagemapFile::open(
            process.memory_set_arc(),
            abs_path.clone(),
        ));
        let new_fd = fd_table.alloc_fd()?;
        fd_table.set(new_fd, FileDescriptor::new(flags, file));
        notify_path_event(&abs_path, FAN_OPEN);
        fs_info.insert(abs_path, new_fd);
        return Ok(new_fd);
    }

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
    // Capture the resolved inode path before moving `inode` into the fd table.
    // realpath(3) (musl in particular) reads /proc/self/fd/N and must see the
    // resolved target rather than the symlink alias.  fanotify still reports
    // the original path requested by userspace.
    // Only FileClass::File carries an Inode whose path() reflects the
    // symlink-resolved location; abstract files (device nodes, pipes, etc.)
    // keep the original abs_path because their File::path() may panic.
    let fd_path = match &inode {
        FileClass::File(osfile) => osfile.inode.path(),
        _ => abs_path.clone(),
    };
    fd_table.set(new_fd, FileDescriptor::new(flags, inode));

    notify_path_event(&abs_path, FAN_OPEN);
    fs_info.insert(fd_path, new_fd);
    return Ok(new_fd);
}

/// 参考 https://man7.org/linux/man-pages/man2/close.2.html
pub fn sys_close(fd: usize) -> SyscallRet {
    let task = current_task().unwrap();
    let owner_pid = task.pid() as i32;
    let inner = &task.process; // 拿到锁就不用调用get_fd_table了
    let fd_table = inner.fd_table.clone();
    // debug!("[sys_close] fd is {}", fd);

    if (fd as isize) < 0 || fd >= fd_table.len() {
        return Err(SysErrNo::EBADF);
    }

    if fd_table.try_get(fd).is_none() {
        return Ok(0);
    }

    if let Some(desc) = fd_table.close(fd) {
        if let Ok(osfile) = desc.file() {
            let path = osfile.inode.path();
            file_lock::release_posix_locks(&path, owner_pid);
            file_lock::release_file_leases(&path, owner_pid);
            if Arc::strong_count(&osfile) <= 2 {
                file_lock::release_posix_locks(&path, osfile.ofd_lock_owner());
            }
        }
        inner.fs_info.remove(fd);
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
        let proc = &task.process;
        for fd in first..=last {
            if fd as usize >= proc.fd_table.len() {
                continue;
            }
            // Try to get the file descriptor, ignore errors
            if let Some(mut desc) = proc.fd_table.try_get(fd as usize) {
                desc.set_cloexec();
            }
        }
    } else {
        // Close all file descriptors in the range
        let task = current_task().unwrap();
        let owner_pid = task.pid() as i32;
        let proc = &task.process;

        for fd in first..=last {
            if fd as usize >= proc.fd_table.len() {
                continue;
            }

            // Remove from fd_table
            if let Some(desc) = proc.fd_table.close(fd as usize) {
                if let Ok(osfile) = desc.file() {
                    let path = osfile.inode.path();
                    file_lock::release_posix_locks(&path, owner_pid);
                    file_lock::release_file_leases(&path, owner_pid);
                    if Arc::strong_count(&osfile) <= 2 {
                        file_lock::release_posix_locks(&path, osfile.ofd_lock_owner());
                    }
                }
                // Remove from fs_info
                proc.fs_info.remove(fd as usize);
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
/// `struct open_how { __u64 flags; __u64 mode; __u64 resolve; }`
/// 的已知 resolve 掩码。
const RESOLVE_KNOWN: u64 = (RESOLVE_NO_XDEV
    | RESOLVE_NO_MAGICLINKS
    | RESOLVE_NO_SYMLINKS
    | RESOLVE_BENEATH
    | RESOLVE_IN_ROOT
    | RESOLVE_CACHED) as u64;

/// 返回路径所在的挂载根。`/proc` 目前由 rootfs 内的伪文件模拟，但仍须作为
/// `RESOLVE_NO_XDEV` 可见的独立挂载点处理。
fn openat2_mount_root(path: &str) -> String {
    if path == "/proc" || path.starts_with("/proc/") {
        return String::from("/proc");
    }

    MNT_TABLE
        .lock()
        .mount_for_path(path)
        .map(|(_, dir, _, _)| dir)
        .unwrap_or_else(|| String::from("/"))
}

/// `RESOLVE_BENEATH` 只允许相对路径在起点之下移动；任何试图越过起点的
/// `..` 都必须返回 `EXDEV`。
fn openat2_escapes_beneath(path: &str) -> bool {
    if is_abs_path(path) {
        return true;
    }

    let mut depth = 0usize;
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." if depth == 0 => return true,
            ".." => depth -= 1,
            _ => depth += 1,
        }
    }
    false
}

/// `RESOLVE_IN_ROOT` 将绝对路径和超出起点的 `..` 都限制在 dirfd 指向的根中。
fn openat2_in_root_path(path: &str) -> String {
    let mut components = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                components.pop();
            }
            _ => components.push(component),
        }
    }
    components.join("/")
}

/// procfs 的这些入口并非普通 inode symlink，而是随进程状态动态解析的
/// magic-link；`RESOLVE_NO_MAGICLINKS` 必须拒绝它们。
fn openat2_is_magic_link(path: &str) -> bool {
    let mut components = path.trim_start_matches('/').split('/');
    if components.next() != Some("proc") {
        return false;
    }

    let Some(pid) = components.next() else {
        return false;
    };
    if pid != "self" && pid.parse::<usize>().is_err() {
        return false;
    }

    match components.next() {
        Some("exe" | "cwd" | "root") => true,
        Some("fd") => components.next().is_some(),
        _ => false,
    }
}

/// 获取 `openat2` 路径解析的起点。普通 `openat` 在真正打开前也会做同样的
/// dirfd 校验；这里提前取得它，以便 resolve 约束可以在进入 VFS 前生效。
fn openat2_base_path(process: &Process, dirfd: isize) -> Result<String, SysErrNo> {
    if dirfd == AT_FDCWD as isize {
        Ok(process.fs_info.get_cwd())
    } else {
        process
            .fd_table
            .get(dirfd as usize)?
            .file()
            .map(|file| file.inode.path())
    }
}

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

    let how_size = core::mem::size_of::<open_how>();
    if usize < how_size {
        return Err(SysErrNo::EINVAL);
    }

    // EFAULT: how 必须有效
    if how.is_null() || (how as isize) <= 0 || if_bad_address(how as usize) {
        return Err(SysErrNo::EFAULT);
    }

    let task = current_task().unwrap();
    let proc = &task.process;
    let memory_set = proc.memory_set_arc();

    // 从用户空间读取 open_how 结构。比已知 ABI 更长的全零尾部可以向前兼容；
    // 不可读尾部返回 EFAULT，非零字段表示调用者需要更新的 ABI，返回 E2BIG。
    let mut open_how_val = open_how {
        flags: 0,
        mode: 0,
        resolve: 0,
    };
    copy_from_user(&memory_set, how as usize, unsafe {
        core::slice::from_raw_parts_mut(&mut open_how_val as *mut open_how as *mut u8, how_size)
    })?;

    let extra_size = usize - how_size;
    if extra_size > PAGE_SIZE {
        return Err(SysErrNo::E2BIG);
    }
    let mut offset = how_size;
    while offset < usize {
        let chunk_len = (usize - offset).min(64);
        let mut extra = [0u8; 64];
        let extra_addr = (how as usize).checked_add(offset).ok_or(SysErrNo::EFAULT)?;
        copy_from_user(&memory_set, extra_addr, &mut extra[..chunk_len])?;
        if extra[..chunk_len].iter().any(|byte| *byte != 0) {
            return Err(SysErrNo::E2BIG);
        }
        offset += chunk_len;
    }

    if path.is_null() || (path as isize) <= 0 || if_bad_address(path as usize) {
        return Err(SysErrNo::EFAULT);
    }
    let path = read_user_cstr(&memory_set, path)?;
    if has_too_long_path_component(&path) {
        return Err(SysErrNo::ENAMETOOLONG);
    }

    let allowed_open_flags = OpenFlags::all().bits() & !OpenFlags::O_UNLINK.bits();
    if open_how_val.flags & !(allowed_open_flags as u64) != 0 {
        return Err(SysErrNo::EINVAL);
    }
    let flags = open_how_val.flags as u32;
    let flags = OpenFlags::from_bits(flags).ok_or(SysErrNo::EINVAL)?;
    let creates = flags.contains(OpenFlags::O_CREATE) || flags.contains(OpenFlags::O_TMPFILE);
    let valid_create_mode = (FileMode::S_ISUID
        | FileMode::S_ISGID
        | FileMode::S_ISVTX
        | FileMode::S_IRWXU
        | FileMode::S_IRWXG
        | FileMode::S_IRWXO)
        .bits() as u64;
    if open_how_val.mode > valid_create_mode || (!creates && open_how_val.mode != 0) {
        return Err(SysErrNo::EINVAL);
    }
    if open_how_val.resolve & !RESOLVE_KNOWN != 0 {
        return Err(SysErrNo::EINVAL);
    }

    let base_path = openat2_base_path(proc, dirfd)?;
    if open_how_val.resolve & RESOLVE_BENEATH as u64 != 0 && openat2_escapes_beneath(&path) {
        return Err(SysErrNo::EXDEV);
    }
    let relative_path = if open_how_val.resolve & RESOLVE_IN_ROOT as u64 != 0 {
        openat2_in_root_path(&path)
    } else {
        path.clone()
    };
    let resolved_path = get_abs_path(&base_path, &relative_path);
    if open_how_val.resolve & RESOLVE_NO_XDEV as u64 != 0
        && openat2_mount_root(&base_path) != openat2_mount_root(&resolved_path)
    {
        return Err(SysErrNo::EXDEV);
    }
    if open_how_val.resolve & RESOLVE_NO_MAGICLINKS as u64 != 0
        && openat2_is_magic_link(&resolved_path)
    {
        return Err(SysErrNo::ELOOP);
    }

    // 释放用户内存引用后复用普通 open 的内核路径入口。
    drop(memory_set);
    drop(task);

    debug!(
        "[sys_openat2] flags=0x{:x}, mode=0{:o}, resolve=0x{:x}",
        open_how_val.flags, open_how_val.mode, open_how_val.resolve
    );

    let mut open_flags = flags;
    if open_how_val.resolve & RESOLVE_NO_SYMLINKS as u64 != 0 {
        // 现有 VFS 的 O_NOFOLLOW 可准确拒绝末级 symlink，并绕开 dentry cache
        // 以确保底层路径查询返回 ELOOP。
        open_flags.insert(OpenFlags::O_NOFOLLOW);
    }

    sys_openat_path(
        AT_FDCWD as isize,
        &resolved_path,
        open_flags.bits(),
        open_how_val.mode as u32,
    )
}
