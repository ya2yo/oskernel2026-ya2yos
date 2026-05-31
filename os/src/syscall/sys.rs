use log::{debug, warn};

use crate::{
    fs::open_device_file,
    mm::{get_data, if_bad_address, put_data, translated_byte_buffer, UserBuffer},
    syscall::Utsname,
    task::{current_task, current_token, tid_to_task, Sysinfo},
    timer::get_time_ms,
    utils::{SysErrNo, SyscallRet},
};

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
/// https://man7.org/linux/man-pages/man2/setresuid.2.html
pub fn sys_setresuid(_ruid: u32, _euid: u32, _suid: u32)->SyscallRet {
    warn!("[sys_setresuid] not implement!");
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
    let token = task
        .process
        .inner_lock()
        .get_locked_memory_set_read()
        .token();

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
    let token = task
        .process
        .inner_lock()
        .get_locked_memory_set_read()
        .token();

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

// ---------------------------------------------------------------------------
// capability 系统调用
// ---------------------------------------------------------------------------

/// Linux capability 版本号
const LINUX_CAPABILITY_VERSION_1: u32 = 0x19980330;
const LINUX_CAPABILITY_VERSION_2: u32 = 0x20071026;
const LINUX_CAPABILITY_VERSION_3: u32 = 0x20080522;

/// 对应 C struct __user_cap_header_struct
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct CapUserHeader {
    pub version: u32,
    pub pid: i32,
}

/// 对应 C struct __user_cap_data_struct
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct CapUserData {
    pub effective: u32,
    pub permitted: u32,
    pub inheritable: u32,
}

/// 参考 https://man7.org/linux/man-pages/man2/capget.2.html
pub fn sys_capget(hdrp: *mut CapUserHeader, datap: *mut CapUserData) -> SyscallRet {
    let task = current_task().unwrap();
    let proc_inner = task.process.inner_lock();
    let token = proc_inner.get_locked_memory_set_read().token();

    // EFAULT: 非法地址
    if (hdrp as isize) <= 0 || if_bad_address(hdrp as usize) {
        return Err(SysErrNo::EFAULT);
    }
    if (datap as isize) <= 0 || if_bad_address(datap as usize) {
        return Err(SysErrNo::EFAULT);
    }

    let mut hdr = get_data(token, hdrp);
    debug!("[capget] version=0x{:x}, pid={}", hdr.version, hdr.pid);

    // EINVAL: pid < 0 (仅 pid == 0 表示自身)
    if hdr.pid < 0 {
        debug!("[capget] negative pid -> EINVAL");
        return Err(SysErrNo::EINVAL);
    }

    // ESRCH: 进程不存在 (pid > 0 且不是自己)
    if hdr.pid > 0 && hdr.pid as usize != task.pid() {
        debug!("[capget] pid {} not found -> ESRCH", hdr.pid);
        return Err(SysErrNo::ESRCH);
    }

    // 版本检查: 如果传入了不支持的值，回填内核首选版本
    let supported = matches!(
        hdr.version,
        LINUX_CAPABILITY_VERSION_1 | LINUX_CAPABILITY_VERSION_2 | LINUX_CAPABILITY_VERSION_3
    );
    if !supported {
        hdr.version = LINUX_CAPABILITY_VERSION_3;
        put_data(token, hdrp, hdr);
        debug!("[capget] unsupported version -> fallback to V3");
        return Err(SysErrNo::EINVAL);
    }

    // 成功: 返回零填充的能力集
    let data = CapUserData {
        effective: 0,
        permitted: 0,
        inheritable: 0,
    };
    put_data(token, datap, data);
    debug!("[capget] success, version=0x{:x}", hdr.version);
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/capset.2.html
pub fn sys_capset(hdrp: *mut CapUserHeader, datap: *const CapUserData) -> SyscallRet {
    // 伪实现: 允许设置但不实际存储
    let task = current_task().unwrap();
    let proc_inner = task.process.inner_lock();
    let token = proc_inner.get_locked_memory_set_read().token();

    if (hdrp as isize) <= 0 || if_bad_address(hdrp as usize) {
        return Err(SysErrNo::EFAULT);
    }
    if (datap as isize) <= 0 || if_bad_address(datap as usize) {
        return Err(SysErrNo::EFAULT);
    }

    let hdr = get_data(token, hdrp);
    debug!("[capset] version=0x{:x}, pid={}", hdr.version, hdr.pid);

    // EPERM: 非 root 不能设置 capabilities
    if task.inner_lock().user_id != 0 {
        return Err(SysErrNo::EPERM);
    }

    Ok(0)
}
