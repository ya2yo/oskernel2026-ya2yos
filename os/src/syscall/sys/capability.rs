use alloc::sync::Arc;

use linux_raw_sys::general::{
    _LINUX_CAPABILITY_VERSION_1, _LINUX_CAPABILITY_VERSION_2, _LINUX_CAPABILITY_VERSION_3,
};
use log::debug;

use crate::{
    mm::{copy_from_user_val, copy_to_user_val, if_bad_address},
    task::{
        current_task, CapabilitySets, Process, TaskControlBlock, CAPABILITY_FULL_MASK,
        CAPABILITY_U32S,
    },
    utils::{SysErrNo, SyscallRet},
};

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

impl Default for CapUserData {
    fn default() -> Self {
        Self {
            effective: 0,
            permitted: 0,
            inheritable: 0,
        }
    }
}

const CAPABILITY_U32S_1: usize = 1;
const CAPABILITY_U32S_2: usize = 2;
const CAPABILITY_U32S_3: usize = 2;

fn cap_version_u32s(version: u32) -> Option<usize> {
    match version {
        _LINUX_CAPABILITY_VERSION_1 => Some(CAPABILITY_U32S_1),
        _LINUX_CAPABILITY_VERSION_2 => Some(CAPABILITY_U32S_2),
        _LINUX_CAPABILITY_VERSION_3 => Some(CAPABILITY_U32S_3),
        _ => None,
    }
}

fn cap_data_user_ptr(datap: *const CapUserData, index: usize) -> *const CapUserData {
    (datap as usize + index * core::mem::size_of::<CapUserData>()) as *const CapUserData
}

fn cap_data_user_mut_ptr(datap: *mut CapUserData, index: usize) -> *mut CapUserData {
    (datap as usize + index * core::mem::size_of::<CapUserData>()) as *mut CapUserData
}

fn cap_target_task(
    pid: i32,
    current: &Arc<TaskControlBlock>,
) -> Result<Arc<TaskControlBlock>, SysErrNo> {
    if pid == 0 || pid as usize == current.pid() {
        return Ok(Arc::clone(current));
    }
    let process = Process::get_process_arc_by_pid(pid as usize).ok_or(SysErrNo::ESRCH)?;
    let target = process
        .meta_lock()
        .tasks
        .iter()
        .find_map(|task| task.upgrade());
    target.ok_or(SysErrNo::ESRCH)
}

fn cap_data_from_sets(caps: CapabilitySets, index: usize) -> CapUserData {
    CapUserData {
        effective: caps.effective[index],
        permitted: caps.permitted[index],
        inheritable: caps.inheritable[index],
    }
}

fn cap_sets_from_data(data: &[CapUserData; CAPABILITY_U32S]) -> CapabilitySets {
    let mut caps = CapabilitySets {
        effective: [0; CAPABILITY_U32S],
        permitted: [0; CAPABILITY_U32S],
        inheritable: [0; CAPABILITY_U32S],
    };
    for i in 0..CAPABILITY_U32S {
        caps.effective[i] = data[i].effective;
        caps.permitted[i] = data[i].permitted;
        caps.inheritable[i] = data[i].inheritable;
    }
    caps
}

fn cap_data_has_unknown_bits(data: &[CapUserData; CAPABILITY_U32S], u32s: usize) -> bool {
    for i in 0..u32s {
        let invalid = !CAPABILITY_FULL_MASK[i];
        if (data[i].effective | data[i].permitted | data[i].inheritable) & invalid != 0 {
            return true;
        }
    }
    false
}

fn cap_subset(lhs: &[u32; CAPABILITY_U32S], rhs: &[u32; CAPABILITY_U32S]) -> bool {
    (0..CAPABILITY_U32S).all(|i| lhs[i] & !rhs[i] == 0)
}

fn cap_union_subset(
    lhs: &[u32; CAPABILITY_U32S],
    rhs_a: &[u32; CAPABILITY_U32S],
    rhs_b: &[u32; CAPABILITY_U32S],
) -> bool {
    (0..CAPABILITY_U32S).all(|i| lhs[i] & !(rhs_a[i] | rhs_b[i]) == 0)
}

/// 参考 https://man7.org/linux/man-pages/man2/capget.2.html
pub fn sys_capget(hdrp: *mut CapUserHeader, datap: *mut CapUserData) -> SyscallRet {
    let task = current_task().unwrap();
    let proc_inner = &task.process;
    let memory_set = proc_inner.memory_set_arc();

    // EFAULT: 非法地址
    if (hdrp as isize) <= 0 || if_bad_address(hdrp as usize) {
        return Err(SysErrNo::EFAULT);
    }
    if (datap as isize) <= 0 || if_bad_address(datap as usize) {
        return Err(SysErrNo::EFAULT);
    }

    let mut hdr = copy_from_user_val(&memory_set, hdrp as *const CapUserHeader)?;
    debug!("[capget] version=0x{:x}, pid={}", hdr.version, hdr.pid);

    // EINVAL: pid < 0 (仅 pid == 0 表示自身)
    if hdr.pid < 0 {
        debug!("[capget] negative pid -> EINVAL");
        return Err(SysErrNo::EINVAL);
    }

    // 版本检查: 如果传入了不支持的值，回填内核首选版本
    let Some(u32s) = cap_version_u32s(hdr.version) else {
        hdr.version = _LINUX_CAPABILITY_VERSION_3;
        copy_to_user_val(&memory_set, hdrp, &hdr)?;
        debug!("[capget] unsupported version -> fallback to V3");
        return Err(SysErrNo::EINVAL);
    };

    let target = cap_target_task(hdr.pid, &task)?;
    let caps = target.inner_lock().capabilities;
    for i in 0..u32s {
        let data = cap_data_from_sets(caps, i);
        copy_to_user_val(&memory_set, cap_data_user_mut_ptr(datap, i), &data)?;
    }
    debug!("[capget] success, version=0x{:x}", hdr.version);
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/capset.2.html
pub fn sys_capset(hdrp: *mut CapUserHeader, datap: *const CapUserData) -> SyscallRet {
    let task = current_task().unwrap();
    let proc_inner = &task.process;
    let memory_set = proc_inner.memory_set_arc();

    if (hdrp as isize) <= 0 || if_bad_address(hdrp as usize) {
        return Err(SysErrNo::EFAULT);
    }
    if (datap as isize) <= 0 || if_bad_address(datap as usize) {
        return Err(SysErrNo::EFAULT);
    }
    let mut hdr = copy_from_user_val(&memory_set, hdrp as *const CapUserHeader)?;
    debug!("[capset] version=0x{:x}, pid={}", hdr.version, hdr.pid);

    if hdr.pid < 0 {
        return Err(SysErrNo::EINVAL);
    }
    if hdr.pid > 0 && hdr.pid as usize != task.pid() {
        return Err(SysErrNo::ESRCH);
    }

    let Some(u32s) = cap_version_u32s(hdr.version) else {
        hdr.version = _LINUX_CAPABILITY_VERSION_3;
        copy_to_user_val(&memory_set, hdrp, &hdr)?;
        return Err(SysErrNo::EINVAL);
    };

    let mut requested = [CapUserData::default(); CAPABILITY_U32S];
    for i in 0..u32s {
        requested[i] = copy_from_user_val(&memory_set, cap_data_user_ptr(datap, i))?;
    }
    if cap_data_has_unknown_bits(&requested, u32s) {
        return Err(SysErrNo::EINVAL);
    }

    let requested_caps = cap_sets_from_data(&requested);
    if !cap_subset(&requested_caps.effective, &requested_caps.permitted) {
        return Err(SysErrNo::EPERM);
    }

    let mut task_inner = task.inner_lock();
    let old_caps = task_inner.capabilities;
    let privileged = task_inner.effective_uid == 0;
    if !privileged {
        if !cap_subset(&requested_caps.permitted, &old_caps.permitted)
            || !cap_union_subset(
                &requested_caps.inheritable,
                &old_caps.inheritable,
                &old_caps.permitted,
            )
        {
            return Err(SysErrNo::EPERM);
        }
    }

    task_inner.capabilities = requested_caps;
    debug!("[capset] success, version=0x{:x}", hdr.version);
    Ok(0)
}
