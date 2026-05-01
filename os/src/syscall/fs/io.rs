use alloc::sync::Arc;

use crate::{fs::File, mm::{UserBuffer, safe_translated_byte_buffer}, task::current_task, utils::SysErrNo};


/// 参考 https://man7.org/linux/man-pages/man2/write.2.html
pub fn sys_write(fd: usize, buf: *const u8, len: usize) -> SyscallRet {
    debug!("[sys_write] fd is {}, len={}", fd, len);

    let task = current_task().unwrap();
    let fd_table = task.get_fd_table();

    if fd >= fd_table.len() {
        debug!("write EINVAL early return");
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
        let ret=f.write(buffer);
        debug!("buffer 3");
        Ok(ret)
    } else {
        debug!("write EBADF");
        Err(SysErrNo::EBADF)
    }
}

/// 参考 https://man7.org/linux/man-pages/man2/read.2.html
pub fn sys_read(fd: usize, buf: *const u8, len: usize) -> SyscallRet {
    let task = current_task().unwrap();
    let fd_table=task.get_fd_table();
    if fd >= fd_table.len() {
        return Err(SysErrNo::EINVAL);
    }
    if let Some(file) = fd_table.try_get_file(fd) {
        let process = task.process.inner_lock();
        let memory_set = process.get_locked_memory_set_read();
        if !file.readable() {
            return Err(SysErrNo::EACCES);
        }
        // 注意！一些文件的read可能会阻塞，还可能借用task_inner，所以我们应该drop task_inner
        let buffer = safe_translated_byte_buffer(&*memory_set, buf, len).unwrap();
        let buffer = UserBuffer::new(buffer);
        drop(inner);
        drop(memory_set);
        drop(process);
        let ret = file.read(buffer)?;
        Ok(ret)
    } else {
        Err(SysErrNo::EBADF)
    }
}