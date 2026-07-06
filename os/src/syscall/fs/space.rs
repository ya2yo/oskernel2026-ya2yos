use alloc::sync::Arc;

use crate::{
    fs::{File, OpenFlags, open, superblock_fs_stat}, mm::read_user_cstr, task::current_task, utils::{SysErrNo, SyscallRet}
};

const FALLOC_FL_KEEP_SIZE: u32 = 0x01;
const FALLOC_SUPPORTED_FLAGS: u32 = FALLOC_FL_KEEP_SIZE;

/// https://www.man7.org/linux/man-pages/man2/truncate.2.html
pub fn sys_truncate(path: usize, length: usize) -> SyscallRet {
    let task = current_task().unwrap();
    let proc = Arc::clone(&task.process);
    let memory_set = proc.memory_set_arc();
    let path = read_user_cstr(&memory_set, path as *const u8)?;
    let f = open(&path, OpenFlags::O_WRONLY, 0)?;
    f.file()?.inode.truncate(length)
}

/// https://www.man7.org/linux/man-pages/man2/ftruncate.2.html
pub fn sys_ftruncate(fd: usize, length: i32) -> SyscallRet {
    let task = current_task().unwrap();
    let inner = &task.process;

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

/// https://www.man7.org/linux/man-pages/man2/fallocate.2.html
pub fn sys_fallocate(fd: usize, mode: u32, offset: usize, len: usize) -> SyscallRet {
    let offset = offset as isize;
    let len = len as isize;

    if offset < 0 || len <= 0 {
        return Err(SysErrNo::EINVAL);
    }
    if mode & !FALLOC_SUPPORTED_FLAGS != 0 {
        return Err(SysErrNo::EOPNOTSUPP);
    }

    let end = (offset as usize)
        .checked_add(len as usize)
        .ok_or(SysErrNo::EFBIG)?;
    if end > isize::MAX as usize {
        return Err(SysErrNo::EFBIG);
    }

    let file = {
        let task = current_task().unwrap();
        let inner = &task.process;
        if fd >= inner.fd_table.len() {
            return Err(SysErrNo::EBADF);
        }
        let file = inner.fd_table.try_get(fd).ok_or(SysErrNo::EBADF)?.file()?;
        if !file.writable() {
            return Err(SysErrNo::EBADF);
        }
        file
    };

    if !file.inode.types().is_file() {
        return Err(SysErrNo::ENODEV);
    }

    let stat = superblock_fs_stat();
    let block_size = stat.f_bsize.max(1) as usize;
    let current_size = file.inode.size();
    let reserve_len = if mode & FALLOC_FL_KEEP_SIZE != 0 {
        len as usize
    } else {
        end.saturating_sub(current_size)
    };
    let needed_blocks = reserve_len.saturating_add(block_size - 1) / block_size;
    if needed_blocks > stat.f_bavail.max(0) as usize {
        return Err(SysErrNo::ENOSPC);
    }

    if mode & FALLOC_FL_KEEP_SIZE == 0 && end > current_size {
        file.inode.truncate(end)?;
    }

    Ok(0)
}
