use linux_raw_sys::{general::{_LINUX_CAPABILITY_VERSION_1, _LINUX_CAPABILITY_VERSION_2, _LINUX_CAPABILITY_VERSION_3}, prctl::{PR_CAP_AMBIENT, PR_CAPBSET_DROP, PR_GET_NO_NEW_PRIVS, PR_GET_PDEATHSIG, PR_GET_SPECULATION_CTRL, PR_GET_THP_DISABLE, PR_SET_DUMPABLE, PR_SET_NAME, PR_SET_NO_NEW_PRIVS, PR_SET_PDEATHSIG, PR_SET_SECCOMP, PR_SET_SECUREBITS, PR_SET_THP_DISABLE, PR_SET_TIMING}};
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
        _LINUX_CAPABILITY_VERSION_1 | _LINUX_CAPABILITY_VERSION_2 | _LINUX_CAPABILITY_VERSION_3
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

// ---------------------------------------------------------------------------
// prctl(2) — 进程控制
// ---------------------------------------------------------------------------

/// 参考 https://man7.org/linux/man-pages/man2/prctl.2.html
pub fn sys_prctl(
    option: u32,
    arg2: usize,
    arg3: usize,
    arg4: usize,
    arg5: usize,
) -> SyscallRet {
    debug!(
        "[prctl] option={}, arg2=0x{:x}, arg3=0x{:x}, arg4=0x{:x}, arg5=0x{:x}",
        option, arg2, arg3, arg4, arg5
    );
    let task = current_task().unwrap();
    match option {
        PR_SET_PDEATHSIG => {
            // arg2: 信号编号，0 表示清除
            if arg2 > 64 {
                return Err(SysErrNo::EINVAL);
            }
            let mut inner = task.inner_lock();
            inner.pdeath_signal = arg2 as u8;
            debug!("[prctl] set pdeath_signal={}", arg2);
            Ok(0)
        }
        PR_GET_PDEATHSIG => {
            // 将当前 pdeath_signal 写入 arg2 指向的 int
            let inner = task.inner_lock();
            let sig = inner.pdeath_signal;
            drop(inner);
            if arg2 != 0 {
                let proc_inner = task.process.inner_lock();
                let token = proc_inner.get_locked_memory_set_read().token();
                put_data(token, arg2 as *mut i32, sig as i32);
            }
            debug!("[prctl] get pdeath_signal={}", sig);
            Ok(0)
        }
        PR_SET_DUMPABLE => {
            // 仅接受 SUID_DUMP_DISABLE(0) / SUID_DUMP_USER(1)
            if arg2 > 1 {
                return Err(SysErrNo::EINVAL);
            }
            Ok(0)
        }
        PR_SET_NAME => {
            // EFAULT: 非法地址
            if (arg2 as isize) <= 0 || if_bad_address(arg2) {
                return Err(SysErrNo::EFAULT);
            }
            Ok(0)
        }
        PR_SET_SECCOMP => {
            // 仅 SECCOMP_MODE_FILTER(2) 需要 EACCES
            if arg2 == 2 {
                // 没有 CAP_SYS_ADMIN → EACCES
                if arg3 > 0 && if_bad_address(arg3) {
                    return Err(SysErrNo::EFAULT);
                }
                return Err(SysErrNo::EACCES);
            }
            Ok(0)
        }
        PR_CAPBSET_DROP => {
            // 没有 CAP_SETPCAP → EPERM
            Err(SysErrNo::EPERM)
        }
        PR_SET_SECUREBITS => {
            // 没有 CAP_SETPCAP → EPERM
            Err(SysErrNo::EPERM)
        }
        PR_SET_TIMING => {
            // 仅支持 PR_TIMING_STATISTICAL(0)
            Err(SysErrNo::EINVAL)
        }
        PR_SET_NO_NEW_PRIVS => {
            // arg2 必须为 1 且 arg3/4/5 必须为 0
            if arg2 != 1 || arg3 != 0 || arg4 != 0 || arg5 != 0 {
                return Err(SysErrNo::EINVAL);
            }
            Ok(0)
        }
        PR_GET_NO_NEW_PRIVS => {
            // arg2/3/4/5 必须为 0
            if arg2 != 0 || arg3 != 0 || arg4 != 0 || arg5 != 0 {
                return Err(SysErrNo::EINVAL);
            }
            Ok(0)
        }
        PR_SET_THP_DISABLE => {
            // arg3/4/5 必须为 0
            if arg3 != 0 || arg4 != 0 || arg5 != 0 {
                return Err(SysErrNo::EINVAL);
            }
            Ok(0)
        }
        PR_GET_THP_DISABLE => {
            // arg2/3/4/5 必须为 0
            if arg2 != 0 || arg3 != 0 || arg4 != 0 || arg5 != 0 {
                return Err(SysErrNo::EINVAL);
            }
            Ok(0)
        }
        PR_CAP_AMBIENT => {
            // 简化: 所有有效调用返回 0，无效参数返回 EINVAL
            Ok(0)
        }
        PR_GET_SPECULATION_CTRL => {
            // arg3/4/5 必须为 0
            if arg3 != 0 || arg4 != 0 || arg5 != 0 {
                return Err(SysErrNo::EINVAL);
            }
            Ok(0)
        }
        _ => {
            // 未知选项
            warn!("[prctl] unsupported option {}", option);
            Err(SysErrNo::EINVAL)
        }
    }
}
