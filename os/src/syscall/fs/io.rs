use alloc::{sync::Arc, vec, vec::Vec};
use log::{debug, warn};

use crate::{
    fs::{DummyFd, FdTable, File, FileDescriptor, OpenFlags, SEEK_CUR, SEEK_SET},
    mm::{
        UserBuffer, safe_translated_byte_buffer, translated_byte_buffer, translated_ref, translated_refmut
    },
    syscall::{fs::dummyfd_create, options::Iovec},
    task::current_task,
    timer::get_time_ms,
    utils::{SysErrNo, SyscallRet},
};

/// 参考 https://man7.org/linux/man-pages/man2/write.2.html
pub fn sys_write(fd: usize, buf: *const u8, len: usize) -> SyscallRet {
    // debug!("[sys_write] fd is {}, len={}", fd, len);

    let task = current_task().unwrap();
    let fd_table = task.get_fd_table();

    if fd >= fd_table.len() {
        warn!("write EINVAL early return");
        return Err(SysErrNo::EBADF);
    }
    if let Some(f) = fd_table.try_get_file(fd) {
        let process = task.process.inner_lock();
        let memory_set = process.get_locked_memory_set_read();
        let buffer = safe_translated_byte_buffer(&*memory_set, buf, len).unwrap();
        let buffer = UserBuffer::new(buffer);
        if !f.writable() {
            return Err(SysErrNo::EBADF);
        }
        // 注意！一些文件的write可能会阻塞，还可能借用process，所以我们应该drop process
        drop(memory_set);
        drop(process);
        drop(task);
        let ret = f.write(buffer)?;
        // debug!("buffer 3");
        Ok(ret)
    } else {
        warn!("write EBADF");
        Err(SysErrNo::EBADF)
    }
}

/// 参考 https://man7.org/linux/man-pages/man2/read.2.html
pub fn sys_read(fd: usize, buf: *const u8, len: usize) -> SyscallRet {
    let task = current_task().unwrap();
    let fd_table = task.get_fd_table();
    if fd >= fd_table.len() {
        return Err(SysErrNo::EINVAL);
    }
    if let Some(file) = fd_table.try_get_file(fd) {
        let process = task.process.inner_lock();
        let memory_set = process.get_locked_memory_set_write();
        if !file.readable() {
            return Err(SysErrNo::EACCES);
        }
        // 注意！一些文件的read可能会阻塞，还可能借用task_inner，所以我们应该drop task_inner
        let buffer = safe_translated_byte_buffer(&*memory_set, buf, len).unwrap();
        let buffer = UserBuffer::new(buffer);
        drop(memory_set);
        drop(process);
        let ret = file.read(buffer)?;
        Ok(ret)
    } else {
        Err(SysErrNo::EBADF)
    }
}

/// 参考 https://man7.org/linux/man-pages/man2/writev.2.html
pub fn sys_writev(fd: usize, iov: *const u8, iovcnt: usize) -> SyscallRet {
    let task = current_task().unwrap();
    let proc_inner = task.process.inner_lock();
    let token = proc_inner.get_locked_memory_set_read().token();
    let fd_table = proc_inner.fd_table.clone();
    debug!(
        "[sys_writev] fd is {}, iov is {:x}, iovcnt is {}",
        fd, iov as usize, iovcnt
    );

    if fd >= proc_inner.fd_table.len() {
        return Err(SysErrNo::EINVAL);
    }
    if let Some(file) = fd_table.try_get(fd) {
        let file = file.any();
        if !file.writable() {
            return Err(SysErrNo::EBADF);
        }
        // release current task TCB manually to avoid multi-borrow
        drop(proc_inner);
        drop(task);
        let mut ret: usize = 0;
        let iovec_size = core::mem::size_of::<Iovec>();

        for i in 0..iovcnt {
            // current iovec pointer
            let current = unsafe { iov.add(iovec_size * i) };
            let iovinfo = *translated_refmut(token, current as *mut Iovec);
            let buf = UserBuffer::new(
                translated_byte_buffer(token, iovinfo.iov_base as *mut u8, iovinfo.iov_len)
                    .unwrap(),
            );
            let write_ret = file.write(buf)?;
            ret += write_ret as usize;
        }
        Ok(ret)
    } else {
        Err(SysErrNo::EBADF)
    }
}

/// 参考 https://man7.org/linux/man-pages/man2/readv.2.html
pub fn sys_readv(fd: usize, iov: *const u8, iovcnt: usize) -> SyscallRet {
    let task = current_task().unwrap();
    let proc_inner = task.process.inner_lock();
    let token = proc_inner.get_locked_memory_set_read().token();

    if fd >= proc_inner.fd_table.len() {
        return Err(SysErrNo::EINVAL);
    }
    if let Some(file) = proc_inner.fd_table.try_get(fd) {
        let file = file.any();
        if !file.readable() {
            return Err(SysErrNo::EACCES);
        }
        // release current task TCB manually to avoid multi-borrow
        drop(proc_inner);
        drop(task);
        let mut ret: usize = 0;
        let iovec_size = core::mem::size_of::<Iovec>();

        for i in 0..iovcnt {
            // current iovec pointer
            let current = unsafe { iov.add(iovec_size * i) };
            let iovinfo = *translated_refmut(token, current as *mut Iovec);
            let buf = UserBuffer::new(
                translated_byte_buffer(token, iovinfo.iov_base as *mut u8, iovinfo.iov_len)
                    .unwrap(),
            );
            let read_ret = file.read(buf)?;
            ret += read_ret as usize;
        }
        Ok(ret)
    } else {
        Err(SysErrNo::EBADF)
    }
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
    let token = inner.get_locked_memory_set_read().token();

    // debug!(
    //     "[sys_sendfile] outfd is {}, infd is {}, offset_ptr is {}, count is {}",
    //     outfd, infd, offset_ptr, count
    // );

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

    drop(inner);
    drop(task);

    //构造输入缓冲池
    let mut buf = vec![0u8; count];
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
        let offset = *translated_ref(token, offset_ptr as *const isize);
        if offset < 0 {
            return Err(SysErrNo::EINVAL);
        }
        // infile.set_offset(offset as usize);
        infile.lseek(offset, SEEK_SET)?;
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
    let token = inner.get_locked_memory_set_read().token();

    if offset < 0 || fd >= inner.fd_table.len() {
        return Err(SysErrNo::EINVAL);
    }
    if let Some(file) = inner.fd_table.try_get(fd) {
        let file = file.file()?;
        if !file.writable() {
            return Err(SysErrNo::EACCES);
        }
        let file = file.clone();
        // release current task TCB manually to avoid multi-borrow
        drop(inner);
        drop(task);
        let cur_offset = file.lseek(0, SEEK_CUR)? as isize;
        file.lseek(offset, SEEK_SET)?;
        let ret = file.write(UserBuffer::new(
            translated_byte_buffer(token, buf, count).unwrap(),
        ))?;
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
        let ret = file.read(UserBuffer::new(
            safe_translated_byte_buffer(memory_set, buf, count).unwrap(),
        ))?;
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
    let token = inner.get_locked_memory_set_read().token();

    // debug!("[sys_copy_file_range] infd is {}, off_in is {}, outfd is {}, off_out is {},count is {}, flags is {}", infd, off_in, outfd, off_out, count, flags);

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

    drop(inner);
    drop(task);

    //构造输入缓冲池
    let mut buf = vec![0u8; count];
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
        let offset = *translated_ref(token, off_in as *const isize);
        if offset < 0 {
            return Err(SysErrNo::EINVAL);
        }
        let in_offset = infile.lseek(0, SEEK_CUR)?;
        infile.lseek(offset, SEEK_SET)?;
        readcount = infile.read(inbuffer)?;
        infile.lseek(in_offset as isize, SEEK_SET)?;
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
        let offset = *translated_ref(token, off_out as *const isize);
        if offset < 0 {
            return Err(SysErrNo::EINVAL);
        }
        let out_offset = outfile.lseek(0, SEEK_CUR)?;
        outfile.lseek(offset, SEEK_SET)?;
        writecount = outfile.write(outbuffer)?;
        outfile.lseek(out_offset as isize, SEEK_SET)?;
    }
    outfile
        .inode
        .set_timestamps(None, Some((get_time_ms() / 1000) as u64), None);
    //如果系统调用执行成功，*off_in和*off_out将会增加复制的长度
    if off_in != 0 {
        *translated_refmut(token, off_in as *mut isize) += writecount as isize;
    }
    if off_out != 0 {
        *translated_refmut(token, off_out as *mut isize) += writecount as isize;
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

