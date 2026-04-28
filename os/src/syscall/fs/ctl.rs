use crate::utils::{SyscallRet, SysErrNo};
use crate::task::{current_task};
use crate::mm::{safe_translated_byte_buffer,UserBuffer};

/// 参考 https://man7.org/linux/man-pages/man2/getcwd.2.html
pub fn sys_getcwd(buf: *const u8, size: usize) -> SyscallRet {
    let task = current_task().unwrap();
    let proc_inner = task.process.inner_lock();
    let cwd = proc_inner.fs_info.get_cwd();
    let cwd_bytes = cwd.as_bytes();
    let cwd_len_with_null = cwd_bytes.len() + 1;
    if size < cwd_len_with_null {
        return Err(SysErrNo::ERANGE);
    }
    let memory_set=proc_inner.get_locked_memory_set_read();
    let buffers=match safe_translated_byte_buffer(&memory_set, buf, size) {
        Some(bufs)=>bufs,
        None=>return Err(SysErrNo::EFAULT),
    };
    let mut user_buf=UserBuffer::new(buffers);
    user_buf.write(cwd_bytes);
    let null_bytes:[u8;1]=[0];
    user_buf.write_at(cwd_bytes.len(),&null_bytes);
    Ok(buf as usize)
}