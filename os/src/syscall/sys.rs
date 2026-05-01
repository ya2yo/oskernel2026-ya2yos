/// 参考 https://man7.org/linux/man-pages/man2/getrandom.2.html
pub fn sys_getrandom(buf_ptr: *const u8, buflen: usize, flags: u32) -> SyscallRet {
    let task = current_task().unwrap();
    let token = task.process.inner_lock().get_locked_memory_set().token();

    if (flags as i32) < 0 {
        return Err(SysErrNo::EINVAL);
    }

    if (buf_ptr as isize) < 0 || if_bad_address(buf_ptr as usize) {
        return Err(SysErrNo::EFAULT);
    }

    if buf_ptr.is_null() {
        return Err(SysErrNo::EINVAL);
    }

    open_device_file("/dev/random")?.read(UserBuffer::new(
        translated_byte_buffer(token, buf_ptr, buflen).unwrap(),
    ))
}
