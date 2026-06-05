use linux_raw_sys::{general::{_LINUX_CAPABILITY_VERSION_1, _LINUX_CAPABILITY_VERSION_2, _LINUX_CAPABILITY_VERSION_3}, prctl::{PR_CAP_AMBIENT, PR_CAPBSET_DROP, PR_GET_NO_NEW_PRIVS, PR_GET_PDEATHSIG, PR_GET_SPECULATION_CTRL, PR_GET_THP_DISABLE, PR_SET_DUMPABLE, PR_SET_NAME, PR_SET_NO_NEW_PRIVS, PR_SET_PDEATHSIG, PR_SET_SECCOMP, PR_SET_SECUREBITS, PR_SET_THP_DISABLE, PR_SET_TIMING}};
use log::{debug, warn};

use crate::{
    fs::{InodeType, NONE_MODE, OpenFlags, open, open_device_file},
    mm::{UserBuffer, copy_from_user, copy_to_user, if_bad_address, read_user_cstr, safe_translated_byte_buffer},
    syscall::Utsname,
    task::{Sysinfo, current_task, tid_to_task},
    timer::get_time_ms,
    utils::{SysErrNo, SyscallRet},
};
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

// ---------------------------------------------------------------------------
// 动态 domainname (被 uname / setdomainname 共享)
// ---------------------------------------------------------------------------

/// 内核级 NIS domainname 存储
static DOMAIN_NAME: spin::Mutex<[u8; 65]> = spin::Mutex::new([0; 65]);
static DOMAIN_NAME_INIT: AtomicBool = AtomicBool::new(false);

/// 获取动态 domainname 的字节数组 (未设置则返回默认值 "TrustOS")
fn get_domainname_bytes() -> [u8; 65] {
    if DOMAIN_NAME_INIT.load(Ordering::Relaxed) {
        *DOMAIN_NAME.lock()
    } else {
        let mut b = [0; 65];
        let default = b"TrustOS";
        let copy_len = default.len().min(65);
        b[..copy_len].copy_from_slice(&default[..copy_len]);
        b
    }
}

/// 参考 https://man7.org/linux/man-pages/man2/getuid.2.html
pub fn sys_getuid() -> SyscallRet {
    let task = current_task().unwrap();
    let task_inner = task.inner_lock();
    Ok(task_inner.user_id)
}

/// 参考 https://man7.org/linux/man-pages/man2/geteuid.2.html
pub fn sys_geteuid() -> SyscallRet {
    let task = current_task().unwrap();
    let uid = task.inner_lock().effective_uid as usize;
    Ok(uid)
}

/// 参考 https://man7.org/linux/man-pages/man2/getgid.2.html
pub fn sys_getgid() -> SyscallRet {
    let task = current_task().unwrap();
    let gid = task.inner_lock().effective_gid as usize;
    Ok(gid)
}

/// 参考 https://man7.org/linux/man-pages/man2/getegid.2.html
pub fn sys_getegid() -> SyscallRet {
    let task = current_task().unwrap();
    let gid = task.inner_lock().effective_gid as usize;
    Ok(gid)
}

pub fn sys_setuid(uid: usize) -> SyscallRet {
    let task = current_task().unwrap();
    let mut task_inner = task.inner_lock();
    if uid > 65535 {
        return Err(SysErrNo::EINVAL);
    }
    let uid = uid as u32;
    task_inner.user_id = uid as usize;
    task_inner.effective_uid = uid;
    task_inner.saved_uid = uid;
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/chroot.2.html
pub fn sys_chroot(path: *const u8) -> SyscallRet {
    debug!("[chroot] path=0x{:x}", path as usize);

    let task = current_task().unwrap();

    if path.is_null() {
        return Err(SysErrNo::EINVAL);
    }
    if (path as isize) <= 0 || if_bad_address(path as usize) {
        return Err(SysErrNo::EFAULT);
    }

    if task.inner_lock().user_id != 0 {
        return Err(SysErrNo::EPERM);
    }

    let path_str = {
        let proc_inner = task.process.inner_lock();
        let memory_set = proc_inner.get_locked_memory_set_read();
        read_user_cstr(&memory_set, path)?
    };

    let file = open(&path_str, OpenFlags::O_RDONLY, NONE_MODE)?;
    let osfile = file.file()?;
    if osfile.inode.types() != InodeType::Dir {
        return Err(SysErrNo::ENOTDIR);
    }

    task.process.inner_lock().fs_info.set_cwd(path_str);
    debug!("[chroot] success");
    Ok(0)
}

/// (uid_t)-1：保持对应 UID 不变
const UID_UNCHANGED: u32 = u32::MAX;

fn uid_arg_valid(uid: u32) -> bool {
    uid == UID_UNCHANGED || uid <= 65535
}

fn compute_resuid(
    cur_r: u32,
    cur_e: u32,
    cur_s: u32,
    ruid: u32,
    euid: u32,
    suid: u32,
) -> (u32, u32, u32) {
    let mut new_r = cur_r;
    let mut new_e = cur_e;
    let mut new_s = cur_s;

    if ruid != UID_UNCHANGED {
        new_r = ruid;
        if euid == UID_UNCHANGED {
            new_e = ruid;
        }
        if suid == UID_UNCHANGED {
            new_s = ruid;
        }
    }
    if euid != UID_UNCHANGED {
        new_e = euid;
        if suid == UID_UNCHANGED {
            new_s = euid;
        }
    }
    if suid != UID_UNCHANGED {
        new_s = suid;
    }

    (new_r, new_e, new_s)
}

fn setresuid_allowed(
    cur_r: u32,
    cur_e: u32,
    cur_s: u32,
    new_r: u32,
    new_e: u32,
    new_s: u32,
    ruid: u32,
    euid: u32,
    suid: u32,
) -> bool {
    let allowed = [cur_r, cur_e, cur_s];
    if new_r != cur_r && !allowed.contains(&new_r) {
        return false;
    }
    if new_e != cur_e && !allowed.contains(&new_e) {
        return false;
    }
    if new_s != cur_s && !allowed.contains(&new_s) {
        return false;
    }

    let explicit = (ruid != UID_UNCHANGED) as u32
        + (euid != UID_UNCHANGED) as u32
        + (suid != UID_UNCHANGED) as u32;
    explicit <= 1
}

/// 参考 https://man7.org/linux/man-pages/man2/setreuid.2.html
///
/// 设置进程的真实用户 ID 和有效用户 ID。
/// -1 表示保持该值不变。
pub fn sys_setreuid(ruid: usize, euid: usize) -> SyscallRet {
    let ruid = ruid as u32;
    let euid = euid as u32;

    if !uid_arg_valid(ruid) || !uid_arg_valid(euid) {
        return Err(SysErrNo::EINVAL);
    }

    let task = current_task().unwrap();
    let mut inner = task.inner_lock();

    let old_r = inner.user_id as u32;
    let old_e = inner.effective_uid;
    let old_s = inner.saved_uid;

    // -1 保持原值
    let new_r = if ruid == UID_UNCHANGED { old_r } else { ruid };
    let new_e = if euid == UID_UNCHANGED { old_e } else { euid };

    // 两者都为 -1：无变化
    if ruid == UID_UNCHANGED && euid == UID_UNCHANGED {
        return Ok(0);
    }

    let privileged = inner.user_id == 0 || inner.effective_uid == 0;

    if !privileged {
        // 非特权：ruid 只能设为 old_ruid 或 old_euid
        if ruid != UID_UNCHANGED && ruid != old_r && ruid != old_e {
            return Err(SysErrNo::EPERM);
        }
        // 非特权：euid 只能设为 old_ruid、old_euid 或 old_suid
        if euid != UID_UNCHANGED && euid != old_r && euid != old_e && euid != old_s {
            return Err(SysErrNo::EPERM);
        }
    }

    // Linux 语义: ruid 被改变 或 euid 被设为不等于旧 ruid 的值时，
    // saved-set-user-ID 被置为新的 euid
    if (ruid != UID_UNCHANGED) || (euid != UID_UNCHANGED && new_e != old_r) {
        inner.saved_uid = new_e;
    }

    inner.user_id = new_r as usize;
    inner.effective_uid = new_e;

    Ok(0)
}

/// https://man7.org/linux/man-pages/man2/setresuid.2.html
pub fn sys_setresuid(ruid: u32, euid: u32, suid: u32) -> SyscallRet {
    if !uid_arg_valid(ruid) || !uid_arg_valid(euid) || !uid_arg_valid(suid) {
        return Err(SysErrNo::EINVAL);
    }

    let task = current_task().unwrap();
    let mut inner = task.inner_lock();

    let cur_r = inner.user_id as u32;
    let cur_e = inner.effective_uid;
    let cur_s = inner.saved_uid;
    let (new_r, new_e, new_s) = compute_resuid(cur_r, cur_e, cur_s, ruid, euid, suid);

    let privileged = inner.user_id == 0 || inner.effective_uid == 0;
    if !privileged
        && !setresuid_allowed(cur_r, cur_e, cur_s, new_r, new_e, new_s, ruid, euid, suid)
    {
        return Err(SysErrNo::EPERM);
    }

    inner.user_id = new_r as usize;
    inner.effective_uid = new_e;
    inner.saved_uid = new_s;
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/getresuid.2.html
pub fn sys_getresuid(ruid: *mut u32, euid: *mut u32, suid: *mut u32) -> SyscallRet {
    let task = current_task().unwrap();
    let proc_inner = task.process.inner_lock();
    let memory_set = proc_inner.get_locked_memory_set_read();
    let inner = task.inner_lock();
    let real_uid = inner.user_id as u32;

    for (ptr, value) in [
        (ruid, real_uid),
        (euid, inner.effective_uid),
        (suid, inner.saved_uid),
    ] {
        if ptr.is_null() {
            continue;
        }
        if (ptr as isize) <= 0 || if_bad_address(ptr as usize) {
            return Err(SysErrNo::EFAULT);
        }
        copy_to_user(&memory_set, ptr as usize, unsafe {
            core::slice::from_raw_parts(&value as *const u32 as *const u8, core::mem::size_of::<u32>())
        })?;
    }
    Ok(0)
}

/// (gid_t)-1：保持对应 GID 不变
const GID_UNCHANGED: u32 = u32::MAX;

fn gid_arg_valid(gid: u32) -> bool {
    gid == GID_UNCHANGED || gid <= 65535
}

fn compute_resgid(
    cur_r: u32,
    cur_e: u32,
    cur_s: u32,
    rgid: u32,
    egid: u32,
    sgid: u32,
) -> (u32, u32, u32) {
    let mut new_r = cur_r;
    let mut new_e = cur_e;
    let mut new_s = cur_s;

    if rgid != GID_UNCHANGED {
        new_r = rgid;
        if egid == GID_UNCHANGED {
            new_e = rgid;
        }
        if sgid == GID_UNCHANGED {
            new_s = rgid;
        }
    }
    if egid != GID_UNCHANGED {
        new_e = egid;
        if sgid == GID_UNCHANGED {
            new_s = egid;
        }
    }
    if sgid != GID_UNCHANGED {
        new_s = sgid;
    }

    (new_r, new_e, new_s)
}

fn setresgid_allowed(
    cur_r: u32,
    cur_e: u32,
    cur_s: u32,
    new_r: u32,
    new_e: u32,
    new_s: u32,
    rgid: u32,
    egid: u32,
    sgid: u32,
) -> bool {
    let allowed = [cur_r, cur_e, cur_s];
    if new_r != cur_r && !allowed.contains(&new_r) {
        return false;
    }
    if new_e != cur_e && !allowed.contains(&new_e) {
        return false;
    }
    if new_s != cur_s && !allowed.contains(&new_s) {
        return false;
    }

    let explicit = (rgid != GID_UNCHANGED) as u32
        + (egid != GID_UNCHANGED) as u32
        + (sgid != GID_UNCHANGED) as u32;
    explicit <= 1
}

/// 参考 https://man7.org/linux/man-pages/man2/setregid.2.html
///
/// 设置进程的真实组 ID 和有效组 ID。
/// -1 表示保持该值不变。
pub fn sys_setregid(rgid: usize, egid: usize) -> SyscallRet {
    let rgid = rgid as u32;
    let egid = egid as u32;

    if !gid_arg_valid(rgid) || !gid_arg_valid(egid) {
        return Err(SysErrNo::EINVAL);
    }

    let task = current_task().unwrap();
    let mut inner = task.inner_lock();

    let old_r = inner.real_gid;
    let old_e = inner.effective_gid;
    let old_s = inner.saved_gid;

    let new_r = if rgid == GID_UNCHANGED { old_r } else { rgid };
    let new_e = if egid == GID_UNCHANGED { old_e } else { egid };

    if rgid == GID_UNCHANGED && egid == GID_UNCHANGED {
        return Ok(0);
    }

    let privileged = inner.user_id == 0 || inner.effective_gid == 0;

    if !privileged {
        if rgid != GID_UNCHANGED && rgid != old_r && rgid != old_e {
            return Err(SysErrNo::EPERM);
        }
        if egid != GID_UNCHANGED && egid != old_r && egid != old_e && egid != old_s {
            return Err(SysErrNo::EPERM);
        }
    }

    // Linux 语义: rgid 被改变 或 egid 被设为不等于旧 rgid 的值时，
    // saved-set-group-ID 被置为新的 egid
    if (rgid != GID_UNCHANGED) || (egid != GID_UNCHANGED && new_e != old_r) {
        inner.saved_gid = new_e;
    }

    inner.real_gid = new_r;
    inner.effective_gid = new_e;

    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/setresgid.2.html
pub fn sys_setresgid(rgid: u32, egid: u32, sgid: u32) -> SyscallRet {
    if !gid_arg_valid(rgid) || !gid_arg_valid(egid) || !gid_arg_valid(sgid) {
        return Err(SysErrNo::EINVAL);
    }

    let task = current_task().unwrap();
    let mut inner = task.inner_lock();

    let cur_r = inner.real_gid;
    let cur_e = inner.effective_gid;
    let cur_s = inner.saved_gid;
    let (new_r, new_e, new_s) = compute_resgid(cur_r, cur_e, cur_s, rgid, egid, sgid);

    let privileged = inner.user_id == 0 || inner.effective_gid == 0;
    if !privileged
        && !setresgid_allowed(cur_r, cur_e, cur_s, new_r, new_e, new_s, rgid, egid, sgid)
    {
        return Err(SysErrNo::EPERM);
    }

    inner.real_gid = new_r;
    inner.effective_gid = new_e;
    inner.saved_gid = new_s;
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/getresgid.2.html
pub fn sys_getresgid(rgid: *mut u32, egid: *mut u32, sgid: *mut u32) -> SyscallRet {
    let task = current_task().unwrap();
    let proc_inner = task.process.inner_lock();
    let memory_set = proc_inner.get_locked_memory_set_read();
    let inner = task.inner_lock();

    for (ptr, value) in [
        (rgid, inner.real_gid),
        (egid, inner.effective_gid),
        (sgid, inner.saved_gid),
    ] {
        if ptr.is_null() {
            continue;
        }
        if (ptr as isize) <= 0 || if_bad_address(ptr as usize) {
            return Err(SysErrNo::EFAULT);
        }
        copy_to_user(&memory_set, ptr as usize, unsafe {
            core::slice::from_raw_parts(&value as *const u32 as *const u8, core::mem::size_of::<u32>())
        })?;
    }
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
        domainname: get_domainname_bytes(),
    };
    let task = current_task().unwrap();
    let proc_inner = task.process.inner_lock();
    let memory_set = proc_inner.get_locked_memory_set_read();
    copy_to_user(&memory_set, buf as usize, unsafe {
        core::slice::from_raw_parts(&uname as *const Utsname as *const u8, core::mem::size_of::<Utsname>())
    })?;
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/sysinfo.2.html
pub fn sys_sysinfo(info: *const u8) -> SyscallRet {
    let task = current_task().unwrap();
    let proc_inner = task.process.inner_lock();
    let memory_set = proc_inner.get_locked_memory_set_read();
    let sysinfo = Sysinfo::new(get_time_ms() / 1000, 1 << 56, tid_to_task::task_num());
    copy_to_user(&memory_set, info as usize, unsafe {
        core::slice::from_raw_parts(&sysinfo as *const Sysinfo as *const u8, core::mem::size_of::<Sysinfo>())
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
    let proc_inner = task.process.inner_lock();
    let memory_set = proc_inner.get_locked_memory_set_read();

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
        safe_translated_byte_buffer(&memory_set, buf_ptr, buflen).ok_or(SysErrNo::EFAULT)?,
    ))
}

// ---------------------------------------------------------------------------
// capability 系统调用
// ---------------------------------------------------------------------------
/// 对应 C struct __user_cap_header_struct
#[derive(Debug, Clone, Copy, Default)]
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
    let memory_set = proc_inner.get_locked_memory_set_read();

    // EFAULT: 非法地址
    if (hdrp as isize) <= 0 || if_bad_address(hdrp as usize) {
        return Err(SysErrNo::EFAULT);
    }
    if (datap as isize) <= 0 || if_bad_address(datap as usize) {
        return Err(SysErrNo::EFAULT);
    }

    let mut hdr = CapUserHeader::default();
    copy_from_user(&memory_set, hdrp as usize, unsafe {
        core::slice::from_raw_parts_mut(&mut hdr as *mut CapUserHeader as *mut u8, core::mem::size_of::<CapUserHeader>())
    })?;
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
        hdr.version = _LINUX_CAPABILITY_VERSION_3;
        copy_to_user(&memory_set, hdrp as usize, unsafe {
            core::slice::from_raw_parts(&hdr as *const CapUserHeader as *const u8, core::mem::size_of::<CapUserHeader>())
        })?;
        debug!("[capget] unsupported version -> fallback to V3");
        return Err(SysErrNo::EINVAL);
    }

    // 成功: 返回零填充的能力集
    let data = CapUserData {
        effective: 0,
        permitted: 0,
        inheritable: 0,
    };
    copy_to_user(&memory_set, datap as usize, unsafe {
        core::slice::from_raw_parts(&data as *const CapUserData as *const u8, core::mem::size_of::<CapUserData>())
    })?;
    debug!("[capget] success, version=0x{:x}", hdr.version);
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/capset.2.html
pub fn sys_capset(hdrp: *mut CapUserHeader, datap: *const CapUserData) -> SyscallRet {
    // 伪实现: 允许设置但不实际存储
    let task = current_task().unwrap();
    let proc_inner = task.process.inner_lock();
    let memory_set = proc_inner.get_locked_memory_set_read();

    if (hdrp as isize) <= 0 || if_bad_address(hdrp as usize) {
        return Err(SysErrNo::EFAULT);
    }
    if (datap as isize) <= 0 || if_bad_address(datap as usize) {
        return Err(SysErrNo::EFAULT);
    }
    let mut hdr = CapUserHeader::default(); // 假设实现了 Default，或者用 core::mem::zeroed()

    let hdr_ptr = &mut hdr as *mut CapUserHeader as *mut u8;
    let hdr_size = core::mem::size_of::<CapUserHeader>();

    copy_from_user(
        &memory_set, 
        hdrp as usize, 
        unsafe { core::slice::from_raw_parts_mut(hdr_ptr, hdr_size) }
    )?;
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
                let memory_set = proc_inner.get_locked_memory_set_read();
                let sig_val = sig as i32;
                copy_to_user(&memory_set, arg2, unsafe {
                    core::slice::from_raw_parts(&sig_val as *const i32 as *const u8, core::mem::size_of::<i32>())
                })?;
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

// ---------------------------------------------------------------------------
// getgroups(158) / setgroups(159)
// ---------------------------------------------------------------------------

/// 参考 https://man7.org/linux/man-pages/man2/getgroups.2.html
pub fn sys_getgroups(size: usize, list: *mut u32) -> SyscallRet {
    let task = current_task().unwrap();
    let proc_inner = task.process.inner_lock();
    let memory_set = proc_inner.get_locked_memory_set_read();

    // 返回至少一个组 (root: gid=0)
    let count = 1usize;

    if size == 0 {
        // 查询所需缓冲区大小
        debug!("[getgroups] query size -> {}", count);
        return Ok(count);
    }

    if size < count {
        return Err(SysErrNo::EINVAL);
    }

    if (list as isize) <= 0 || if_bad_address(list as usize) {
        return Err(SysErrNo::EFAULT);
    }

    let gid = task.inner_lock().effective_gid;
    copy_to_user(&memory_set, list as usize, unsafe {
        core::slice::from_raw_parts(&gid as *const u32 as *const u8, core::mem::size_of::<u32>())
    })?;
    debug!("[getgroups] wrote {} group", count);
    Ok(count)
}

/// 参考 https://man7.org/linux/man-pages/man2/setgroups.2.html
pub fn sys_setgroups(size: usize, list: *const u32) -> SyscallRet {
    let task = current_task().unwrap();

    // 非 root 不可设置
    if task.inner_lock().user_id != 0 {
        return Err(SysErrNo::EPERM);
    }

    if size == 0 || size > 65536 {
        return Err(SysErrNo::EINVAL);
    }

    if (list as isize) <= 0 || if_bad_address(list as usize) {
        return Err(SysErrNo::EFAULT);
    }

    debug!("[setgroups] size={}", size);
    Ok(0)
}

// ---------------------------------------------------------------------------
// getcpu(168)
// ---------------------------------------------------------------------------

/// 参考 https://man7.org/linux/man-pages/man2/getcpu.2.html
///
/// 返回当前线程所在的 CPU 编号和 NUMA 节点编号。
/// 单核系统下始终返回 cpu=0, node=0。
pub fn sys_getcpu(cpu: *mut u32, node: *mut u32, _tcache: *mut u8) -> SyscallRet {
    let task = current_task().unwrap();
    let proc_inner = task.process.inner_lock();
    let memory_set = proc_inner.get_locked_memory_set_read();

    // tcache must be NULL when called from userspace (used only by vDSO)
    // We ignore it for simplicity

    let cpu_val: u32 = 0;
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

    debug!("[getcpu] cpu=0, node=0");
    Ok(0)
}

// ---------------------------------------------------------------------------
// setdomainname(162)
// ---------------------------------------------------------------------------

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

    let proc_inner = task.process.inner_lock();
    let memory_set = proc_inner.get_locked_memory_set_read();

    if len > 0 {
        if name.is_null() || (name as isize) <= 0 || if_bad_address(name as usize) {
            return Err(SysErrNo::EFAULT);
        }

        let bufs = safe_translated_byte_buffer(&memory_set, name as *mut u8, len)
            .ok_or(SysErrNo::EFAULT)?;

        let mut dn = DOMAIN_NAME.lock();
        dn.fill(0);
        let mut copied = 0usize;
        for buf_slice in bufs {
            let to_copy = core::cmp::min(buf_slice.len(), len - copied);
            dn[copied..copied + to_copy].copy_from_slice(&buf_slice[..to_copy]);
            copied += to_copy;
            if copied >= len {
                break;
            }
        }
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

// ---------------------------------------------------------------------------
// init_module(105) / delete_module(106)
// ---------------------------------------------------------------------------

/// 参考 https://man7.org/linux/man-pages/man2/init_module.2.html
///
/// 加载内核模块。当前内核不支持模块加载。
pub fn sys_init_module(_module_image: *const u8, _len: usize, _param_values: *const u8) -> SyscallRet {
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
