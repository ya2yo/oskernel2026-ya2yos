//! POSIX 文件记录锁（advisory record locking）实现
//!
//! 参考:
//! - linux-7.0/fs/locks.c
//! - linux-7.0/include/uapi/asm-generic/fcntl.h
//! - https://man7.org/linux/man-pages/man2/fcntl.2.html
//!
//! 当前实现为简化的全局锁表，按 inode 路径管理锁列表，
//! 支持 F_SETLK / F_SETLKW / F_GETLK 的基本语义。

use super::fcntl::{
    F_RDLCK, F_UNLCK, F_WRLCK, LOCK_EX, LOCK_SH, LOCK_UN, SEEK_CUR, SEEK_END, SEEK_SET,
};
use crate::utils::{SysErrNo, SyscallRet};
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::task::Waker;
use futures_util::task::AtomicWaker;
use spin::RwLock;

/// 与 Linux struct flock 布局兼容（64 位平台）
///
/// C 布局:
/// ```c
/// struct flock {
///     short l_type;     // offset 0
///     short l_whence;   // offset 2
///     off_t l_start;    // offset 8  (padding after l_whence)
///     off_t l_len;      // offset 16
///     pid_t l_pid;      // offset 24
/// };
/// ```
/// 总大小 = 32 字节（尾部 padding 到 8 字节对齐）
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Flock {
    pub l_type: i16,
    pub l_whence: i16,
    pub l_start: i64,
    pub l_len: i64,
    pub l_pid: i32,
}

impl Flock {
    /// 从原始字节构造（从用户空间拷贝后使用）
    /// 期望 28 字节（不含尾部 padding）
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 28 {
            return None;
        }
        Some(Flock {
            l_type: i16::from_ne_bytes([bytes[0], bytes[1]]),
            l_whence: i16::from_ne_bytes([bytes[2], bytes[3]]),
            // bytes[4..8] 为 padding，跳过
            l_start: i64::from_ne_bytes([
                bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14],
                bytes[15],
            ]),
            l_len: i64::from_ne_bytes([
                bytes[16], bytes[17], bytes[18], bytes[19], bytes[20], bytes[21], bytes[22],
                bytes[23],
            ]),
            l_pid: i32::from_ne_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]),
        })
    }

    /// 将自身写入字节数组（用于 copy_to_user）
    pub fn to_bytes(&self) -> [u8; 32] {
        let mut bytes = [0u8; 32];
        bytes[0..2].copy_from_slice(&self.l_type.to_ne_bytes());
        bytes[2..4].copy_from_slice(&self.l_whence.to_ne_bytes());
        // bytes[4..8] 保持为 0 (padding)
        bytes[8..16].copy_from_slice(&self.l_start.to_ne_bytes());
        bytes[16..24].copy_from_slice(&self.l_len.to_ne_bytes());
        bytes[24..28].copy_from_slice(&self.l_pid.to_ne_bytes());
        // 部分 libc/架构组合把 l_pid 放在尾部 padding 位置，双写可兼容两种布局。
        bytes[28..32].copy_from_slice(&self.l_pid.to_ne_bytes());
        bytes
    }
}

// ---------------------------------------------------------------------------
// 内部锁结构（使用绝对坐标）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct PosixLock {
    l_type: i16,
    l_start: i64, // 绝对起始偏移
    l_end: i64,   // 绝对结束偏移（闭区间），i64::MAX 表示直到文件末尾
    l_pid: i32,
}

#[derive(Debug, Clone)]
struct FileLease {
    l_type: i16,
    l_pid: i32,
}

// ---------------------------------------------------------------------------
// 全局文件锁注册表
// ---------------------------------------------------------------------------

use spin::Lazy;
static FILE_LOCKS: Lazy<RwLock<BTreeMap<String, Vec<PosixLock>>>> =
    Lazy::new(|| RwLock::new(BTreeMap::new()));
static FILE_LEASES: Lazy<RwLock<BTreeMap<String, Vec<FileLease>>>> =
    Lazy::new(|| RwLock::new(BTreeMap::new()));
static POSIX_LOCK_WAITERS: Lazy<RwLock<BTreeMap<String, AtomicWaker>>> =
    Lazy::new(|| RwLock::new(BTreeMap::new()));
static POSIX_LOCK_WAITS: Lazy<RwLock<BTreeMap<i32, Vec<i32>>>> =
    Lazy::new(|| RwLock::new(BTreeMap::new()));

// ---------------------------------------------------------------------------
// 辅助函数
// ---------------------------------------------------------------------------

/// 将 flock 中的相对/基于 whence 的偏移转换为绝对字节区间
fn to_absolute(fl: &Flock, file_size: i64, current_offset: i64) -> Result<(i64, i64), SysErrNo> {
    let start = match fl.l_whence {
        SEEK_SET => fl.l_start,
        SEEK_CUR => current_offset.saturating_add(fl.l_start),
        SEEK_END => file_size.saturating_add(fl.l_start),
        _ => return Err(SysErrNo::EINVAL),
    };

    if start < 0 {
        return Err(SysErrNo::EINVAL);
    }

    let (start, end) = if fl.l_len == 0 {
        (start, i64::MAX) // 直到 EOF
    } else if fl.l_len < 0 {
        let lock_start = start.saturating_add(fl.l_len).saturating_add(1);
        if lock_start < 0 {
            return Err(SysErrNo::EINVAL);
        }
        (lock_start, start)
    } else {
        (start, start.saturating_add(fl.l_len).saturating_sub(1))
    };

    Ok((start, end))
}

/// 检测两个锁是否冲突
fn locks_conflict(l1: &PosixLock, l2: &PosixLock) -> bool {
    // 同为读锁时不冲突
    if l1.l_type == F_RDLCK && l2.l_type == F_RDLCK {
        return false;
    }
    // 区间重叠检查
    l1.l_start <= l2.l_end && l2.l_start <= l1.l_end
}

fn ranges_overlap(start1: i64, end1: i64, start2: i64, end2: i64) -> bool {
    start1 <= end2 && start2 <= end1
}

fn ranges_touch_or_overlap(start1: i64, end1: i64, start2: i64, end2: i64) -> bool {
    start1 <= end2.saturating_add(1) && start2 <= end1.saturating_add(1)
}

fn split_owned_lock(existing: &PosixLock, start: i64, end: i64, out: &mut Vec<PosixLock>) {
    if existing.l_start < start {
        out.push(PosixLock {
            l_type: existing.l_type,
            l_start: existing.l_start,
            l_end: start.saturating_sub(1),
            l_pid: existing.l_pid,
        });
    }

    if end != i64::MAX && existing.l_end > end {
        out.push(PosixLock {
            l_type: existing.l_type,
            l_start: end.saturating_add(1),
            l_end: existing.l_end,
            l_pid: existing.l_pid,
        });
    }
}

fn normalize_locks(locks: &mut Vec<PosixLock>) {
    locks.sort_by_key(|lock| (lock.l_start, lock.l_end, lock.l_pid, lock.l_type));

    let mut merged: Vec<PosixLock> = Vec::new();
    for lock in locks.drain(..) {
        if let Some(last) = merged.last_mut() {
            if last.l_pid == lock.l_pid
                && last.l_type == lock.l_type
                && ranges_touch_or_overlap(last.l_start, last.l_end, lock.l_start, lock.l_end)
            {
                last.l_end = last.l_end.max(lock.l_end);
                continue;
            }
        }
        merged.push(lock);
    }
    *locks = merged;
}

fn conflicting_owners_locked(entry: &[PosixLock], new_lock: &PosixLock) -> Vec<i32> {
    let mut owners = Vec::new();
    for existing in entry.iter() {
        if existing.l_pid != new_lock.l_pid && locks_conflict(existing, new_lock) {
            if !owners.contains(&existing.l_pid) {
                owners.push(existing.l_pid);
            }
        }
    }
    owners
}

pub fn conflicting_owners(
    path: &str,
    fl: &Flock,
    file_size: i64,
    current_offset: i64,
    owner_pid: i32,
) -> Vec<i32> {
    if fl.l_type == F_UNLCK {
        return Vec::new();
    }
    let Ok((start, end)) = to_absolute(fl, file_size, current_offset) else {
        return Vec::new();
    };
    let new_lock = PosixLock {
        l_type: fl.l_type,
        l_start: start,
        l_end: end,
        l_pid: owner_pid,
    };

    let locks = FILE_LOCKS.read();
    locks
        .get(path)
        .map(|entry| conflicting_owners_locked(entry, &new_lock))
        .unwrap_or_default()
}

fn wait_path_reaches(
    graph: &BTreeMap<i32, Vec<i32>>,
    current: i32,
    target: i32,
    visited: &mut Vec<i32>,
) -> bool {
    if current == target {
        return true;
    }
    if visited.contains(&current) {
        return false;
    }
    visited.push(current);

    graph
        .get(&current)
        .map(|next| {
            next.iter()
                .any(|pid| wait_path_reaches(graph, *pid, target, visited))
        })
        .unwrap_or(false)
}

pub fn would_deadlock(owner_pid: i32, waiting_for: &[i32]) -> bool {
    let graph = POSIX_LOCK_WAITS.read();
    waiting_for.iter().any(|pid| {
        let mut visited = Vec::new();
        wait_path_reaches(&graph, *pid, owner_pid, &mut visited)
    })
}

pub fn record_wait(owner_pid: i32, waiting_for: &[i32]) {
    let mut graph = POSIX_LOCK_WAITS.write();
    if waiting_for.is_empty() {
        graph.remove(&owner_pid);
    } else {
        graph.insert(owner_pid, waiting_for.to_vec());
    }
}

pub fn clear_wait(owner_pid: i32) {
    POSIX_LOCK_WAITS.write().remove(&owner_pid);
}

fn wake_posix_waiters(path: &str) {
    if let Some(waker) = POSIX_LOCK_WAITERS.read().get(path) {
        waker.wake();
    }
}

pub fn register_posix_waker(path: &str, waker: &Waker) {
    let mut waiters = POSIX_LOCK_WAITERS.write();
    waiters
        .entry(String::from(path))
        .or_insert_with(AtomicWaker::new)
        .register(waker);
}

pub fn release_posix_locks(path: &str, owner_pid: i32) {
    let mut changed = false;
    {
        let mut locks = FILE_LOCKS.write();
        if let Some(entry) = locks.get_mut(path) {
            let before = entry.len();
            entry.retain(|lock| lock.l_pid != owner_pid);
            changed = entry.len() != before;
            if entry.is_empty() {
                locks.remove(path);
            }
        }
    }
    clear_wait(owner_pid);
    if changed {
        wake_posix_waiters(path);
    }
}

pub fn release_file_leases(path: &str, owner_pid: i32) {
    let mut leases = FILE_LEASES.write();
    if let Some(entry) = leases.get_mut(path) {
        entry.retain(|lease| lease.l_pid != owner_pid);
        if entry.is_empty() {
            leases.remove(path);
        }
    }
}

pub fn release_posix_locks_by_owner(owner_pid: i32) {
    let mut changed_paths = Vec::new();
    {
        let mut locks = FILE_LOCKS.write();
        for (path, entry) in locks.iter_mut() {
            let before = entry.len();
            entry.retain(|lock| lock.l_pid != owner_pid);
            if entry.len() != before {
                changed_paths.push(path.clone());
            }
        }
        locks.retain(|_, entry| !entry.is_empty());
    }
    clear_wait(owner_pid);
    for path in changed_paths {
        wake_posix_waiters(&path);
    }
}

pub fn release_file_leases_by_owner(owner_pid: i32) {
    let mut leases = FILE_LEASES.write();
    for entry in leases.values_mut() {
        entry.retain(|lease| lease.l_pid != owner_pid);
    }
    leases.retain(|_, entry| !entry.is_empty());
}

fn file_leases_conflict(existing: &FileLease, new_lease: &FileLease) -> bool {
    existing.l_pid != new_lease.l_pid && (existing.l_type == F_WRLCK || new_lease.l_type == F_WRLCK)
}

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

pub fn get_file_lease(path: &str, owner_pid: i32) -> i16 {
    let leases = FILE_LEASES.read();
    leases
        .get(path)
        .and_then(|entry| entry.iter().find(|lease| lease.l_pid == owner_pid))
        .map(|lease| lease.l_type)
        .unwrap_or(F_UNLCK)
}

// ---------------------------------------------------------------------------
// 公共接口
// ---------------------------------------------------------------------------

/// F_SETLK / F_SETLKW 的处理核心
///
/// - `path`: 文件的 inode 路径，用作锁表键
/// - `fl`: 用户传入的 flock 结构
/// - `file_size`: 文件大小，用于 SEEK_END 计算
///
/// 若 `fl.l_type == F_UNLCK` 则释放锁；否则尝试获取锁。
/// 返回 `Ok(0)` 表示成功，`Err(EAGAIN)` 表示存在冲突。
pub fn setlk(
    path: &str,
    fl: &Flock,
    file_size: i64,
    current_offset: i64,
    owner_pid: i32,
) -> SyscallRet {
    if fl.l_type != F_RDLCK && fl.l_type != F_WRLCK && fl.l_type != F_UNLCK {
        return Err(SysErrNo::EINVAL);
    }

    let (start, end) = to_absolute(fl, file_size, current_offset)?;

    let new_lock = PosixLock {
        l_type: fl.l_type,
        l_start: start,
        l_end: end,
        l_pid: owner_pid,
    };

    let mut locks = FILE_LOCKS.write();
    let entry = locks.entry(String::from(path)).or_insert_with(Vec::new);

    // 尝试获取锁：只和其它进程的冲突锁互斥。同一进程的锁会在下方转换。
    if fl.l_type != F_UNLCK {
        for existing in entry.iter() {
            if existing.l_pid != owner_pid && locks_conflict(existing, &new_lock) {
                return Err(SysErrNo::EAGAIN);
            }
        }
    }

    let mut updated = Vec::new();
    for existing in entry.iter() {
        if existing.l_pid == owner_pid
            && ranges_overlap(existing.l_start, existing.l_end, start, end)
        {
            split_owned_lock(existing, start, end, &mut updated);
        } else {
            updated.push(existing.clone());
        }
    }

    *entry = updated;
    if fl.l_type != F_UNLCK {
        entry.push(new_lock);
    }
    normalize_locks(entry);
    drop(locks);

    wake_posix_waiters(path);

    Ok(0)
}

/// F_GETLK 的处理核心
///
/// 查询在 `fl` 描述的位置上是否存在冲突锁。
/// 若有冲突，`fl` 的 `l_type` / `l_pid` / `l_start` / `l_len` / `l_whence`
/// 会被更新为冲突锁的信息。
/// 若无冲突，`fl.l_type` 会被设置为 `F_UNLCK`。
pub fn getlk(
    path: &str,
    fl: &mut Flock,
    file_size: i64,
    current_offset: i64,
    owner_pid: i32,
) -> SyscallRet {
    if fl.l_type != F_RDLCK && fl.l_type != F_WRLCK {
        return Err(SysErrNo::EINVAL);
    }

    let (start, end) = to_absolute(fl, file_size, current_offset)?;

    let probe = PosixLock {
        l_type: fl.l_type,
        l_start: start,
        l_end: end,
        l_pid: 0,
    };

    let locks = FILE_LOCKS.read();
    if let Some(entry) = locks.get(path) {
        let conflict = entry
            .iter()
            .filter(|existing| existing.l_pid != owner_pid && locks_conflict(existing, &probe))
            .min_by_key(|existing| (existing.l_start, existing.l_end));

        if let Some(existing) = conflict {
            fl.l_type = existing.l_type;
            fl.l_pid = existing.l_pid;
            fl.l_start = existing.l_start;
            fl.l_len = if existing.l_end == i64::MAX {
                0
            } else {
                existing.l_end - existing.l_start + 1
            };
            fl.l_whence = SEEK_SET;
            return Ok(0);
        }
    }

    // 无冲突锁
    fl.l_type = F_UNLCK;
    Ok(0)
}

// ---------------------------------------------------------------------------
// flock() 锁实现
//
// 与 fcntl 记录锁不同，flock 锁：
// - 作用于整个文件（非字节范围）
// - 与打开的文件描述关联（非进程）
// - 同一文件描述的重复加锁会进行锁转换
// - LOCK_NB 未设置时阻塞等待直到锁可用
// - 文件关闭时自动释放（当前简化实现未自动释放）
// ---------------------------------------------------------------------------

/// flock 锁条目：记录锁类型与持有者（打开的文件描述）
#[derive(Debug, Clone)]
struct FlockOwner {
    lock_type: i32,
    file_ptr: usize, // Arc<OSFile> 数据指针，唯一标识打开的文件描述
}

/// 每个 inode 的 flock 锁状态
struct FlockInodeState {
    locks: Vec<FlockOwner>,
    waker: AtomicWaker, // 唤醒阻塞等待的 flock 调用者
}

/// 全局 flock 锁表，按 inode 路径索引
static FLOCK_TABLE: Lazy<RwLock<BTreeMap<String, FlockInodeState>>> =
    Lazy::new(|| RwLock::new(BTreeMap::new()));

/// 非阻塞尝试获取 flock 锁
///
/// 若成功则返回 `Ok(())`；若存在冲突锁则返回 `Err(EAGAIN)`。
/// 同一文件描述已有锁时自动进行锁类型转换（先移除旧锁）。
pub fn flock_try_lock(path: &str, file_ptr: usize, lock_type: i32) -> Result<(), SysErrNo> {
    let mut table = FLOCK_TABLE.write();
    let state = table
        .entry(String::from(path))
        .or_insert_with(|| FlockInodeState {
            locks: Vec::new(),
            waker: AtomicWaker::new(),
        });

    // 先移除同一文件描述持有的旧锁（锁类型转换）
    state.locks.retain(|e| e.file_ptr != file_ptr);

    // 冲突检测：
    // - LOCK_EX 与任何已有锁冲突
    // - LOCK_SH 仅与已有 LOCK_EX 冲突
    for existing in state.locks.iter() {
        if existing.lock_type == LOCK_EX || lock_type == LOCK_EX {
            return Err(SysErrNo::EAGAIN);
        }
    }

    // 无冲突，添加新锁
    state.locks.push(FlockOwner {
        lock_type,
        file_ptr,
    });
    Ok(())
}

/// 释放 flock 锁并唤醒阻塞等待者
///
/// 移除该文件描述在此 inode 上持有的所有锁，然后唤醒可能阻塞的 waiter。
pub fn flock_unlock(path: &str, file_ptr: usize) {
    let mut table = FLOCK_TABLE.write();
    if let Some(state) = table.get_mut(path) {
        state.locks.retain(|e| e.file_ptr != file_ptr);
        state.waker.wake();
    }
}

/// 为指定 inode 注册 waker（由阻塞等待的 poll_fn 调用）
///
/// 在 `flock_try_lock` 返回 `EAGAIN` 后调用，确保在锁释放时能被唤醒。
pub fn flock_register_waker(path: &str, waker: &Waker) {
    let table = FLOCK_TABLE.read();
    if let Some(state) = table.get(path) {
        state.waker.register(waker);
    }
}
