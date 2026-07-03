use alloc::{sync::Arc, vec, vec::Vec};

use crate::{
    arch::memory_layout::PAGE_SIZE,
    fs::{FILE_PAGE_CACHE, File, OSFile, OpenFlags, Pipe, SEEK_CUR, SEEK_SET, StMode},
    mm::{
        UserBuffer, copy_from_user, copy_from_user_val, copy_to_user_val, user_buffer_from_kernel,
    },
    syscall::options::Iovec,
    task::current_task,
    utils::{SysErrNo, SyscallRet},
};

const SPLICE_F_MOVE: u32 = 0x01;
const SPLICE_F_NONBLOCK: u32 = 0x02;
const SPLICE_F_MORE: u32 = 0x04;
const SPLICE_F_GIFT: u32 = 0x08;
const SPLICE_CHUNK_SIZE: usize = 0x10000;

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
/// 在两个文件描述符之间传输数据（至少一个必须是管道）。
/// pipe -> pipe 通过 PipeBuf 引用移动；其他路径仍通过内核缓冲区兼容搬运。
pub fn sys_splice(
    fd_in: i32,
    off_in: *mut i64,
    fd_out: i32,
    off_out: *mut i64,
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
    let valid_flags = SPLICE_F_MOVE | SPLICE_F_NONBLOCK | SPLICE_F_MORE;
    if flags & !valid_flags != 0 {
        return Err(SysErrNo::EINVAL);
    }

    if fd_in < 0 || fd_out < 0 {
        return Err(SysErrNo::EBADF);
    }

    let task = current_task().unwrap();
    let proc = &task.process;
    let fd_table = proc.fd_table_arc();
    let memory_set = proc.memory_set_arc();
    let fd_in = fd_table.get(fd_in as usize)?;
    let fd_out = fd_table.get(fd_out as usize)?;

    if fd_in.is_path_only() || fd_out.is_path_only() {
        return Err(SysErrNo::EBADF);
    }

    let in_pipe = fd_in.pipe().ok();
    let out_pipe = fd_out.pipe().ok();
    let in_is_pipe = in_pipe.is_some();
    let out_is_pipe = out_pipe.is_some();

    if !in_is_pipe && !out_is_pipe {
        return Err(SysErrNo::EINVAL);
    }
    if in_is_pipe && !off_in.is_null() {
        return Err(SysErrNo::ESPIPE);
    }
    if out_is_pipe && !off_out.is_null() {
        return Err(SysErrNo::ESPIPE);
    }
    if in_is_pipe
        && out_is_pipe
        && Arc::ptr_eq(in_pipe.as_ref().unwrap(), out_pipe.as_ref().unwrap())
    {
        return Err(SysErrNo::EINVAL);
    }
    if !out_is_pipe && fd_out.flags() & OpenFlags::O_APPEND.bits() != 0 {
        return Err(SysErrNo::EINVAL);
    }

    if let (Some(input), Some(output)) = (in_pipe.as_ref(), out_pipe.as_ref()) {
        drop(task);
        return input.splice_to_pipe(
            output,
            len.min(isize::MAX as usize),
            flags & SPLICE_F_NONBLOCK != 0,
        );
    }

    let in_file = fd_in.any();
    let out_file = fd_out.any();
    if !in_file.readable() || !out_file.writable() {
        return Err(SysErrNo::EBADF);
    }
    validate_splice_file_types(&in_file, in_is_pipe, &out_file, out_is_pipe)?;
    let cached_file_to_pipe = if !in_is_pipe && out_is_pipe {
        fd_in
            .file()
            .ok()
            .map(|input| (input, out_pipe.as_ref().unwrap().clone()))
    } else {
        None
    };

    let mut in_offset = read_splice_offset(&memory_set, off_in)?;
    let mut out_offset = read_splice_offset(&memory_set, off_out)?;
    drop(task);

    if let Some((input, output)) = cached_file_to_pipe {
        let written = splice_file_to_pipe_cached(
            &input,
            &output,
            &mut in_offset,
            len.min(isize::MAX as usize),
            flags & SPLICE_F_NONBLOCK != 0,
        )?;
        write_splice_offset(&memory_set, off_in, in_offset)?;
        return Ok(written);
    }

    let mut total = 0usize;
    let mut remaining = len.min(isize::MAX as usize);

    while remaining > 0 {
        let mut chunk_len = remaining.min(SPLICE_CHUNK_SIZE);

        if let Some(pipe) = in_pipe.as_ref() {
            let readable = pipe.available_read();
            if readable == 0 {
                if pipe.all_write_ends_closed() {
                    break;
                }
                if flags & SPLICE_F_NONBLOCK != 0 {
                    if total > 0 {
                        break;
                    }
                    return Err(SysErrNo::EAGAIN);
                }
            } else {
                chunk_len = chunk_len.min(readable);
            }
        }

        if let Some(pipe) = out_pipe.as_ref() {
            let writable = pipe.available_write();
            if writable == 0 {
                if pipe.all_read_ends_closed() {
                    if total > 0 {
                        break;
                    }
                    return Err(SysErrNo::EPIPE);
                }
                if flags & SPLICE_F_NONBLOCK != 0 {
                    if total > 0 {
                        break;
                    }
                    return Err(SysErrNo::EAGAIN);
                }
                // Avoid consuming a large input chunk before a blocking pipe write can proceed.
                chunk_len = chunk_len.min(1);
            } else {
                chunk_len = chunk_len.min(writable);
            }
        }

        if chunk_len == 0 {
            break;
        }

        let mut buf = vec![0u8; chunk_len];
        let read_len = match splice_read(&in_file, in_offset, &mut buf) {
            Ok(read_len) => read_len,
            Err(err) => {
                if total > 0 {
                    break;
                }
                return Err(err);
            }
        };
        if read_len == 0 {
            break;
        }

        let written = match splice_write(&out_file, out_offset, &mut buf[..read_len]) {
            Ok(written) => written.min(read_len),
            Err(err) => {
                rewind_splice_input(&in_file, in_is_pipe, in_offset, read_len, 0);
                if total > 0 {
                    break;
                }
                return Err(err);
            }
        };
        if written == 0 {
            rewind_splice_input(&in_file, in_is_pipe, in_offset, read_len, 0);
            break;
        }

        if written < read_len {
            rewind_splice_input(&in_file, in_is_pipe, in_offset, read_len, written);
        }

        if let Some(offset) = in_offset.as_mut() {
            *offset = offset.checked_add(written as i64).ok_or(SysErrNo::EINVAL)?;
        }
        if let Some(offset) = out_offset.as_mut() {
            *offset = offset.checked_add(written as i64).ok_or(SysErrNo::EINVAL)?;
        }

        total += written;
        remaining -= written;

        if read_len < chunk_len || written < read_len {
            break;
        }
    }

    write_splice_offset(&memory_set, off_in, in_offset)?;
    write_splice_offset(&memory_set, off_out, out_offset)?;
    Ok(total)
}

fn validate_splice_file_types(
    input: &Arc<dyn File>,
    in_is_pipe: bool,
    output: &Arc<dyn File>,
    out_is_pipe: bool,
) -> Result<(), SysErrNo> {
    if !in_is_pipe && !splice_input_supported(input) {
        return Err(SysErrNo::EINVAL);
    }
    if !out_is_pipe && !splice_output_supported(output) {
        return Err(SysErrNo::EINVAL);
    }
    Ok(())
}

fn splice_input_supported(file: &Arc<dyn File>) -> bool {
    let mode = stat_file_type(file.fstat().st_mode);
    mode == StMode::FREG.bits() || mode == StMode::FCHR.bits()
}

fn splice_output_supported(file: &Arc<dyn File>) -> bool {
    stat_file_type(file.fstat().st_mode) == StMode::FREG.bits()
}

fn stat_file_type(mode: u32) -> u32 {
    mode & 0o170000
}

fn splice_file_to_pipe_cached(
    input: &Arc<OSFile>,
    output: &Arc<Pipe>,
    offset: &mut Option<i64>,
    len: usize,
    nonblock: bool,
) -> SyscallRet {
    let mut file_offset = match offset {
        Some(offset) => *offset as usize,
        None => input.offset(),
    };
    let file_size = input.inode.size();
    if file_offset >= file_size {
        return Ok(0);
    }

    let mut total = 0usize;
    let mut remaining = len.min(file_size - file_offset);
    while remaining > 0 {
        let page_index = file_offset / PAGE_SIZE;
        let page_offset = file_offset % PAGE_SIZE;
        let page = match FILE_PAGE_CACHE.get_or_load(input.inode.clone(), page_index) {
            Ok(page) => page,
            Err(err) => {
                if total > 0 {
                    break;
                }
                return Err(err);
            }
        };
        let valid_len = page.valid_len.saturating_sub(page_offset);
        if valid_len == 0 {
            break;
        }
        let chunk_len = remaining.min(valid_len).min(PAGE_SIZE - page_offset);
        let written = match output.push_file_page(page, page_offset, chunk_len, nonblock) {
            Ok(written) => written,
            Err(err) => {
                if total > 0 {
                    break;
                }
                return Err(err);
            }
        };
        if written == 0 {
            break;
        }

        file_offset += written;
        total += written;
        remaining -= written;
        if written < chunk_len {
            break;
        }
    }

    if total > 0 {
        if let Some(offset) = offset.as_mut() {
            *offset = offset.checked_add(total as i64).ok_or(SysErrNo::EINVAL)?;
        } else {
            input.set_offset(file_offset);
        }
    }
    Ok(total)
}

fn read_splice_offset(
    memory_set: &crate::mm::MemorySet,
    ptr: *mut i64,
) -> Result<Option<i64>, SysErrNo> {
    if ptr.is_null() {
        return Ok(None);
    }
    let offset = copy_from_user_val(memory_set, ptr as *const i64)?;
    if offset < 0 || offset > isize::MAX as i64 {
        return Err(SysErrNo::EINVAL);
    }
    Ok(Some(offset))
}

fn write_splice_offset(
    memory_set: &crate::mm::MemorySet,
    ptr: *mut i64,
    offset: Option<i64>,
) -> Result<(), SysErrNo> {
    if let Some(offset) = offset {
        copy_to_user_val(memory_set, ptr, &offset)?;
    }
    Ok(())
}

fn splice_read(file: &Arc<dyn File>, offset: Option<i64>, buf: &mut [u8]) -> SyscallRet {
    if let Some(offset) = offset {
        let old_offset = file.lseek(0, SEEK_CUR)?;
        file.lseek(offset as isize, SEEK_SET)?;
        let ret = file.read(unsafe { user_buffer_from_kernel(buf) });
        let restore = file.lseek(old_offset as isize, SEEK_SET);
        let ret = ret?;
        restore?;
        Ok(ret)
    } else {
        file.read(unsafe { user_buffer_from_kernel(buf) })
    }
}

fn splice_write(file: &Arc<dyn File>, offset: Option<i64>, buf: &mut [u8]) -> SyscallRet {
    if let Some(offset) = offset {
        let old_offset = file.lseek(0, SEEK_CUR)?;
        file.lseek(offset as isize, SEEK_SET)?;
        let ret = file.write(unsafe { user_buffer_from_kernel(buf) });
        let restore = file.lseek(old_offset as isize, SEEK_SET);
        let ret = ret?;
        restore?;
        Ok(ret)
    } else {
        file.write(unsafe { user_buffer_from_kernel(buf) })
    }
}

fn rewind_splice_input(
    file: &Arc<dyn File>,
    is_pipe: bool,
    offset: Option<i64>,
    read_len: usize,
    written: usize,
) {
    if is_pipe || offset.is_some() || written >= read_len {
        return;
    }
    let unread = read_len - written;
    if unread <= isize::MAX as usize {
        let _ = file.lseek(-(unread as isize), SEEK_CUR);
    }
}

/// 参考 https://man7.org/linux/man-pages/man2/tee.2.html
///
/// 在两个管道之间复制数据而不消耗数据。
/// 通过 PipeBuf 引用复制实现，不复制底层数据内容。
pub fn sys_tee(fd_in: i32, fd_out: i32, len: usize, flags: u32) -> SyscallRet {
    log::debug!(
        "[sys_tee] fd_in={}, fd_out={}, len={}, flags={}.",
        fd_in,
        fd_out,
        len,
        flags
    );
    if len == 0 {
        return Ok(0);
    }
    let valid_flags = SPLICE_F_MOVE | SPLICE_F_NONBLOCK | SPLICE_F_MORE | SPLICE_F_GIFT;
    if flags & !valid_flags != 0 {
        return Err(SysErrNo::EINVAL);
    }
    if fd_in < 0 || fd_out < 0 {
        return Err(SysErrNo::EBADF);
    }

    let task = current_task().unwrap();
    let proc = &task.process;
    let fd_table = proc.fd_table_arc();
    let fd_in = fd_table.get(fd_in as usize)?;
    let fd_out = fd_table.get(fd_out as usize)?;

    if fd_in.is_path_only() || fd_out.is_path_only() {
        return Err(SysErrNo::EBADF);
    }

    let input = fd_in.pipe().map_err(|_| SysErrNo::EINVAL)?;
    let output = fd_out.pipe().map_err(|_| SysErrNo::EINVAL)?;
    drop(task);

    input.tee_to_pipe(
        &output,
        len.min(isize::MAX as usize),
        flags & SPLICE_F_NONBLOCK != 0,
    )
}
