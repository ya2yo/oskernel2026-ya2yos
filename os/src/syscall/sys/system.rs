use alloc::vec;
use core::sync::atomic::{AtomicBool, Ordering};

use log::{debug, warn};

use crate::{
    arch::cpu::hart_id,
    fs::open_device_file,
    mm::{copy_from_user, copy_to_user, if_bad_address, user_buffer_from_kernel, UserBuffer},
    syscall::Utsname,
    task::{current_task, tid_to_task, Sysinfo},
    timer::get_time_ms,
    utils::{SysErrNo, SyscallRet},
};

// ---------------------------------------------------------------------------
// 动态 domainname (被 uname / setdomainname 共享)
// ---------------------------------------------------------------------------

/// 内核级 NIS domainname 存储
static DOMAIN_NAME: spin::Mutex<[u8; 65]> = spin::Mutex::new([0; 65]);
static DOMAIN_NAME_INIT: AtomicBool = AtomicBool::new(false);

#[cfg(target_arch = "riscv64")]
const MACHINE_NAME: &str = "riscv64";

#[cfg(target_arch = "loongarch64")]
const MACHINE_NAME: &str = "loongarch64";

/// 获取动态 domainname 的字节数组 (未设置则返回默认值 "Ya2yOS")
fn get_domainname_bytes() -> [u8; 65] {
    if DOMAIN_NAME_INIT.load(Ordering::Relaxed) {
        *DOMAIN_NAME.lock()
    } else {
        let mut b = [0; 65];
        let default = b"Ya2yOS";
        let copy_len = default.len().min(65);
        b[..copy_len].copy_from_slice(&default[..copy_len]);
        b
    }
}

/// 参考 https://man7.org/linux/man-pages/man2/uname.2.html
pub fn sys_uname(buf: *mut u8) -> SyscallRet {
    fn str2u8(s: &str) -> [u8; 65] {
        let mut b = [0; 65];
        b[0..s.len()].copy_from_slice(s.as_bytes());
        b
    }
    let uname = Utsname {
        sysname: str2u8("Linux"),
        nodename: str2u8("Ya2yOS"),
        release: str2u8("5.0.0"),
        version: str2u8("5.0.0"),
        machine: str2u8(MACHINE_NAME),
        domainname: get_domainname_bytes(),
    };
    let task = current_task().unwrap();
    let proc_inner = &task.process;
    let memory_set = proc_inner.memory_set_arc();
    copy_to_user(&memory_set, buf as usize, unsafe {
        core::slice::from_raw_parts(
            &uname as *const Utsname as *const u8,
            core::mem::size_of::<Utsname>(),
        )
    })?;
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/sysinfo.2.html
pub fn sys_sysinfo(info: *const u8) -> SyscallRet {
    let task = current_task().unwrap();
    let proc_inner = &task.process;
    let memory_set = proc_inner.memory_set_arc();
    let sysinfo = Sysinfo::new(get_time_ms() / 1000, 1 << 56, tid_to_task::task_num());
    copy_to_user(&memory_set, info as usize, unsafe {
        core::slice::from_raw_parts(
            &sysinfo as *const Sysinfo as *const u8,
            core::mem::size_of::<Sysinfo>(),
        )
    })?;
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
    let proc_inner = &task.process;
    let memory_set = proc_inner.memory_set_arc();

    if (flags as i32) < 0 {
        return Err(SysErrNo::EINVAL);
    }

    if (buf_ptr as isize) < 0 || if_bad_address(buf_ptr as usize) {
        return Err(SysErrNo::EFAULT);
    }

    if buf_ptr.is_null() {
        return Err(SysErrNo::EINVAL);
    }

    {
        let mut kernel_buf = vec![0u8; buflen];
        let ub = unsafe { user_buffer_from_kernel(&mut kernel_buf) };
        let read_len = open_device_file("/dev/random")?.read(ub)?;
        copy_to_user(&memory_set, buf_ptr as usize, &kernel_buf[..read_len])?;
        Ok(read_len)
    }
}

// getcpu(168)
/// 参考 https://man7.org/linux/man-pages/man2/getcpu.2.html
///
/// 返回当前线程所在的 CPU 编号和 NUMA 节点编号。
pub fn sys_getcpu(cpu: *mut u32, node: *mut u32, _tcache: *mut u8) -> SyscallRet {
    let task = current_task().unwrap();
    let proc_inner = &task.process;
    let memory_set = proc_inner.memory_set_arc();

    // tcache must be NULL when called from userspace (used only by vDSO)
    // We ignore it for simplicity

    let cpu_val = hart_id() as u32;
    let node_val: u32 = 0;

    if !cpu.is_null() {
        if (cpu as isize) <= 0 || if_bad_address(cpu as usize) {
            return Err(SysErrNo::EFAULT);
        }
        copy_to_user(&memory_set, cpu as usize, unsafe {
            core::slice::from_raw_parts(
                &cpu_val as *const u32 as *const u8,
                core::mem::size_of::<u32>(),
            )
        })?;
    }
    if !node.is_null() {
        if (node as isize) <= 0 || if_bad_address(node as usize) {
            return Err(SysErrNo::EFAULT);
        }
        copy_to_user(&memory_set, node as usize, unsafe {
            core::slice::from_raw_parts(
                &node_val as *const u32 as *const u8,
                core::mem::size_of::<u32>(),
            )
        })?;
    }

    debug!("[getcpu] cpu={}, node={}", cpu_val, node_val);
    Ok(0)
}

// setdomainname(162)
/// 参考 https://man7.org/linux/man-pages/man2/setdomainname.2.html
///
/// 设置系统的 NIS domain name。
/// 仅 root 可以调用；name 最长为 64 字节。
pub fn sys_setdomainname(name: *const u8, len: usize) -> SyscallRet {
    let task = current_task().unwrap();

    // EPERM: 非 root 不可设置
    if task.inner_lock().user_id != 0 {
        return Err(SysErrNo::EPERM);
    }

    // EINVAL: 长度超出限制
    if len > 64 {
        return Err(SysErrNo::EINVAL);
    }

    let proc_inner = &task.process;
    let memory_set = proc_inner.memory_set_arc();

    if len > 0 {
        if name.is_null() || (name as isize) <= 0 || if_bad_address(name as usize) {
            return Err(SysErrNo::EFAULT);
        }

        let mut dn = DOMAIN_NAME.lock();
        dn.fill(0);
        copy_from_user(&memory_set, name as usize, &mut dn[..len])?;
        DOMAIN_NAME_INIT.store(true, Ordering::Relaxed);
    } else {
        // len == 0: 清除 domainname
        let mut dn = DOMAIN_NAME.lock();
        *dn = [0; 65];
        DOMAIN_NAME_INIT.store(false, Ordering::Relaxed);
    }

    debug!("[setdomainname] len={}", len);
    Ok(0)
}

// init_module(105) / delete_module(106)
/// 参考 https://man7.org/linux/man-pages/man2/init_module.2.html
///
/// 加载内核模块。当前内核不支持模块加载。
pub fn sys_init_module(
    _module_image: *const u8,
    _len: usize,
    _param_values: *const u8,
) -> SyscallRet {
    warn!("[init_module] kernel module loading not supported");
    Err(SysErrNo::EPERM)
}

/// 参考 https://man7.org/linux/man-pages/man2/delete_module.2.html
///
/// 卸载内核模块。当前内核不支持模块卸载。
pub fn sys_delete_module(_name: *const u8, _flags: u32) -> SyscallRet {
    warn!("[delete_module] kernel module unloading not supported");
    Err(SysErrNo::EPERM)
}

// vhangup(58)
/// 参考 https://man7.org/linux/man-pages/man2/vhangup.2.html
///
/// 模拟在当前终端上挂起（hangup）。调用成功时返回 0。
/// 调用需要 CAP_SYS_TTY_CONFIG 特权（root）。
/// 由于当前内核没有完整的 VT 支持，特权检查通过后直接返回成功。
pub fn sys_vhangup() -> SyscallRet {
    let task = current_task().unwrap();
    // 仅 root 可以调用 vhangup()
    if task.inner_lock().effective_uid != 0 {
        return Err(SysErrNo::EPERM);
    }
    Ok(0)
}
