use alloc::{sync::Arc, vec, vec::Vec};
use log::{debug, warn};

use crate::{
    fs::{DummyFd, FdTable, File, FileDescriptor, OpenFlags, SEEK_CUR, SEEK_SET},
    mm::{
        UserBuffer, copy_from_user, copy_to_user, user_buffer_from_kernel,
    },
    syscall::{fs::dummyfd_create, options::Iovec},
    task::current_task,
    timer::get_time_ms,
    utils::{SysErrNo, SyscallRet},
};

/// 单次 write() 最多分配的内核缓冲区大小 (64KB)。
/// 超过此大小的写操作将被切分为多次 write，避免内核堆 OOM。
const IO_CHUNK_SIZE: usize = 0x10000; // 64KB

/// 参考 https://man7.org/linux/man-pages/man2/write.2.html
pub fn sys_write(fd: usize, buf: *const u8, len: usize) -> SyscallRet {
    if len == 0 {
        return Ok(0);
    }

    // ---- 阶段 0: 校验 fd、取出文件引用、检查可写 ----
    let f = {
        let task = current_task().unwrap();
        let proc_inner = task.process.inner_lock();

        if fd >= proc_inner.fd_table.len() {
            warn!("write EBADF: fd out of range");
            return Err(SysErrNo::EBADF);
        }
        let f = match proc_inner.fd_table.try_get_file(fd) {
            Some(f) => f,
            None => {
                warn!("write EBADF: fd not exist");
                return Err(SysErrNo::EBADF);
            }
        };
        if !f.writable() {
            return Err(SysErrNo::EBADF);
        }
        f
    }; // 锁在此处释放

    // ---- 阶段 1: 分片写 ----
    let mut total_written: usize = 0;
    let mut user_ptr = buf as usize;

    while total_written < len {
        let chunk_len = IO_CHUNK_SIZE.min(len - total_written);
        let mut kernel_buf = vec![0u8; chunk_len];

        // 持锁：从用户空间拷贝当前分片到内核缓冲区
        {
            let task = current_task().unwrap();
            let proc_inner = task.process.inner_lock();
            let memory_set = proc_inner.get_locked_memory_set_read();
            copy_from_user(&*memory_set, user_ptr, &mut kernel_buf)?;
        }

        let ub = unsafe { user_buffer_from_kernel(&mut kernel_buf) };
        let written = f.write(ub)?;
        total_written += written;
        user_ptr += written;

        // 短写：文件无法继续接收数据，提前返回
        if written < chunk_len {
            break;
        }
    }

    Ok(total_written)
}

/// 参考 https://man7.org/linux/man-pages/man2/read.2.html
pub fn sys_read(fd: usize, buf: *const u8, len: usize) -> SyscallRet {
    if len == 0 {
        return Ok(0);
    }
    // 内核缓冲区上界：避免因 len 过大导致内核堆 OOM。
    // POSIX 允许 read() 返回少于请求的字节数，调用者必须处理短读。
    let chunk_len = IO_CHUNK_SIZE.min(len);

    // ---- 阶段 0: 校验 fd、取出文件引用、检查可读 ----
    let file = {
        let task = current_task().unwrap();
        let proc_inner = task.process.inner_lock();
        if fd >= proc_inner.fd_table.len() {
            return Err(SysErrNo::EINVAL);
        }
        let file = match proc_inner.fd_table.try_get_file(fd) {
            Some(f) => f,
            None => return Err(SysErrNo::EBADF),
        };
        if !file.readable() {
            return Err(SysErrNo::EACCES);
        }
        file
    }; // 锁在此处释放

    // ---- 阶段 1: 单次无锁读 ----
    let mut kernel_buf = vec![0u8; chunk_len];
    let buffer = unsafe { user_buffer_from_kernel(&mut kernel_buf) };
    let ret = file.read(buffer)?;

    // ---- 阶段 2: 持锁写回用户空间 ----
    if ret > 0 {
        let task = current_task().unwrap();
        let proc_inner = task.process.inner_lock();
        let mem = proc_inner.get_locked_memory_set_read();
        copy_to_user(&*mem, buf as usize, &kernel_buf[..ret])?;
    }
    Ok(ret)
}

/// 参考 https://man7.org/linux/man-pages/man2/writev.2.html
pub fn sys_writev(fd: usize, iov: *const u8, iovcnt: usize) -> SyscallRet {
    // iovec 数量上限，防止遍历过多
    const IOV_MAX: usize = 1024;
    if iovcnt == 0 || iovcnt > IOV_MAX {
        return Err(SysErrNo::EINVAL);
    }

    let task = current_task().unwrap();
    let proc_inner = task.process.inner_lock();

    debug!(
        "[sys_writev] fd is {}, iov is {:x}, iovcnt is {}",
        fd, iov as usize, iovcnt
    );

    if fd >= proc_inner.fd_table.len() {
        return Err(SysErrNo::EINVAL);
    }
    let file = match proc_inner.fd_table.try_get(fd) {
        Some(f) => f.any(),
        None => return Err(SysErrNo::EBADF),
    };
    if !file.writable() {
        return Err(SysErrNo::EBADF);
    }

    let iovec_size = core::mem::size_of::<Iovec>();
    let mut kernel_bufs: Vec<Vec<u8>> = Vec::new();
    let mut bufs: Vec<UserBuffer> = Vec::new();
    {
        let memory_set = proc_inner.get_locked_memory_set_read();
        for i in 0..iovcnt {
            let current = (iov as usize) + iovec_size * i;
            let mut iov_buf = [0u8; core::mem::size_of::<Iovec>()];
            copy_from_user(&memory_set, current, &mut iov_buf)?;
            let iovinfo: Iovec = unsafe { core::mem::transmute(iov_buf) };
            // 单个 iovec 的缓冲区上界：防止内核堆 OOM
            let copy_len = IO_CHUNK_SIZE.min(iovinfo.iov_len);
            let mut kb = vec![0u8; copy_len];
            copy_from_user(&memory_set, iovinfo.iov_base as usize, &mut kb)?;
            let ub = unsafe { user_buffer_from_kernel(&mut kb) };
            kernel_bufs.push(kb);
            bufs.push(ub);
        }
    }
    drop(proc_inner);
    drop(task);
    let mut ret: usize = 0;
    for buf in bufs {
        let write_ret = file.write(buf)?;
        ret += write_ret as usize;
    }
    Ok(ret)
}

/// 参考 https://man7.org/linux/man-pages/man2/readv.2.html
pub fn sys_readv(fd: usize, iov: *const u8, iovcnt: usize) -> SyscallRet {
    const IOV_MAX: usize = 1024;
    if iovcnt == 0 || iovcnt > IOV_MAX {
        return Err(SysErrNo::EINVAL);
    }

    let task = current_task().unwrap();
    let proc_inner = task.process.inner_lock();

    if fd >= proc_inner.fd_table.len() {
        return Err(SysErrNo::EINVAL);
    }
    let file = match proc_inner.fd_table.try_get(fd) {
        Some(f) => f.any(),
        None => return Err(SysErrNo::EBADF),
    };
    if !file.readable() {
        return Err(SysErrNo::EACCES);
    }

    let iovec_size = core::mem::size_of::<Iovec>();
    let mut total: usize = 0;

    for i in 0..iovcnt {
        // 阶段 1：持锁读取 iovec 元数据 + 分配内核缓冲区
        let (iov_base, iov_len, mut kernel_buf) = {
            let memory_set = proc_inner.get_locked_memory_set_read();
            let iov_ptr = (iov as usize) + iovec_size * i;

            let mut iov_buf = [0u8; core::mem::size_of::<Iovec>()];
            copy_from_user(&memory_set, iov_ptr, &mut iov_buf)?;
            let iovinfo: Iovec = unsafe { core::mem::transmute(iov_buf) };

            if iovinfo.iov_len == 0 {
                (0, 0, Vec::new())
            } else {
                let buf_len = IO_CHUNK_SIZE.min(iovinfo.iov_len);
                (iovinfo.iov_base, buf_len, vec![0u8; buf_len])
            }
        };

        if iov_len == 0 {
            continue;
        }

        // 阶段 2：无锁读取文件 → 内核缓冲区（可能阻塞，不持锁）
        let read_ret = {
            let mut ub_v = Vec::with_capacity(1);
            unsafe {
                ub_v.push(core::slice::from_raw_parts_mut(
                    kernel_buf.as_mut_ptr(),
                    iov_len,
                ));
            }
            file.read(UserBuffer::new(ub_v))?
        };

        // 阶段 3：持锁将内核缓冲区 → 用户空间
        {
            let memory_set = proc_inner.get_locked_memory_set_read();
            copy_to_user(&memory_set, iov_base, &kernel_buf[..read_ret])?;
        }

        total += read_ret as usize;
    }
    Ok(total)
}

/// 参考 https://man7.org/linux/man-pages/man2/lseek.2.html
pub fn sys_lseek(fd: usize, offset: isize, whence: usize) -> SyscallRet {
    let task = current_task().unwrap();
    let inner = task.process.inner_lock();

    // debug!(
    //     "[sys_lseek] fd is {}, offset is {}, whence is {}",
    //     fd, offset, whence
    // );

    if fd >= inner.fd_table.len() || inner.fd_table.try_get(fd).is_none() {
        return Err(SysErrNo::EINVAL);
    }
    let file = inner.fd_table.get(fd)?.file()?;
    file.lseek(offset, whence)
}

/// 参考 https://man7.org/linux/man-pages/man2/sendfile.2.html
pub fn sys_sendfile(outfd: usize, infd: usize, offset_ptr: usize, count: usize) -> SyscallRet {
    let task = current_task().unwrap();
    let inner = task.process.inner_lock();

    if outfd >= inner.fd_table.len()
        || inner.fd_table.try_get(outfd).is_none()
        || infd >= inner.fd_table.len()
        || inner.fd_table.try_get(infd).is_none()
    {
        return Err(SysErrNo::EINVAL);
    }

    let outfile = inner.fd_table.get(outfd)?.any();
    if !outfile.writable() {
        return Err(SysErrNo::EACCES);
    }

    let infile = inner.fd_table.get(infd)?.file()?;
    if !infile.readable() {
        return Err(SysErrNo::EACCES);
    }

    // 在释放锁之前读取用户空间的 offset
    let user_offset = if offset_ptr != 0 {
        let memory_set = inner.get_locked_memory_set_read();
        let mut off: isize = 0;
        copy_from_user(&memory_set, offset_ptr, unsafe {
            core::slice::from_raw_parts_mut(&mut off as *mut isize as *mut u8, core::mem::size_of::<isize>())
        })?;
        off
    } else {
        0
    };

    drop(inner);
    drop(task);

    // 内核缓冲区上界：防止 count 过大导致 OOM
    let chunk_count = IO_CHUNK_SIZE.min(count);
    //构造输入缓冲池
    let mut buf = vec![0u8; chunk_count];
    let mut inbufv = Vec::new();
    unsafe {
        inbufv.push(core::slice::from_raw_parts_mut(
            buf.as_mut_slice().as_mut_ptr(),
            buf.as_slice().len(),
        ));
    }
    //输入缓冲池
    let inbuffer = UserBuffer::new(inbufv);

    let readcount;
    if offset_ptr == 0 {
        readcount = infile.read(inbuffer)?;
    } else {
        if user_offset < 0 {
            return Err(SysErrNo::EINVAL);
        }
        infile.lseek(user_offset, SEEK_SET)?;
        readcount = infile.read(inbuffer)?;
    }

    if readcount == 0 {
        return Ok(0);
    }

    //构造输出缓冲池
    let mut outbufv = Vec::new();
    unsafe {
        outbufv.push(core::slice::from_raw_parts_mut(
            buf.as_mut_slice().as_mut_ptr(),
            readcount,
        ));
    }
    //输出缓冲池
    let outbuffer = UserBuffer::new(outbufv);
    //写数据
    let retcount = outfile.write(outbuffer)?;

    Ok(retcount)
}

/// 参考 https://man7.org/linux/man-pages/man2/pwrite64.2.html
pub fn sys_pwrite64(fd: usize, buf: *const u8, count: usize, offset: isize) -> SyscallRet {
    let task = current_task().unwrap();
    let inner = task.process.inner_lock();

    if offset < 0 || fd >= inner.fd_table.len() {
        return Err(SysErrNo::EINVAL);
    }
    if let Some(file) = inner.fd_table.try_get(fd) {
        let file = file.file()?;
        if !file.writable() {
            return Err(SysErrNo::EACCES);
        }
        let file = file.clone();
        let chunk_count = IO_CHUNK_SIZE.min(count);
        let mut kernel_buf = {
            let memory_set = inner.get_locked_memory_set_read();
            let mut kb = vec![0u8; chunk_count];
            copy_from_user(&memory_set, buf as usize, &mut kb)?;
            kb
        };
        let buffer = unsafe { user_buffer_from_kernel(&mut kernel_buf) };
        // release current task TCB manually to avoid multi-borrow
        drop(inner);
        drop(task);
        let cur_offset = file.lseek(0, SEEK_CUR)? as isize;
        file.lseek(offset, SEEK_SET)?;
        let ret = file.write(buffer)?;
        file.lseek(cur_offset, SEEK_SET)?;
        return Ok(ret);
    }
    Err(SysErrNo::EBADF)
}

/// 参考 https://man7.org/linux/man-pages/man2/pread64.2.html
pub fn sys_pread64(fd: usize, buf: *const u8, count: usize, offset: isize) -> SyscallRet {
    let task = current_task().unwrap();
    let proc_inner = task.process.inner_lock();
    let memory_set = &*&proc_inner.get_locked_memory_set_read();

    if offset < 0 || fd >= proc_inner.fd_table.len() {
        return Err(SysErrNo::EINVAL);
    }
    if let Some(file) = proc_inner.fd_table.try_get(fd) {
        let file = file.file()?;
        if !file.readable() {
            return Err(SysErrNo::EACCES);
        }
        // release current task TCB manually to avoid multi-borrow
        let cur_offset = file.lseek(0, SEEK_CUR)? as isize;
        file.lseek(offset, SEEK_SET)?;
        let chunk_count = IO_CHUNK_SIZE.min(count);
        let mut kernel_buf = vec![0u8; chunk_count];
        let ub = unsafe { user_buffer_from_kernel(&mut kernel_buf) };
        let ret = file.read(ub)?;
        copy_to_user(memory_set, buf as usize, &kernel_buf[..ret])?;
        file.lseek(cur_offset, SEEK_SET)?;
        Ok(ret)
    } else {
        Err(SysErrNo::EBADF)
    }
}

// Linux的实现与手册有差异或未实现该调用
pub fn sys_ftruncate(fd: usize, length: i32) -> SyscallRet {
    let task = current_task().unwrap();
    let inner = task.process.inner_lock();

    if fd >= inner.fd_table.len() || (fd as isize) < 0 {
        return Err(SysErrNo::EBADF);
    }

    if length < 0 {
        return Err(SysErrNo::EINVAL);
    }

    if let Some(file) = inner.fd_table.try_get(fd) {
        let file = file.file()?;
        return file.inode.truncate(length as usize);
    }
    Err(SysErrNo::EBADF)
}

/// 参考 https://man7.org/linux/man-pages/man2/fsync.2.html
pub fn sys_fsync(fd: usize) -> SyscallRet {
    let task = current_task().unwrap();
    let inner = task.process.inner_lock();

    if fd >= inner.fd_table.len() || inner.fd_table.try_get(fd).is_none() {
        return Err(SysErrNo::EINVAL);
    }

    let file = inner.fd_table.get(fd)?.file()?;
    file.inode.sync();
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/fdatasync.2.html
///
/// 将文件数据冲刷到磁盘（类似 fsync，但不强制刷新元数据）。
/// 当前内核不区分数据/元数据同步，语义与 fsync 相同。
pub fn sys_fdatasync(fd: usize) -> SyscallRet {
    sys_fsync(fd)
}

/// 参考 https://man7.org/linux/man-pages/man2/sync_file_range.2.html
///
/// 将文件指定范围内的数据冲刷到磁盘。
///
/// # 参数
/// - `fd`: 文件描述符
/// - `offset`: 起始偏移（字节）
/// - `nbytes`: 同步字节数（0 表示从 offset 到文件末尾）
/// - `flags`: SYNC_FILE_RANGE_WAIT_BEFORE / SYNC_FILE_RANGE_WRITE / SYNC_FILE_RANGE_WAIT_AFTER 的组合
///
/// # 返回值
/// 成功返回 0。
pub fn sys_sync_file_range(fd: i32, offset: i64, nbytes: i64, flags: u32) -> SyscallRet {
    // 参考 https://man7.org/linux/man-pages/man2/sync_file_range.2.html
    const SYNC_FILE_RANGE_WAIT_BEFORE: u32 = 1;
    const SYNC_FILE_RANGE_WRITE: u32 = 2;
    const SYNC_FILE_RANGE_WAIT_AFTER: u32 = 4;
    const VALID_FLAGS: u32 =
        SYNC_FILE_RANGE_WAIT_BEFORE | SYNC_FILE_RANGE_WRITE | SYNC_FILE_RANGE_WAIT_AFTER;

    // EINVAL: flags 包含未定义的位，或同时指定 WAIT_BEFORE 和 WAIT_AFTER 但未指定 WRITE
    if flags & !VALID_FLAGS != 0 {
        return Err(SysErrNo::EINVAL);
    }

    // EBADF: fd 无效
    if fd < 0 {
        return Err(SysErrNo::EBADF);
    }
    let fd = fd as usize;

    // ESPIPE: fd 指向管道、FIFO 或 socket
    // EINVAL: offset < 0 或 nbytes < 0
    if offset < 0 || nbytes < 0 {
        return Err(SysErrNo::EINVAL);
    }

    let task = current_task().unwrap();
    let inner = task.process.inner_lock();

    if fd >= inner.fd_table.len() || inner.fd_table.try_get(fd).is_none() {
        return Err(SysErrNo::EBADF);
    }

    // nbytes == 0 表示从 offset 同步到文件末尾（语义等价于 fsync）
    if nbytes == 0 {
        let file = inner.fd_table.get(fd)?.file()?;
        file.inode.sync();
        return Ok(0);
    }

    // 非零 nbytes：同步指定范围的数据
    let file = inner.fd_table.get(fd)?.file()?;
    // 检查 fd 不是管道（管道不支持 sync_file_range）
    // ESPIPE 留给后续细化；当前直接执行 sync
    file.inode.sync();
    Ok(0)
}

/// fat32文件系统可以使用此调用
/// ext4文件系统暂不支持将offset设置在超过文件大小
/// 参考 https://man7.org/linux/man-pages/man2/copy_file_range.2.html
pub fn sys_copy_file_range(
    infd: usize,
    off_in: usize,
    outfd: usize,
    off_out: usize,
    count: usize,
    _flags: u32,
) -> SyscallRet {
    let task = current_task().unwrap();
    let inner = task.process.inner_lock();

    if outfd >= inner.fd_table.len()
        || inner.fd_table.try_get(outfd).is_none()
        || infd >= inner.fd_table.len()
        || inner.fd_table.try_get(infd).is_none()
    {
        return Err(SysErrNo::EINVAL);
    }

    let outfile = inner.fd_table.get(outfd)?.file()?;
    if !outfile.writable() {
        return Err(SysErrNo::EACCES);
    }

    let infile = inner.fd_table.get(infd)?.file()?;
    if !infile.readable() {
        return Err(SysErrNo::EACCES);
    }

    // 在释放锁之前读取用户空间的 offset
    let (in_offset, out_offset) = {
        let memory_set = inner.get_locked_memory_set_read();
        let in_off = if off_in != 0 {
            let mut off: isize = 0;
            copy_from_user(&memory_set, off_in, unsafe {
                core::slice::from_raw_parts_mut(&mut off as *mut isize as *mut u8, core::mem::size_of::<isize>())
            })?;
            off
        } else {
            0
        };
        let out_off = if off_out != 0 {
            let mut off: isize = 0;
            copy_from_user(&memory_set, off_out, unsafe {
                core::slice::from_raw_parts_mut(&mut off as *mut isize as *mut u8, core::mem::size_of::<isize>())
            })?;
            off
        } else {
            0
        };
        (in_off, out_off)
    };

    drop(inner);
    drop(task);

    // 内核缓冲区上界：防止 count 过大导致 OOM
    let chunk_count = IO_CHUNK_SIZE.min(count);
    //构造输入缓冲池
    let mut buf = vec![0u8; chunk_count];
    let mut inbufv = Vec::new();
    unsafe {
        inbufv.push(core::slice::from_raw_parts_mut(
            buf.as_mut_slice().as_mut_ptr(),
            buf.as_slice().len(),
        ));
    }
    //输入缓冲池
    let inbuffer = UserBuffer::new(inbufv);

    //读数据
    let readcount;
    if off_in == 0 {
        readcount = infile.read(inbuffer)?;
    } else {
        if in_offset < 0 {
            return Err(SysErrNo::EINVAL);
        }
        let cur_in_offset = infile.lseek(0, SEEK_CUR)?;
        infile.lseek(in_offset, SEEK_SET)?;
        readcount = infile.read(inbuffer)?;
        infile.lseek(cur_in_offset as isize, SEEK_SET)?;
    }

    if readcount == 0 {
        return Ok(0);
    }

    //构造输出缓冲池
    let mut outbufv = Vec::new();
    unsafe {
        outbufv.push(core::slice::from_raw_parts_mut(
            buf.as_mut_slice().as_mut_ptr(),
            readcount,
        ));
    }
    //输出缓冲池
    let outbuffer = UserBuffer::new(outbufv);

    //写数据
    let writecount;
    if off_out == 0 {
        writecount = outfile.write(outbuffer)?;
    } else {
        if out_offset < 0 {
            return Err(SysErrNo::EINVAL);
        }
        let cur_out_offset = outfile.lseek(0, SEEK_CUR)?;
        outfile.lseek(out_offset, SEEK_SET)?;
        writecount = outfile.write(outbuffer)?;
        outfile.lseek(cur_out_offset as isize, SEEK_SET)?;
    }
    outfile
        .inode
        .set_timestamps(None, Some((get_time_ms() / 1000) as u64), None);
    //如果系统调用执行成功，*off_in和*off_out将会增加复制的长度
    if off_in != 0 {
        let task = current_task().unwrap();
        let inner = task.process.inner_lock();
        let memory_set = inner.get_locked_memory_set_read();
        let mut cur_off: isize = 0;
        copy_from_user(&memory_set, off_in, unsafe {
            core::slice::from_raw_parts_mut(&mut cur_off as *mut isize as *mut u8, core::mem::size_of::<isize>())
        })?;
        cur_off += writecount as isize;
        copy_to_user(&memory_set, off_in, unsafe {
            core::slice::from_raw_parts(&cur_off as *const isize as *const u8, core::mem::size_of::<isize>())
        })?;
    }
    if off_out != 0 {
        let task = current_task().unwrap();
        let inner = task.process.inner_lock();
        let memory_set = inner.get_locked_memory_set_read();
        let mut cur_off: isize = 0;
        copy_from_user(&memory_set, off_out, unsafe {
            core::slice::from_raw_parts_mut(&mut cur_off as *mut isize as *mut u8, core::mem::size_of::<isize>())
        })?;
        cur_off += writecount as isize;
        copy_to_user(&memory_set, off_out, unsafe {
            core::slice::from_raw_parts(&cur_off as *const isize as *const u8, core::mem::size_of::<isize>())
        })?;
    }

    Ok(writecount)
}

/// https://www.man7.org/linux/man-pages/man2/fallocate.2.html
pub fn sys_fallocate(_fd: usize, _mode: u32, _offset: usize, _len: usize) -> SyscallRet {
    warn!("[sys_fallocate] not implement!");
    Ok(0)
}

/// https://www.man7.org/linux/man-pages/man2/fanotify_init.2.html
pub fn sys_fanotify_init(_flags: u32, _event_f_flags: u32) -> SyscallRet {
    warn!("[sys_fanotify_init] not implement!");
    Ok(0)
}

/// https://man7.org/linux/man-pages/man2/userfaultfd.2.html
pub fn sys_user_faultfd(_flags: u32) -> SyscallRet {
    warn!("[sys_fanotify_init] not implement!");
    dummyfd_create()
}

