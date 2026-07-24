use alloc::{vec, vec::Vec};
use log::{debug, warn};

use crate::{
    fs::{File, Inode, OSFile, OpenFlags, StMode, SEEK_CUR, SEEK_SET},
    mm::{copy_from_user, copy_to_user, probe_user_write, user_buffer_from_kernel, UserBuffer},
    signal::{send_signal_to_thread_group, SigSet, SIGXFSZ},
    syscall::options::Iovec,
    task::current_task,
    timer::get_time_ms,
    utils::{SysErrNo, SyscallRet},
};

/// 单次 write() 最多分配的内核缓冲区大小 (64KB)。
/// 超过此大小的写操作将被切分为多次 write，避免内核堆 OOM。
const IO_CHUNK_SIZE: usize = 0x10000; // 64KB
const IOV_MAX: usize = 1024;
const RWF_SUPPORTED_FLAGS: u32 = 0;

fn split_offset_to_i64(pos_l: usize, pos_h: usize) -> i64 {
    let raw = ((pos_h as u64) << 32) | (pos_l as u32 as u64);
    raw as i64
}

fn validate_preadv2_offset(pos_l: usize, pos_h: usize) -> Result<Option<isize>, SysErrNo> {
    let offset = split_offset_to_i64(pos_l, pos_h);
    if offset == -1 {
        Ok(None)
    } else if offset < 0 || offset > isize::MAX as i64 {
        Err(SysErrNo::EINVAL)
    } else {
        Ok(Some(offset as isize))
    }
}

fn check_rw_flags(flags: u32) -> Result<(), SysErrNo> {
    if flags & !RWF_SUPPORTED_FLAGS != 0 {
        Err(SysErrNo::EOPNOTSUPP)
    } else {
        Ok(())
    }
}

fn fd_allows_write(flags: u32) -> bool {
    flags & OpenFlags::O_ACCMODE.bits() != OpenFlags::O_RDONLY.bits()
}

fn fd_allows_read(flags: u32) -> bool {
    flags & OpenFlags::O_ACCMODE.bits() != OpenFlags::O_WRONLY.bits()
}

fn stat_file_type(mode: u32) -> u32 {
    mode & StMode::FILE_TYPE_MASK.bits()
}

fn ranges_overlap(a_start: usize, a_len: usize, b_start: usize, b_len: usize) -> bool {
    if a_len == 0 || b_len == 0 {
        return false;
    }
    let a_end = a_start.saturating_add(a_len);
    let b_end = b_start.saturating_add(b_len);
    a_start < b_end && b_start < a_end
}

fn same_file_by_stat(a_dev: usize, a_ino: usize, b_dev: usize, b_ino: usize) -> bool {
    a_dev == b_dev && a_ino != 0 && a_ino == b_ino
}

fn read_iovec(
    memory_set: &crate::mm::MemorySet,
    iov: *const u8,
    index: usize,
) -> Result<Iovec, SysErrNo> {
    let iovec_size = core::mem::size_of::<Iovec>();
    let iov_ptr = (iov as usize) + iovec_size * index;
    let mut iov_buf = [0u8; core::mem::size_of::<Iovec>()];
    copy_from_user(memory_set, iov_ptr, &mut iov_buf)?;
    Ok(unsafe { core::mem::transmute(iov_buf) })
}

fn validate_iovcnt(iovcnt: usize) -> Result<(), SysErrNo> {
    if iovcnt > IOV_MAX {
        Err(SysErrNo::EINVAL)
    } else {
        Ok(())
    }
}

fn validate_iov_len(len: usize) -> Result<(), SysErrNo> {
    if len > isize::MAX as usize {
        Err(SysErrNo::EINVAL)
    } else {
        Ok(())
    }
}

fn read_iovecs(
    memory_set: &crate::mm::MemorySet,
    iov: *const u8,
    iovcnt: usize,
) -> Result<Vec<Iovec>, SysErrNo> {
    let mut total = 0usize;
    let mut iovecs = Vec::with_capacity(iovcnt);

    for i in 0..iovcnt {
        let iovinfo = read_iovec(memory_set, iov, i)?;
        validate_iov_len(iovinfo.iov_len)?;
        total = total.checked_add(iovinfo.iov_len).ok_or(SysErrNo::EINVAL)?;
        if total > isize::MAX as usize {
            return Err(SysErrNo::EINVAL);
        }
        iovecs.push(iovinfo);
    }

    Ok(iovecs)
}

/// 参考 https://man7.org/linux/man-pages/man2/write.2.html
pub fn sys_write(fd: usize, buf: *const u8, len: usize) -> SyscallRet {
    if len == 0 {
        return Ok(0);
    }

    // ---- 阶段 0: 校验 fd、取出文件引用、检查可写 ----
    let f = {
        let task = current_task().unwrap();
        let proc = &task.process;

        if fd >= proc.fd_table.len() {
            warn!("write EBADF: fd out of range");
            return Err(SysErrNo::EBADF);
        }
        let file_desc = match proc.fd_table.try_get(fd) {
            Some(f) => f,
            None => {
                warn!("write EBADF: fd not exist");
                return Err(SysErrNo::EBADF);
            }
        };
        // O_PATH 只获得路径句柄，不打开文件数据流，不能用于 write(2)。
        if file_desc.is_path_only() {
            return Err(SysErrNo::EBADF);
        }
        let f = file_desc.any();
        if !f.writable() {
            return Err(SysErrNo::EBADF);
        }
        f
    }; // 锁在此处释放

    // ---- 阶段 0.5: 检查 RLIMIT_FSIZE ---- //
    {
        let task = current_task().unwrap();
        let rlimit_fsize = task.process.get_rlimit_fsize();
        if rlimit_fsize.rlim_cur != usize::MAX {
            let pos = f.lseek(0, SEEK_CUR)?;
            if pos >= rlimit_fsize.rlim_cur {
                let _ = send_signal_to_thread_group(task.process.pid, SigSet::SIGXFSZ);
                return Err(SysErrNo::EFBIG);
            }
        }
    }

    // ---- 阶段 1: 分片写 ----
    let mut total_written: usize = 0;
    let mut user_ptr = buf as usize;

    while total_written < len {
        let chunk_len = IO_CHUNK_SIZE.min(len - total_written);
        let mut kernel_buf = vec![0u8; chunk_len];

        // 持锁：从用户空间拷贝当前分片到内核缓冲区
        {
            let task = current_task().unwrap();
            let proc = &task.process;
            let memory_set = proc.memory_set_arc();
            if let Err(err) = copy_from_user(&*memory_set, user_ptr, &mut kernel_buf) {
                return if total_written > 0 {
                    Ok(total_written)
                } else {
                    Err(err)
                };
            }
        }

        let ub = unsafe { user_buffer_from_kernel(&mut kernel_buf) };
        let written = match f.write(ub) {
            Ok(written) => written.min(chunk_len),
            Err(err) => {
                return if total_written > 0 {
                    Ok(total_written)
                } else {
                    Err(err)
                };
            }
        };
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

    let (file, is_regular_file) = {
        let task = current_task().unwrap();
        let proc = &task.process;

        let file_desc = proc.fd_table.get(fd)?;
        // O_PATH fd 只能做 fd 级别操作，read(2) 必须按 Linux 语义返回 EBADF。
        if file_desc.is_path_only() {
            return Err(SysErrNo::EBADF);
        }
        let file = file_desc.any();
        if !file.readable() {
            return Err(SysErrNo::EBADF);
        }
        let is_regular_file = if let Ok(os_file) = file_desc.file() {
            if os_file.inode.types().is_dir() {
                return Err(SysErrNo::EISDIR);
            }
            true
        } else {
            false
        };

        (file, is_regular_file)
    }; // 锁在此处释放

    #[cfg(feature = "perf")]
    let _read_active_guard = is_regular_file.then(crate::utils::perf::ReadActiveGuard::new);

    let mut total_read = 0usize;
    let mut user_ptr = buf as usize;

    loop {
        // 内核缓冲区上界：避免因 len 过大导致内核堆 OOM。
        let chunk_len = IO_CHUNK_SIZE.min(len - total_read);
        let mut kernel_buf = vec![0u8; chunk_len];
        let buffer = unsafe { user_buffer_from_kernel(&mut kernel_buf) };
        let ret = match file.read(buffer) {
            Ok(ret) => ret.min(chunk_len),
            Err(err) => {
                return if total_read > 0 {
                    Ok(total_read)
                } else {
                    Err(err)
                };
            }
        };

        if ret > 0 {
            let task = current_task().unwrap();
            let proc = &task.process;
            let mem = proc.memory_set_arc();
            if let Err(err) = copy_to_user(&*mem, user_ptr, &kernel_buf[..ret]) {
                return if total_read > 0 {
                    Ok(total_read)
                } else {
                    Err(err)
                };
            }
        }

        total_read += ret;
        user_ptr += ret;

        if ret == 0 || total_read == len || ret < chunk_len || !is_regular_file {
            break;
        }
    }

    Ok(total_read)
}

/// 参考 https://man7.org/linux/man-pages/man2/writev.2.html
pub fn sys_writev(fd: usize, iov: *const u8, iovcnt: usize) -> SyscallRet {
    let task = current_task().unwrap();
    let proc = &task.process;

    debug!(
        "[sys_writev] fd is {}, iov is {:x}, iovcnt is {}",
        fd, iov as usize, iovcnt
    );

    if fd >= proc.fd_table.len() {
        return Err(SysErrNo::EBADF);
    }
    let file = match proc.fd_table.try_get(fd) {
        Some(f) => f.any(),
        None => return Err(SysErrNo::EBADF),
    };
    if !file.writable() {
        return Err(SysErrNo::EBADF);
    }

    // ---- 检查 RLIMIT_FSIZE ---- //
    {
        let rlimit_fsize = task.process.get_rlimit_fsize();
        if rlimit_fsize.rlim_cur != usize::MAX {
            let pos = file.lseek(0, SEEK_CUR)?;
            if pos >= rlimit_fsize.rlim_cur {
                let _ = send_signal_to_thread_group(task.process.pid, SigSet::SIGXFSZ);
                return Err(SysErrNo::EFBIG);
            }
        }
    }

    // Linux treats an empty iovec array as a successful no-op.  The fd is
    // still checked above, so an invalid descriptor remains EBADF.
    if iovcnt == 0 {
        return Ok(0);
    }
    if iovcnt > IOV_MAX {
        return Err(SysErrNo::EINVAL);
    }

    let iovec_size = core::mem::size_of::<Iovec>();
    let mut kernel_bufs: Vec<Vec<u8>> = Vec::new();
    let mut bufs: Vec<UserBuffer> = Vec::new();
    {
        let memory_set = proc.memory_set_arc();
        for i in 0..iovcnt {
            let current = (iov as usize) + iovec_size * i;
            let mut iov_buf = [0u8; core::mem::size_of::<Iovec>()];
            copy_from_user(&memory_set, current, &mut iov_buf)?;
            let iovinfo: Iovec = unsafe { core::mem::transmute(iov_buf) };
            validate_iov_len(iovinfo.iov_len)?;
            if iovinfo.iov_len == 0 {
                continue;
            }
            // 单个 iovec 的缓冲区上界：防止内核堆 OOM
            let copy_len = IO_CHUNK_SIZE.min(iovinfo.iov_len);
            let mut kb = vec![0u8; copy_len];
            copy_from_user(&memory_set, iovinfo.iov_base as usize, &mut kb)?;
            let ub = unsafe { user_buffer_from_kernel(&mut kb) };
            kernel_bufs.push(kb);
            bufs.push(ub);
        }
    }
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
    let task = current_task().unwrap();
    let proc = &task.process;

    if fd >= proc.fd_table.len() {
        return Err(SysErrNo::EBADF);
    }
    let file = match proc.fd_table.try_get(fd) {
        Some(f) => f.any(),
        None => return Err(SysErrNo::EBADF),
    };
    if !file.readable() {
        return Err(SysErrNo::EBADF);
    }

    // A zero-length iovec array is a successful no-op after fd validation.
    if iovcnt == 0 {
        return Ok(0);
    }
    validate_iovcnt(iovcnt)?;
    if stat_file_type(file.fstat().st_mode) == StMode::FDIR.bits() {
        return Err(SysErrNo::EISDIR);
    }

    let iovecs = {
        let memory_set = proc.memory_set_arc();
        read_iovecs(&memory_set, iov, iovcnt)?
    };

    let mut total = 0usize;
    for iovinfo in iovecs {
        let mut copied = 0usize;
        while copied < iovinfo.iov_len {
            let chunk_len = IO_CHUNK_SIZE.min(iovinfo.iov_len - copied);
            let iov_base = iovinfo.iov_base + copied;

            let memory_set = proc.memory_set_arc();
            if let Err(err) = probe_user_write(&memory_set, iov_base, chunk_len) {
                return if total > 0 { Ok(total) } else { Err(err) };
            }
            drop(memory_set);

            let mut kernel_buf = vec![0u8; chunk_len];
            let read_ret = {
                let buffer = unsafe { user_buffer_from_kernel(&mut kernel_buf) };
                match file.read(buffer) {
                    Ok(read_ret) => read_ret,
                    Err(err) => return if total > 0 { Ok(total) } else { Err(err) },
                }
            };

            if read_ret == 0 {
                return Ok(total);
            }

            let memory_set = proc.memory_set_arc();
            if let Err(err) = copy_to_user(&memory_set, iov_base, &kernel_buf[..read_ret]) {
                return if total > 0 { Ok(total) } else { Err(err) };
            }

            total += read_ret;
            if read_ret < chunk_len {
                return Ok(total);
            }
            copied += read_ret;
        }
    }
    Ok(total)
}

/// 参考 https://man7.org/linux/man-pages/man2/lseek.2.html
pub fn sys_lseek(fd: usize, offset: isize, whence: usize) -> SyscallRet {
    let task = current_task().unwrap();
    let inner = &task.process;

    // debug!(
    //     "[sys_lseek] fd is {}, offset is {}, whence is {}",
    //     fd, offset, whence
    // );

    let file = inner.fd_table.get(fd)?.any();
    file.lseek(offset, whence)
}

/// 参考 https://man7.org/linux/man-pages/man2/sendfile.2.html
pub fn sys_sendfile(outfd: usize, infd: usize, offset_ptr: usize, count: usize) -> SyscallRet {
    let task = current_task().unwrap();
    let inner = &task.process;

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
        let memory_set = inner.memory_set_arc();
        let mut off: isize = 0;
        copy_from_user(&memory_set, offset_ptr, unsafe {
            core::slice::from_raw_parts_mut(
                &mut off as *mut isize as *mut u8,
                core::mem::size_of::<isize>(),
            )
        })?;
        off
    } else {
        0
    };

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
    let inner = &task.process;

    let file_desc = inner.fd_table.get(fd)?;
    let file = file_desc.any();
    if offset < 0 {
        return Err(SysErrNo::EINVAL);
    }

    let cur_offset = file.lseek(0, SEEK_CUR)? as isize;
    if !fd_allows_write(file_desc.flags()) || !file.writable() {
        return Err(SysErrNo::EBADF);
    }
    file.lseek(offset, SEEK_SET)?;

    let chunk_count = IO_CHUNK_SIZE.min(count);
    let mut kernel_buf = {
        let memory_set = inner.memory_set_arc();
        let mut kb = vec![0u8; chunk_count];
        if let Err(err) = copy_from_user(&memory_set, buf as usize, &mut kb) {
            let _ = file.lseek(cur_offset, SEEK_SET);
            return Err(err);
        }
        kb
    };
    let buffer = unsafe { user_buffer_from_kernel(&mut kernel_buf) };
    // release current task TCB manually to avoid multi-borrow
    drop(task);

    let ret = file.write(buffer);
    let restore = file.lseek(cur_offset, SEEK_SET);
    let ret = ret?;
    restore?;
    Ok(ret)
}

/// 参考 https://man7.org/linux/man-pages/man2/pread64.2.html
pub fn sys_pread64(fd: usize, buf: *const u8, count: usize, offset: isize) -> SyscallRet {
    let task = current_task().unwrap();
    let proc = &task.process;
    let memory_set = &*&proc.memory_set_arc();

    let file = proc.fd_table.get(fd)?.any();
    if offset < 0 {
        return Err(SysErrNo::EINVAL);
    }
    if !file.readable() {
        return Err(SysErrNo::EBADF);
    }

    // release current task TCB manually to avoid multi-borrow
    let cur_offset = file.lseek(0, SEEK_CUR)? as isize;
    if stat_file_type(file.fstat().st_mode) == StMode::FDIR.bits() {
        return Err(SysErrNo::EISDIR);
    }
    file.lseek(offset, SEEK_SET)?;
    let chunk_count = IO_CHUNK_SIZE.min(count);
    let mut kernel_buf = vec![0u8; chunk_count];
    let ub = unsafe { user_buffer_from_kernel(&mut kernel_buf) };
    let ret = file.read(ub);
    let restore = file.lseek(cur_offset, SEEK_SET);
    let ret = ret?;
    restore?;
    copy_to_user(memory_set, buf as usize, &kernel_buf[..ret])?;
    Ok(ret)
}

/// https://www.man7.org/linux/man-pages/man2/preadv2.2.html
pub fn sys_pwritev2(
    fd: usize,
    iov: *const u8,
    iovcnt: usize,
    pos_l: usize,
    pos_h: usize,
    flags: u32,
) -> SyscallRet {
    validate_iovcnt(iovcnt)?;
    if iovcnt == 0 {
        return Ok(0);
    }
    check_rw_flags(flags)?;
    let offset = validate_preadv2_offset(pos_l, pos_h)?;

    let task = current_task().unwrap();
    let proc = &task.process;

    let file_desc = proc.fd_table.get(fd)?;
    let file = file_desc.any();
    let cur_offset = file.lseek(0, SEEK_CUR)? as isize;
    if !fd_allows_write(file_desc.flags()) || !file.writable() {
        return Err(SysErrNo::EBADF);
    }
    if let Some(offset) = offset {
        file.lseek(offset, SEEK_SET)?;
    }

    let iovecs = {
        let memory_set = proc.memory_set_arc();
        match read_iovecs(&memory_set, iov, iovcnt) {
            Ok(iovecs) => iovecs,
            Err(err) => {
                if offset.is_some() {
                    let _ = file.lseek(cur_offset, SEEK_SET);
                }
                return Err(err);
            }
        }
    };

    drop(task);

    let mut total = 0usize;
    for iovinfo in iovecs {
        let mut copied = 0usize;
        while copied < iovinfo.iov_len {
            let chunk_len = IO_CHUNK_SIZE.min(iovinfo.iov_len - copied);
            let mut kernel_buf = vec![0u8; chunk_len];

            {
                let task = current_task().unwrap();
                let proc = &task.process;
                let memory_set = proc.memory_set_arc();
                let src = iovinfo.iov_base + copied;
                if let Err(err) = copy_from_user(&memory_set, src, &mut kernel_buf) {
                    if offset.is_some() {
                        let _ = file.lseek(cur_offset, SEEK_SET);
                    }
                    return if total > 0 { Ok(total) } else { Err(err) };
                }
            }

            let written = {
                let buffer = unsafe { user_buffer_from_kernel(&mut kernel_buf) };
                match file.write(buffer) {
                    Ok(written) => written,
                    Err(err) => {
                        if offset.is_some() {
                            let _ = file.lseek(cur_offset, SEEK_SET);
                        }
                        return if total > 0 { Ok(total) } else { Err(err) };
                    }
                }
            };

            total += written;
            if written < chunk_len {
                if offset.is_some() {
                    file.lseek(cur_offset, SEEK_SET)?;
                }
                return Ok(total);
            }
            copied += written;
        }
    }

    if offset.is_some() {
        file.lseek(cur_offset, SEEK_SET)?;
    }
    Ok(total)
}

/// https://www.man7.org/linux/man-pages/man2/preadv2.2.html
pub fn sys_preadv2(
    fd: usize,
    iov: *const u8,
    iovcnt: usize,
    pos_l: usize,
    pos_h: usize,
    flags: u32,
) -> SyscallRet {
    validate_iovcnt(iovcnt)?;
    if iovcnt == 0 {
        return Ok(0);
    }
    check_rw_flags(flags)?;
    let offset = validate_preadv2_offset(pos_l, pos_h)?;

    let task = current_task().unwrap();
    let proc = &task.process;

    let file = proc.fd_table.get(fd)?.any();
    let cur_offset = file.lseek(0, SEEK_CUR)? as isize;
    if !file.readable() {
        return Err(SysErrNo::EBADF);
    }
    if stat_file_type(file.fstat().st_mode) == StMode::FDIR.bits() {
        return Err(SysErrNo::EISDIR);
    }
    if let Some(offset) = offset {
        file.lseek(offset, SEEK_SET)?;
    }

    let iovecs = {
        let memory_set = proc.memory_set_arc();
        match read_iovecs(&memory_set, iov, iovcnt) {
            Ok(iovecs) => iovecs,
            Err(err) => {
                if offset.is_some() {
                    let _ = file.lseek(cur_offset, SEEK_SET);
                }
                return Err(err);
            }
        }
    };

    drop(task);

    let mut total = 0usize;
    for iovinfo in iovecs {
        let mut copied = 0usize;
        while copied < iovinfo.iov_len {
            let chunk_len = IO_CHUNK_SIZE.min(iovinfo.iov_len - copied);
            let iov_base = iovinfo.iov_base + copied;

            {
                let task = current_task().unwrap();
                let proc = &task.process;
                let memory_set = proc.memory_set_arc();
                if let Err(err) = probe_user_write(&memory_set, iov_base, chunk_len) {
                    if offset.is_some() {
                        let _ = file.lseek(cur_offset, SEEK_SET);
                    }
                    return if total > 0 { Ok(total) } else { Err(err) };
                }
            }

            let mut kernel_buf = vec![0u8; chunk_len];
            let read_ret = {
                let buffer = unsafe { user_buffer_from_kernel(&mut kernel_buf) };
                match file.read(buffer) {
                    Ok(read_ret) => read_ret,
                    Err(err) => {
                        if offset.is_some() {
                            let _ = file.lseek(cur_offset, SEEK_SET);
                        }
                        return if total > 0 { Ok(total) } else { Err(err) };
                    }
                }
            };

            if read_ret == 0 {
                if offset.is_some() {
                    file.lseek(cur_offset, SEEK_SET)?;
                }
                return Ok(total);
            }

            {
                let task = current_task().unwrap();
                let proc = &task.process;
                let memory_set = proc.memory_set_arc();
                if let Err(err) = copy_to_user(&memory_set, iov_base, &kernel_buf[..read_ret]) {
                    if offset.is_some() {
                        let _ = file.lseek(cur_offset, SEEK_SET);
                    }
                    return if total > 0 { Ok(total) } else { Err(err) };
                }
            }

            total += read_ret;
            if read_ret < chunk_len {
                if offset.is_some() {
                    file.lseek(cur_offset, SEEK_SET)?;
                }
                return Ok(total);
            }
            copied += read_ret;
        }
    }

    if offset.is_some() {
        file.lseek(cur_offset, SEEK_SET)?;
    }
    Ok(total)
}

/// 参考 https://man7.org/linux/man-pages/man2/fsync.2.html
pub fn sys_fsync(fd: usize) -> SyscallRet {
    let task = current_task().unwrap();
    let inner = &task.process;

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
    let inner = &task.process;

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
    flags: u32,
) -> SyscallRet {
    let task = current_task().unwrap();
    let inner = &task.process;

    // Linux 当前不接受非零 flags；LTP copy_file_range02 会专门检查 EINVAL。
    if flags != 0 {
        return Err(SysErrNo::EINVAL);
    }
    // 返回值类型是 ssize_t，超过 SSIZE_MAX 的 count 应在复制前直接拒绝。
    if count > isize::MAX as usize {
        return Err(SysErrNo::EOVERFLOW);
    }
    if count == 0 {
        return Ok(0);
    }

    let in_desc = inner.fd_table.get(infd)?;
    let out_desc = inner.fd_table.get(outfd)?;
    let in_any = in_desc.any();
    let out_any = out_desc.any();
    let in_stat = in_any.fstat();
    let out_stat = out_any.fstat();

    // 先用通用 File::fstat() 判断类型，保证目录输出返回 EISDIR，
    // block/char/fifo/pipe 等特殊文件返回 EINVAL。
    let out_type = stat_file_type(out_stat.st_mode);
    if out_type == StMode::FDIR.bits() {
        return Err(SysErrNo::EISDIR);
    }
    if out_type != StMode::FREG.bits() {
        return Err(SysErrNo::EINVAL);
    }

    let in_type = stat_file_type(in_stat.st_mode);
    if in_type != StMode::FREG.bits() {
        return Err(SysErrNo::EINVAL);
    }

    // copy_file_range 既要求 fd access mode 允许读/写，也要求底层对象可读/可写。
    // O_APPEND 输出 fd 在 Linux 上按 EBADF 处理，不能退化成普通追加写。
    if !fd_allows_read(in_desc.flags()) || !in_any.readable() {
        return Err(SysErrNo::EBADF);
    }
    if !fd_allows_write(out_desc.flags())
        || out_desc.flags() & OpenFlags::O_APPEND.bits() != 0
        || !out_any.writable()
    {
        return Err(SysErrNo::EBADF);
    }

    let infile = in_desc.file()?;
    let outfile = out_desc.file()?;
    if OSFile::is_immutable_path(&outfile.inode.path()) {
        return Err(SysErrNo::EPERM);
    }

    // 在释放锁之前读取用户空间的 offset
    let (in_offset, out_offset) = {
        let memory_set = inner.memory_set_arc();
        let in_off = if off_in != 0 {
            let mut off: isize = 0;
            copy_from_user(&memory_set, off_in, unsafe {
                core::slice::from_raw_parts_mut(
                    &mut off as *mut isize as *mut u8,
                    core::mem::size_of::<isize>(),
                )
            })?;
            off
        } else {
            0
        };
        let out_off = if off_out != 0 {
            let mut off: isize = 0;
            copy_from_user(&memory_set, off_out, unsafe {
                core::slice::from_raw_parts_mut(
                    &mut off as *mut isize as *mut u8,
                    core::mem::size_of::<isize>(),
                )
            })?;
            off
        } else {
            0
        };
        (in_off, out_off)
    };

    if in_offset < 0 || out_offset < 0 {
        return Err(SysErrNo::EINVAL);
    }

    // off_in/off_out 为 NULL 时使用并推进文件当前 offset；非 NULL 时使用用户传入 offset，
    // 成功后只回写用户 offset，不改变文件当前 offset。
    let in_start = if off_in == 0 {
        infile.lseek(0, SEEK_CUR)?
    } else {
        in_offset as usize
    };
    let out_start = if off_out == 0 {
        outfile.lseek(0, SEEK_CUR)?
    } else {
        out_offset as usize
    };
    if out_start.checked_add(count).is_none() || out_start + count > isize::MAX as usize {
        return Err(SysErrNo::EFBIG);
    }

    // 同一文件内复制时，源区间和目标区间不能重叠；Linux 对该场景返回 EINVAL。
    if same_file_by_stat(
        in_stat.st_dev,
        in_stat.st_ino,
        out_stat.st_dev,
        out_stat.st_ino,
    ) && ranges_overlap(in_start, count, out_start, count)
    {
        return Err(SysErrNo::EINVAL);
    }

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
        let cur_out_offset = outfile.lseek(0, SEEK_CUR)?;
        outfile.lseek(out_offset, SEEK_SET)?;
        writecount = outfile.write(outbuffer)?;
        outfile.lseek(cur_out_offset as isize, SEEK_SET)?;
    }
    let copied_at = (get_time_ms() / 1000) as u64;
    let old_mtime = out_stat.st_mtime as u64;
    let timestamp = copied_at.max(old_mtime.saturating_add(1));
    outfile
        .inode
        .set_timestamps(None, Some(timestamp), Some(timestamp))?;
    //如果系统调用执行成功，*off_in和*off_out将会增加复制的长度
    if off_in != 0 {
        let task = current_task().unwrap();
        let inner = &task.process;
        let memory_set = inner.memory_set_arc();
        let mut cur_off: isize = 0;
        copy_from_user(&memory_set, off_in, unsafe {
            core::slice::from_raw_parts_mut(
                &mut cur_off as *mut isize as *mut u8,
                core::mem::size_of::<isize>(),
            )
        })?;
        cur_off += writecount as isize;
        copy_to_user(&memory_set, off_in, unsafe {
            core::slice::from_raw_parts(
                &cur_off as *const isize as *const u8,
                core::mem::size_of::<isize>(),
            )
        })?;
    }
    if off_out != 0 {
        let task = current_task().unwrap();
        let inner = &task.process;
        let memory_set = inner.memory_set_arc();
        let mut cur_off: isize = 0;
        copy_from_user(&memory_set, off_out, unsafe {
            core::slice::from_raw_parts_mut(
                &mut cur_off as *mut isize as *mut u8,
                core::mem::size_of::<isize>(),
            )
        })?;
        cur_off += writecount as isize;
        copy_to_user(&memory_set, off_out, unsafe {
            core::slice::from_raw_parts(
                &cur_off as *const isize as *const u8,
                core::mem::size_of::<isize>(),
            )
        })?;
    }

    Ok(writecount)
}
