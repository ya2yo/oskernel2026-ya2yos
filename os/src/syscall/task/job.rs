use log::debug;

use crate::{
    mm::{copy_to_user, if_bad_address},
    task::{current_task, Process, TaskControlBlockInner, CAPABILITY_U32S},
    utils::{SysErrNo, SyscallRet},
};

/// 参考 https://man7.org/linux/man-pages/man2/getuid.2.html
pub fn sys_getuid() -> SyscallRet {
    let task = current_task().unwrap();
    let task_inner = task.inner_lock();
    // debug!("[sys_getuid] uid={}", task_inner.user_id);
    Ok(task_inner.user_id)
}

/// 参考 https://man7.org/linux/man-pages/man2/geteuid.2.html
pub fn sys_geteuid() -> SyscallRet {
    let task = current_task().unwrap();
    let uid = task.inner_lock().effective_uid as usize;
    // debug!("[sys_geteuid] uid={uid}");
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

fn sync_capabilities_after_uid_change(inner: &mut TaskControlBlockInner) {
    if inner.user_id != 0 && inner.effective_uid != 0 && inner.saved_uid != 0 {
        inner.capabilities.permitted = [0; CAPABILITY_U32S];
    }
    if inner.effective_uid == 0 {
        inner.capabilities.effective = inner.capabilities.permitted;
    } else {
        inner.capabilities.effective = [0; CAPABILITY_U32S];
    }
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
    sync_capabilities_after_uid_change(&mut task_inner);
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/setgid.2.html
pub fn sys_setgid(gid: usize) -> SyscallRet {
    let task = current_task().unwrap();
    let mut task_inner = task.inner_lock();
    if gid > 65535 {
        return Err(SysErrNo::EINVAL);
    }
    let gid = gid as u32;
    task_inner.real_gid = gid;
    task_inner.effective_gid = gid;
    task_inner.saved_gid = gid;
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
    let new_r = if ruid == UID_UNCHANGED { cur_r } else { ruid };
    let new_e = if euid == UID_UNCHANGED { cur_e } else { euid };
    let new_s = if suid == UID_UNCHANGED { cur_s } else { suid };

    (new_r, new_e, new_s)
}

fn setresuid_allowed(
    cur_r: u32,
    cur_e: u32,
    cur_s: u32,
    new_r: u32,
    new_e: u32,
    new_s: u32,
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

    true
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
    sync_capabilities_after_uid_change(&mut inner);

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
    // debug!("[sys_setresuid] r_uid={}, e_uid={}, s_uid={}", cur_r, cur_e,cur_s);
    let (new_r, new_e, new_s) = compute_resuid(cur_r, cur_e, cur_s, ruid, euid, suid);

    let privileged = inner.user_id == 0 || inner.effective_uid == 0;
    if !privileged && !setresuid_allowed(cur_r, cur_e, cur_s, new_r, new_e, new_s) {
        return Err(SysErrNo::EPERM);
    }

    inner.user_id = new_r as usize;
    inner.effective_uid = new_e;
    inner.saved_uid = new_s;
    sync_capabilities_after_uid_change(&mut inner);
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/getresuid.2.html
pub fn sys_getresuid(ruid: *mut u32, euid: *mut u32, suid: *mut u32) -> SyscallRet {
    let task = current_task().unwrap();
    let proc_inner = &task.process;
    let memory_set = proc_inner.memory_set_arc();
    let inner = task.inner_lock();
    let real_uid = inner.user_id as u32;

    for (ptr, value) in [
        (ruid, real_uid),
        (euid, inner.effective_uid),
        (suid, inner.saved_uid),
    ] {
        // debug!("[sys_getresuid] value={value}");
        if ptr.is_null() {
            continue;
        }
        if (ptr as isize) <= 0 || if_bad_address(ptr as usize) {
            return Err(SysErrNo::EFAULT);
        }
        copy_to_user(&memory_set, ptr as usize, unsafe {
            core::slice::from_raw_parts(
                &value as *const u32 as *const u8,
                core::mem::size_of::<u32>(),
            )
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
    if !privileged && !setresgid_allowed(cur_r, cur_e, cur_s, new_r, new_e, new_s, rgid, egid, sgid)
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
    let proc_inner = &task.process;
    let memory_set = proc_inner.memory_set_arc();
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
            core::slice::from_raw_parts(
                &value as *const u32 as *const u8,
                core::mem::size_of::<u32>(),
            )
        })?;
    }
    Ok(0)
}

// getgroups(158) / setgroups(159)
/// 参考 https://man7.org/linux/man-pages/man2/getgroups.2.html
pub fn sys_getgroups(size: usize, list: *mut u32) -> SyscallRet {
    let task = current_task().unwrap();
    let proc_inner = &task.process;
    let memory_set = proc_inner.memory_set_arc();

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

/// https://www.man7.org/linux/man-pages/man2/getsid.2.html
pub fn sys_getsid(pid: u32) -> SyscallRet {
    let target = if pid == 0 {
        // pid 为 0 时返回调用进程的 session ID。
        current_task().ok_or(SysErrNo::ESRCH)?.process.clone()
    } else {
        Process::get_process_arc_by_pid(pid as usize).ok_or(SysErrNo::ESRCH)?
    };
    Ok(target.sid())
}

/// 参考 https://man7.org/linux/man-pages/man2/setsid.2.html
pub fn sys_setsid() -> SyscallRet {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let mut meta = task.process.meta_lock();
    if meta.pgid == task.pid() {
        return Err(SysErrNo::EPERM);
    }
    meta.sid = task.pid();
    meta.pgid = task.pid();
    debug!(
        "[sys_setsid] pid {} new sid {} pgid {}",
        task.pid(),
        meta.sid,
        meta.pgid
    );
    Ok(meta.sid)
}

/// 参考 https://man7.org/linux/man-pages/man2/getpgid.2.html
pub fn sys_getpgid(pid: u32) -> SyscallRet {
    let target = if pid == 0 {
        current_task().ok_or(SysErrNo::ESRCH)?.process.clone()
    } else {
        Process::get_process_arc_by_pid(pid as usize).ok_or(SysErrNo::ESRCH)?
    };
    Ok(target.pgid())
}

/// https://www.man7.org/linux/man-pages/man2/setpgid.2.html
pub fn sys_setpgid(pid: u32, pgid: u32) -> SyscallRet {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let target_pid = if pid == 0 { task.pid() } else { pid as usize };
    let new_pgid = if pgid == 0 { target_pid } else { pgid as usize };
    let target = Process::get_process_arc_by_pid(target_pid).ok_or(SysErrNo::ESRCH)?;
    let current_sid = task.process.sid();
    let mut target_meta = target.meta_lock();
    if target_pid != task.pid() && target_meta.parent_pid != task.pid() {
        return Err(SysErrNo::ESRCH);
    }
    if target_meta.sid != current_sid || target_meta.sid == target_pid {
        return Err(SysErrNo::EPERM);
    }
    target_meta.pgid = new_pgid;
    debug!(
        "[sys_setpgid] pid {} pgid {} sid {}",
        target_pid, new_pgid, target_meta.sid
    );
    Ok(0)
}
