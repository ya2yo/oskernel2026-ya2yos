//! `capget(2)` / `capset(2)` 系统调用实现。
//!
//! 通过用户态传入的 header（版本 + 目标 pid）和 data 数组读写目标进程的
//! capability 集合（effective / permitted / inheritable），行为与 Linux
//! 保持一致：版本回填、未知 capability 位检查、capset 的权限约束等。

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
    /// capability 接口版本，见 `_LINUX_CAPABILITY_VERSION_*`，决定 data 数组长度
    pub version: u32,
    /// 目标进程 pid；0 表示当前进程
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

/// V1 版本 capability 数据长度为 1 个 u32（仅低 32 个 capability）
const CAPABILITY_U32S_1: usize = 1;
/// V2 版本 capability 数据长度为 2 个 u32
const CAPABILITY_U32S_2: usize = 2;
/// V3 版本 capability 数据长度与 V2 相同（仅文件能力标志的语义不同）
const CAPABILITY_U32S_3: usize = 2;

/// 根据 capability 版本号返回对应的数据数组长度（u32 个数）。
///
/// 不支持的版本返回 `None`，由调用方回填内核首选版本并返回 `EINVAL`。
fn cap_version_u32s(version: u32) -> Option<usize> {
    match version {
        _LINUX_CAPABILITY_VERSION_1 => Some(CAPABILITY_U32S_1),
        _LINUX_CAPABILITY_VERSION_2 => Some(CAPABILITY_U32S_2),
        _LINUX_CAPABILITY_VERSION_3 => Some(CAPABILITY_U32S_3),
        _ => None,
    }
}

/// 返回用户态 data 数组第 `index` 个元素的只读指针。
fn cap_data_user_ptr(datap: *const CapUserData, index: usize) -> *const CapUserData {
    (datap as usize + index * core::mem::size_of::<CapUserData>()) as *const CapUserData
}

/// 返回用户态 data 数组第 `index` 个元素的可写指针。
fn cap_data_user_mut_ptr(datap: *mut CapUserData, index: usize) -> *mut CapUserData {
    (datap as usize + index * core::mem::size_of::<CapUserData>()) as *mut CapUserData
}

/// 解析 header 中的 pid 得到目标任务。
///
/// `pid == 0` 或等于当前线程 pid 时操作当前任务，否则按 pid 查找进程并取其
/// 任意一个线程；进程不存在返回 `ESRCH`。
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

/// 取 capability 集合第 `index` 个 u32 组成一份用户态 data 结构。
fn cap_data_from_sets(caps: CapabilitySets, index: usize) -> CapUserData {
    CapUserData {
        effective: caps.effective[index],
        permitted: caps.permitted[index],
        inheritable: caps.inheritable[index],
    }
}

/// 将用户态 data 数组（2 个 u32 槽位）组装为内核 capability 集合。
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

/// 检查请求的 data 数组是否设置了未定义（超出 `CAP_LAST_CAP`）的 capability 位。
///
/// 只检查前 `u32s` 个槽位；存在未知位返回 `true`，调用方应返回 `EINVAL`。
fn cap_data_has_unknown_bits(data: &[CapUserData; CAPABILITY_U32S], u32s: usize) -> bool {
    for i in 0..u32s {
        let invalid = !CAPABILITY_FULL_MASK[i];
        if (data[i].effective | data[i].permitted | data[i].inheritable) & invalid != 0 {
            return true;
        }
    }
    false
}

/// 逐 u32 判断 `lhs` 是否为 `rhs` 的子集（lhs 中每一位都包含于 rhs）。
fn cap_subset(lhs: &[u32; CAPABILITY_U32S], rhs: &[u32; CAPABILITY_U32S]) -> bool {
    (0..CAPABILITY_U32S).all(|i| lhs[i] & !rhs[i] == 0)
}

/// 逐 u32 判断 `lhs` 是否为 `rhs_a` 与 `rhs_b` 并集的子集。
fn cap_union_subset(
    lhs: &[u32; CAPABILITY_U32S],
    rhs_a: &[u32; CAPABILITY_U32S],
    rhs_b: &[u32; CAPABILITY_U32S],
) -> bool {
    (0..CAPABILITY_U32S).all(|i| lhs[i] & !(rhs_a[i] | rhs_b[i]) == 0)
}

/// `capget(2)`：读取目标进程的 capability 集合。
///
/// - `hdrp` 指向 header（版本 + pid），`datap` 指向输出 data 数组；
/// - `pid < 0` 返回 `EINVAL`；不支持的版本会回填内核首选版本（V3）后返回 `EINVAL`；
/// - 目标进程不存在返回 `ESRCH`。
///
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

/// `capset(2)`：设置目标进程的 capability 集合。
///
/// - 仅允许操作自身（`pid == 0` 或等于当前 pid），其它 pid 返回 `ESRCH`；
/// - `pid < 0` 或不支持的版本（回填 V3 后）返回 `EINVAL`；
/// - 设置了未定义的 capability 位返回 `EINVAL`；
/// - effective 必须是 permitted 的子集，否则返回 `EPERM`；
/// - 非特权进程（effective uid != 0）只能收缩 permitted、且 inheritable 不得
///   超出旧 inheritable 与旧 permitted 的并集，否则返回 `EPERM`。
///
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
