//! `fcntl(F_SETLEASE/F_GETLEASE)` 文件租约的简化实现。
//!
//! Linux 的 lease 还涉及异步通知、break lease 等复杂机制；当前仅维护
//! 对测试可见的基本互斥状态。

use crate::syscall::fs::fcntl::{F_RDLCK, F_UNLCK, F_WRLCK};
use crate::utils::{SysErrNo, SyscallRet};
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use spin::{Lazy, RwLock};

#[derive(Debug, Clone)]
struct FileLease {
    l_type: i16,
    l_pid: i32,
}

static FILE_LEASES: Lazy<RwLock<BTreeMap<String, Vec<FileLease>>>> =
    Lazy::new(|| RwLock::new(BTreeMap::new()));

/// lease 冲突规则：同一 pid 可替换自己的 lease；不同 pid 之间只要任一方
/// 是写 lease 就冲突，多个读 lease 可共存。
fn file_leases_conflict(existing: &FileLease, new_lease: &FileLease) -> bool {
    existing.l_pid != new_lease.l_pid && (existing.l_type == F_WRLCK || new_lease.l_type == F_WRLCK)
}

/// 设置或释放文件 lease。
pub fn set_file_lease(
    path: &str,
    lease_type: i16,
    owner_pid: i32,
    fd_opened_for_write: bool,
) -> SyscallRet {
    if lease_type != F_RDLCK && lease_type != F_WRLCK && lease_type != F_UNLCK {
        return Err(SysErrNo::EINVAL);
    }

    if lease_type == F_RDLCK && fd_opened_for_write {
        return Err(SysErrNo::EAGAIN);
    }

    let mut leases = FILE_LEASES.write();
    let entry = leases.entry(String::from(path)).or_insert_with(Vec::new);

    if lease_type == F_UNLCK {
        entry.retain(|lease| lease.l_pid != owner_pid);
        if entry.is_empty() {
            leases.remove(path);
        }
        return Ok(0);
    }

    let new_lease = FileLease {
        l_type: lease_type,
        l_pid: owner_pid,
    };
    if entry
        .iter()
        .any(|existing| file_leases_conflict(existing, &new_lease))
    {
        return Err(SysErrNo::EAGAIN);
    }

    if let Some(existing) = entry.iter_mut().find(|lease| lease.l_pid == owner_pid) {
        existing.l_type = lease_type;
    } else {
        entry.push(new_lease);
    }

    Ok(0)
}

/// 查询 owner_pid 在指定文件上的 lease 类型；未持有时返回 F_UNLCK。
pub fn get_file_lease(path: &str, owner_pid: i32) -> i16 {
    let leases = FILE_LEASES.read();
    leases
        .get(path)
        .and_then(|entry| entry.iter().find(|lease| lease.l_pid == owner_pid))
        .map(|lease| lease.l_type)
        .unwrap_or(F_UNLCK)
}

/// 释放指定文件上 owner_pid 持有的 lease。
pub fn release_file_leases(path: &str, owner_pid: i32) {
    let mut leases = FILE_LEASES.write();
    if let Some(entry) = leases.get_mut(path) {
        entry.retain(|lease| lease.l_pid != owner_pid);
        if entry.is_empty() {
            leases.remove(path);
        }
    }
}

/// 释放 owner_pid 在所有文件上持有的 lease。
pub fn release_file_leases_by_owner(owner_pid: i32) {
    let mut leases = FILE_LEASES.write();
    for entry in leases.values_mut() {
        entry.retain(|lease| lease.l_pid != owner_pid);
    }
    leases.retain(|_, entry| !entry.is_empty());
}
