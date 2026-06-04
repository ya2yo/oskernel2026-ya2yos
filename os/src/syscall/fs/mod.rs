mod ctl;
mod event;
mod fcntl;
mod fd_ops;
mod file_lock;
mod io;
mod mount;
mod pipe;
mod stat;
mod xattr;

use alloc::vec::Vec;
use linux_raw_sys::ctypes::c_int;
use log::warn;

use crate::{
    fs::{DummyFd, FileDescriptor, OpenFlags},
    mm::{UserBuffer, copy_from_user, safe_translated_byte_buffer},
    syscall::options::Iovec,
    task::current_task,
    utils::{SysErrNo, SyscallRet},
};

pub use self::{
    ctl::*, event::*, fcntl::*, fd_ops::*, io::*, mount::*, pipe::*,
    stat::*, xattr::*,
};

fn dummyfd_create() -> SyscallRet {
    let dummy_file = DummyFd::new();
    let task = current_task().unwrap();
    let fd_table = task.get_fd_table();
    let newfd = fd_table.alloc_fd()?;
    task.get_fd_table()
        .set(newfd, FileDescriptor::new(OpenFlags::empty(), crate::fs::FileClass::Abs(dummy_file)));
    Ok(newfd)
}

/// https://man7.org/linux/man-pages/man2/inotify_init1.2.html
pub fn sys_inotify_init1(_flags: u32) -> SyscallRet {
    warn!("[sys_inotify_init1] not implement!");
    dummyfd_create()
}
/// https://man7.org/linux/man-pages/man2/inotify_add_watch.2.html
pub fn sys_inotify_add_watch(_fd: c_int, _path: *const u8, _mask: u32)-> SyscallRet {
    warn!("[sys_inotify_add_watch] not implement!");
    Ok(0)
}
pub fn sys_inotify_rm_watch(_fd: c_int, _wd: c_int) -> SyscallRet {
    warn!("[sys_inotify_rm_watch] not implement!");
    Ok(0)
}

/// https://man7.org/linux/man-pages/man2/bpf.2.html
pub fn sys_bpf(_cmd: i32, _attr: *mut u8, _size: u32) -> SyscallRet {
    warn!("[sys_bpf] not implement!");
    dummyfd_create()
}

/// https://man7.org/linux/man-pages/man2/io_uring_setup.2.html
pub fn sys_io_uring_setup(_entriers: u32, _params: *mut u8)->SyscallRet {
    warn!("[sys_io_uring_setup] not implement!");
    dummyfd_create()
}

/// https://man7.org/linux/man-pages/man2/memfd_create.2.html
pub fn sys_memfd_create(_name: *const u8, _flags: u32)->SyscallRet {
    warn!("[sys_memfd_create] not implement!");
    dummyfd_create()
}

/// https://www.man7.org/linux/man-pages//man2/memfd_secret.2.html
pub fn sys_memfd_secret(_flags: u32) -> SyscallRet {
    warn!("[sys_memfd_secret] not implement!");
    dummyfd_create()
}

/// https://man7.org/linux/man-pages/man2/perf_event_open.2.html
pub fn sys_perf_event_open(_attr: *mut u8, _pid: u32, _cpu: c_int, _group_fd: c_int, _flags: u32) -> SyscallRet {
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
        let current =  (iov as usize) + iovec_size * i ;
        let mut iov_buf = [0u8; core::mem::size_of::<Iovec>()];
        copy_from_user(&memory_set, current, &mut iov_buf)?;
        let iovinfo: Iovec = unsafe { core::mem::transmute(iov_buf) };
        if iovinfo.iov_len == 0 {
            continue;
        }
        // 翻译用户空间地址，逐个片段复制
        let buf_slices =
            safe_translated_byte_buffer(&memory_set, iovinfo.iov_base as *mut u8, iovinfo.iov_len)
                .ok_or(SysErrNo::EFAULT)?;
        for slice in buf_slices {
            kernel_buf.extend_from_slice(slice);
        }
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