mod ctl;
mod event;
mod fcntl;
mod fd_ops;
mod file_lock;
mod handle;
mod io;
mod mount;
mod mqueue;
mod pipe;
mod stat;
mod xattr;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use linux_raw_sys::ctypes::c_int;
use log::warn;

use crate::{
    fs::{DummyFd, File, FileClass, FileDescriptor, InotifyFd, OpenFlags},
    mm::{copy_from_user, read_user_cstr, UserBuffer},
    syscall::options::Iovec,
    task::current_task,
    utils::{SysErrNo, SyscallRet},
};

// inotify_init1 标志 — 与 O_CLOEXEC / O_NONBLOCK 值相同
const IN_CLOEXEC: u32 = OpenFlags::O_CLOEXEC.bits();
const IN_NONBLOCK: u32 = OpenFlags::O_NONBLOCK.bits();

pub use self::{
    ctl::*, event::*, fcntl::*, fd_ops::*, handle::*, io::*, mount::*, mqueue::*, pipe::*, stat::*,
    xattr::*,
};

fn dummyfd_create() -> SyscallRet {
    let dummy_file = DummyFd::new();
    let task = current_task().unwrap();
    let proc_inner = task.process.inner_lock();
    let newfd = proc_inner.fd_table.alloc_fd()?;
    proc_inner.fd_table.set(
        newfd,
        FileDescriptor::new(OpenFlags::empty(), crate::fs::FileClass::Abs(dummy_file)),
    );
    Ok(newfd)
}

/// https://man7.org/linux/man-pages/man2/inotify_init1.2.html
pub fn sys_inotify_init1(flags: u32) -> SyscallRet {
    // 只允许 IN_CLOEXEC 和 IN_NONBLOCK 两个标志
    let valid_flags = IN_CLOEXEC | IN_NONBLOCK;
    if flags & !valid_flags != 0 {
        return Err(SysErrNo::EINVAL);
    }

    let inotify_file = InotifyFd::new();
    if flags & IN_NONBLOCK != 0 {
        inotify_file.set_nonblocking(true)?;
    }

    let mut open_flags = OpenFlags::O_RDWR;
    if flags & IN_CLOEXEC != 0 {
        open_flags |= OpenFlags::O_CLOEXEC;
    }
    if flags & IN_NONBLOCK != 0 {
        open_flags |= OpenFlags::O_NONBLOCK;
    }

    let task = current_task().unwrap();
    let proc_inner = task.process.inner_lock();
    let fd = proc_inner.fd_table.alloc_fd()?;
    proc_inner.fd_table.set(
        fd,
        FileDescriptor::new(open_flags, FileClass::Abs(inotify_file.clone())),
    )?;
    // 注册到全局表，供 add_watch / rm_watch 查找
    InotifyFd::register_fd(fd, &inotify_file);
    Ok(fd)
}

/// https://man7.org/linux/man-pages/man2/inotify_add_watch.2.html
pub fn sys_inotify_add_watch(fd: c_int, path: *const u8, mask: u32) -> SyscallRet {
    if fd < 0 {
        return Err(SysErrNo::EBADF);
    }
    let fd = fd as usize;
    let inotify = InotifyFd::lookup(fd)?;

    // 从用户空间读取路径字符串
    let path_str = {
        let task = current_task().unwrap();
        let process = task.process.inner_lock();
        let memory_set = process.get_locked_memory_set_read();
        read_user_cstr(&memory_set, path)?
    };

    if mask == 0 {
        return Err(SysErrNo::EINVAL);
    }

    inotify.add_watch(path_str, mask)
}

pub fn sys_inotify_rm_watch(fd: c_int, wd: c_int) -> SyscallRet {
    if fd < 0 {
        return Err(SysErrNo::EBADF);
    }
    let fd = fd as usize;
    let inotify = InotifyFd::lookup(fd)?;
    inotify.rm_watch(wd)
}

/// https://man7.org/linux/man-pages/man2/bpf.2.html
pub fn sys_bpf(_cmd: i32, _attr: *mut u8, _size: u32) -> SyscallRet {
    warn!("[sys_bpf] not implement!");
    dummyfd_create()
}

/// https://man7.org/linux/man-pages/man2/io_uring_setup.2.html
pub fn sys_io_uring_setup(_entriers: u32, _params: *mut u8) -> SyscallRet {
    warn!("[sys_io_uring_setup] not implement!");
    dummyfd_create()
}

/// https://man7.org/linux/man-pages/man2/memfd_create.2.html
pub fn sys_memfd_create(_name: *const u8, _flags: u32) -> SyscallRet {
    warn!("[sys_memfd_create] not implement!");
    dummyfd_create()
}

/// https://www.man7.org/linux/man-pages//man2/memfd_secret.2.html
pub fn sys_memfd_secret(_flags: u32) -> SyscallRet {
    warn!("[sys_memfd_secret] not implement!");
    dummyfd_create()
}

/// https://man7.org/linux/man-pages/man2/perf_event_open.2.html
pub fn sys_perf_event_open(
    _attr: *mut u8,
    _pid: u32,
    _cpu: c_int,
    _group_fd: c_int,
    _flags: u32,
) -> SyscallRet {
    warn!("[sys_perf_event_open] not implement!");
    dummyfd_create()
}

/// 参考 https://man7.org/linux/man-pages/man2/signalfd4.2.html
pub fn sys_signalfd4(_siglfd: u32, _mask: *const u8, _flags: u32) -> SyscallRet {
    warn!("[sys_signalfd4] not implement!");
    dummyfd_create()
}

/// https://www.man7.org/linux/man-pages/man2/timerfd_create.2.html
pub fn sys_timerfd_create(_clockid: u32, _flags: u32) -> SyscallRet {
    warn!("[sys_timerfd_create] not implement!");
    dummyfd_create()
}
pub fn sys_timerfd_settime(
    _fd: u32,
    _flags: u32,
    _new_value: *const u8,
    _old_value: *mut u8,
) -> SyscallRet {
    warn!("[sys_timerfd_settime] not implement!");
    Ok(0)
}
pub fn sys_timerfd_gettime(_fd: u32, _curr_value: *mut u8) -> SyscallRet {
    warn!("[sys_timerfd_gettime] not implement!");
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/vmsplice.2.html
///
/// 将用户空间 iovec 数据 splice 到 pipe 中。
///
/// # 参数
/// - `fd`: 目标 pipe 写端文件描述符
/// - `iov`: 指向用户空间 iovec 数组的指针
/// - `nr_segs`: iovec 数组元素个数（最大 1024）
/// - `flags`: SPLICE_F_MOVE / SPLICE_F_NONBLOCK / SPLICE_F_MORE / SPLICE_F_GIFT
///
/// # 返回值
/// 成功时返回实际写入 pipe 的字节数
pub fn sys_vmsplice(fd: i32, iov: usize, nr_segs: u32, flags: u32) -> SyscallRet {
    const SPLICE_F_MOVE: u32 = 0x01;
    const SPLICE_F_NONBLOCK: u32 = 0x02;
    const SPLICE_F_MORE: u32 = 0x04;
    const SPLICE_F_GIFT: u32 = 0x08;

    // 校验 flags 中不含未定义位
    let valid_flags = SPLICE_F_MOVE | SPLICE_F_NONBLOCK | SPLICE_F_MORE | SPLICE_F_GIFT;
    if flags & !valid_flags != 0 {
        return Err(SysErrNo::EINVAL);
    }

    // nr_segs 上限
    if nr_segs == 0 || nr_segs > 1024 {
        return Err(SysErrNo::EINVAL);
    }

    let task = current_task().unwrap();
    let proc_inner = task.process.inner_lock();
    let memory_set = proc_inner.get_locked_memory_set_read();
    let fd_table = proc_inner.fd_table.clone();
    let fd = fd as usize;

    if fd >= fd_table.len() {
        return Err(SysErrNo::EBADF);
    }

    let fd_entry = match fd_table.try_get(fd) {
        Some(f) => f,
        None => return Err(SysErrNo::EBADF),
    };

    // vmsplice 要求 fd 必须是 pipe（内部用 FileClass::Abs 表示）
    // abs() 对 FileClass::Abs 返回 Ok，对普通文件/套接字返回 Err → 映射为 EINVAL
    let file = fd_entry.abs().map_err(|_| SysErrNo::EINVAL)?;

    if !file.writable() {
        return Err(SysErrNo::EBADF);
    }

    // 遍历 iovec，从用户空间读取全部数据到内核缓冲区（在持锁状态下完成翻译）
    let iovec_size = core::mem::size_of::<Iovec>();
    let mut kernel_buf: Vec<u8> = Vec::new();

    for i in 0..nr_segs as usize {
        let current = (iov as usize) + iovec_size * i;
        let mut iov_buf = [0u8; core::mem::size_of::<Iovec>()];
        copy_from_user(&memory_set, current, &mut iov_buf)?;
        let iovinfo: Iovec = unsafe { core::mem::transmute(iov_buf) };
        if iovinfo.iov_len == 0 {
            continue;
        }
        let offset = kernel_buf.len();
        kernel_buf.resize(offset + iovinfo.iov_len, 0);
        copy_from_user(
            &memory_set,
            iovinfo.iov_base as usize,
            &mut kernel_buf[offset..],
        )?;
    }

    // 释放锁，避免 pipe write 阻塞时死锁
    drop(memory_set);
    drop(proc_inner);
    drop(task);

    let total_len = kernel_buf.len();
    if total_len == 0 {
        return Ok(0);
    }

    // 构造 UserBuffer 写入 pipe
    let mut ub_v = Vec::with_capacity(1);
    unsafe {
        ub_v.push(core::slice::from_raw_parts_mut(
            kernel_buf.as_mut_ptr(),
            total_len,
        ));
    }
    let ub = UserBuffer::new(ub_v);

    // pipe.write() 内部处理阻塞等待和 EINTR
    let ret = file.write(ub)?;
    Ok(ret)
}

/// 参考 https://man7.org/linux/man-pages/man2/splice.2.html
///
/// 在两个文件描述符之间零拷贝传输数据（至少一个必须是管道）。
/// 当前内核未实现零拷贝 splice 机制，始终返回 EINVAL。
pub fn sys_splice(
    _fd_in: i32,
    _off_in: *const i64,
    _fd_out: i32,
    _off_out: *const i64,
    _len: usize,
    _flags: u32,
) -> SyscallRet {
    log::debug!("[sys_splice] not implemented");
    Err(SysErrNo::EINVAL)
}

/// 参考 https://man7.org/linux/man-pages/man2/tee.2.html
///
/// 在两个管道之间复制数据而不消耗数据。
/// 当前内核未实现零拷贝 tee 机制，始终返回 EINVAL。
pub fn sys_tee(_fd_in: i32, _fd_out: i32, _len: usize, _flags: u32) -> SyscallRet {
    log::debug!("[sys_tee] not implemented");
    Err(SysErrNo::EINVAL)
}
