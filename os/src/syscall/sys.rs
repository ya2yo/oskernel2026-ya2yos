use crate::{fs::open_device_file, mm::{UserBuffer, if_bad_address, put_data, translated_byte_buffer}, syscall::Utsname, task::{Sysinfo, current_task, current_token, tid_to_task}, timer::get_time_ms, utils::{SysErrNo, SyscallRet}};

/// 参考 https://man7.org/linux/man-pages/man2/getuid.2.html
pub fn sys_getuid() -> SyscallRet {
    let task = current_task().unwrap();
    let task_inner = task.inner_lock();
    Ok(task_inner.user_id)
}

/// 参考 https://man7.org/linux/man-pages/man2/geteuid.2.html
pub fn sys_geteuid() -> SyscallRet {
    Ok(0) // root user
}

/// 参考 https://man7.org/linux/man-pages/man2/getgid.2.html
pub fn sys_getgid() -> SyscallRet {
    Ok(0) // root group
}

/// 参考 https://man7.org/linux/man-pages/man2/getegid.2.html
pub fn sys_getegid() -> SyscallRet {
    Ok(0) // root group
}

pub fn sys_setuid(uid: usize) -> SyscallRet {
    let task = current_task().unwrap();
    let mut task_inner = task.inner_lock();
    task_inner.user_id = uid;
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/uname.2.html
pub fn sys_uname(buf: *mut u8) -> SyscallRet {
    fn str2u8(s: &str) -> [u8; 65] {
        let mut b = [0; 65];
        b[0..s.len()].copy_from_slice(s.as_bytes());
        b
    }
    let uname = Utsname {
        sysname: str2u8("TrustOS"),
        nodename: str2u8("TrustOS"),
        release: str2u8("5.0.0"),
        version: str2u8("5.0.0"),
        machine: str2u8("RISC-V64"),
        domainname: str2u8("TrustOS"),
    };
    let token = current_token();
    put_data(token, buf as *mut Utsname, uname);
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/sysinfo.2.html
pub fn sys_sysinfo(info: *const u8) -> SyscallRet {
    let task = current_task().unwrap();
    let token = task.process.inner_lock().get_locked_memory_set_read().token();

    put_data(
        token,
        info as *mut Sysinfo,
        Sysinfo::new(get_time_ms() / 1000, 1 << 56, tid_to_task::task_num()),
    );
    // debug!("[sys_sysinfo] ourinfo is {:?}", ourinfo);
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/syslog.2.html
pub fn sys_syslog(_logtype: isize, _bufp: *const u8, _len: usize) -> SyscallRet {
    // 伪实现
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/getrandom.2.html
pub fn sys_getrandom(buf_ptr: *const u8, buflen: usize, flags: u32) -> SyscallRet {
    let task = current_task().unwrap();
    let token = task.process.inner_lock().get_locked_memory_set_read().token();

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
