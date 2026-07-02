use alloc::vec::Vec;

use crate::{
    fs::File,
    mm::{UserBuffer, copy_from_user, copy_from_user_val},
    syscall::options::Iovec,
    task::current_task,
    utils::{SysErrNo, SyscallRet},
};

const SPLICE_F_MOVE: u32 = 0x01;
const SPLICE_F_NONBLOCK: u32 = 0x02;
const SPLICE_F_MORE: u32 = 0x04;
const SPLICE_F_GIFT: u32 = 0x08;

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
    let proc = &task.process;
    let memory_set = proc.memory_set_arc();
    let fd_table = proc.fd_table_arc();
    let fd = fd as usize;

    if fd >= fd_table.len() {
        return Err(SysErrNo::EBADF);
    }

    let fd_entry = match fd_table.try_get(fd) {
        Some(f) => f,
        None => return Err(SysErrNo::EBADF),
    };

    // vmsplice 要求 fd 必须是 pipe；Pipe 作为文件对象统一由 FileClass::Pipe 管理。
    let file = fd_entry.pipe().map_err(|_| SysErrNo::EINVAL)?;

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
    fd_in: i32,
    off_in: *const i64,
    fd_out: i32,
    off_out: *const i64,
    len: usize,
    flags: u32,
) -> SyscallRet {
    log::debug!(
        "[sys_splice] fd_in={}, off_in={:?}, fd_out={}, off_out={:?}, len={}, flags={}.",
        fd_in,
        off_in,
        fd_out,
        off_out,
        len,
        flags
    );
    if len == 0 {
        return Ok(0);
    }
    let task = current_task().unwrap();
    let proc = &task.process;
    let fd_table = proc.fd_table_arc();
    let memory_set = proc.memory_set_arc();
    let fd_in = fd_table.get(fd_in as usize)?;
    let fd_out = fd_table.get(fd_out as usize)?;

    // 当前 splice 仍未实现零拷贝搬运；保留基础 fd / offset 参数校验后返回 EINVAL。
    if !off_in.is_null() {
        let _ = copy_from_user_val(&memory_set, off_in)?;
    }
    if !off_out.is_null() {
        let _ = copy_from_user_val(&memory_set, off_out)?;
    }
    let _ = fd_in;
    let _ = fd_out;

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
