//! POSIX 文件记录锁（advisory record locking）实现。
//!
//! 参考:
//! - linux-7.0/fs/locks.c
//! - linux-7.0/include/uapi/asm-generic/fcntl.h
//! - https://man7.org/linux/man-pages/man2/fcntl.2.html
//!
//! 当前实现为简化的全局锁表，按 inode 路径管理锁列表，
//! 支持 F_SETLK / F_SETLKW / F_GETLK 的基本语义。

use super::types::Flock;
use crate::syscall::fs::fcntl::{F_RDLCK, F_UNLCK, F_WRLCK, SEEK_CUR, SEEK_END, SEEK_SET};
use crate::utils::{SysErrNo, SyscallRet};
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::task::Waker;
use futures_util::task::AtomicWaker;
use spin::{Lazy, RwLock};

// 内部 POSIX 记录锁条目。
//
// 用户态 flock 的 l_start/l_len 可能依赖 SEEK_CUR/SEEK_END，也允许负长度。
// 进入全局锁表前统一转换为 [l_start, l_end] 闭区间，便于后续冲突检测、
// 裁剪和合并。l_pid 使用进程 pid，表示 POSIX record lock 的拥有者。
#[derive(Debug, Clone)]
struct PosixLock {
    l_type: i16,
    l_start: i64, // 绝对起始偏移
    l_end: i64,   // 绝对结束偏移（闭区间），i64::MAX 表示直到文件末尾
    l_pid: i32,
}

// 全局 POSIX 记录锁注册表。
//
// 当前 Ya2yOS 没有完整 inode 对象级锁管理，这里用 inode path 作为稳定键：
// - FILE_LOCKS: fcntl(F_SETLK/F_SETLKW/F_GETLK) 的字节范围锁。
// - POSIX_LOCK_WAITERS: 每个文件一个 waker，用于唤醒阻塞的 F_SETLKW。
// - POSIX_LOCK_WAITS: owner_pid -> 正在等待的持锁 pid 列表，用于死锁检测。
static FILE_LOCKS: Lazy<RwLock<BTreeMap<String, Vec<PosixLock>>>> =
    Lazy::new(|| RwLock::new(BTreeMap::new()));
static POSIX_LOCK_WAITERS: Lazy<RwLock<BTreeMap<String, AtomicWaker>>> =
    Lazy::new(|| RwLock::new(BTreeMap::new()));
static POSIX_LOCK_WAITS: Lazy<RwLock<BTreeMap<i32, Vec<i32>>>> =
    Lazy::new(|| RwLock::new(BTreeMap::new()));

/// 将 flock 中的相对/基于 whence 的偏移转换为绝对字节区间
///
/// Linux 语义中 l_len == 0 表示从起点一直到 EOF；l_len < 0 表示锁区间
/// 反向延伸到起点之前。返回值统一为闭区间，右端为 i64::MAX 时表示 EOF。
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

/// 从同一 owner 已有锁中裁掉 [start, end] 区间。
///
/// POSIX 记录锁允许同一进程重复设置、转换或释放部分区间。设置新锁前先把
/// 与目标区间重叠的旧锁切成左右两段，之后再追加新锁或完成解锁。
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

/// 归一化同一文件上的锁列表。
///
/// 拆分/追加后可能出现同一 owner、同一类型的相邻区间；合并它们可以保持
/// 锁表紧凑，也让后续 F_GETLK 返回更接近 Linux 的最小冲突区间。
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

/// 在已持有 FILE_LOCKS 读/写锁时查找 new_lock 会等待的其它 owner。
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

/// 查询指定 flock 请求当前会被哪些进程阻塞。
///
/// F_SETLKW 的阻塞路径先用该结果构造等待图，再判断是否形成环路。
/// 非法 flock 区间在这里按“无可记录阻塞者”处理，实际错误仍由 setlk 返回。
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

/// 在等待图中检查 current 是否能沿等待边到达 target。
///
/// 若 owner_pid 等待 A，而 A 又直接或间接等待 owner_pid，则 F_SETLKW
/// 应返回 EDEADLK，而不是进入永久等待。
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

/// 判断 owner_pid 等待 waiting_for 中任意进程时是否会形成死锁环。
pub fn would_deadlock(owner_pid: i32, waiting_for: &[i32]) -> bool {
    let graph = POSIX_LOCK_WAITS.read();
    waiting_for.iter().any(|pid| {
        let mut visited = Vec::new();
        wait_path_reaches(&graph, *pid, owner_pid, &mut visited)
    })
}

/// 更新一个 owner 当前等待的持锁者集合。
pub fn record_wait(owner_pid: i32, waiting_for: &[i32]) {
    let mut graph = POSIX_LOCK_WAITS.write();
    if waiting_for.is_empty() {
        graph.remove(&owner_pid);
    } else {
        graph.insert(owner_pid, waiting_for.to_vec());
    }
}

/// 清理 owner 的等待图边。
///
/// 加锁成功、加锁失败退出、文件关闭或进程退出时都需要调用，避免旧等待边
/// 误导后续死锁检测。
pub fn clear_wait(owner_pid: i32) {
    POSIX_LOCK_WAITS.write().remove(&owner_pid);
}

/// 唤醒等待某个 inode path 的阻塞 POSIX 锁请求。
fn wake_posix_waiters(path: &str) {
    if let Some(waker) = POSIX_LOCK_WAITERS.read().get(path) {
        waker.wake();
    }
}

/// 注册 F_SETLKW 当前 task 的 waker。
///
/// AtomicWaker 只保留最近一次注册的 waker；这符合当前简化模型，释放锁时
/// 唤醒等待者重新进入 setlk 尝试。
pub fn register_posix_waker(path: &str, waker: &Waker) {
    let mut waiters = POSIX_LOCK_WAITERS.write();
    waiters
        .entry(String::from(path))
        .or_insert_with(AtomicWaker::new)
        .register(waker);
}

/// 释放指定文件上 owner_pid 持有的全部 POSIX 记录锁。
///
/// 用于关闭 fd 时的清理。若实际移除了锁，唤醒同一文件上的阻塞加锁者。
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

/// 释放 owner_pid 在所有文件上持有的 POSIX 记录锁。
///
/// 用于进程退出路径；需要记录发生变化的 path，锁表写锁释放后再逐一唤醒。
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

    // 同一进程设置新锁或解锁时，目标区间会覆盖原有区间。先把重叠旧锁裁掉，
    // 再根据 l_type 决定是追加新锁还是保持裁剪结果作为解锁结果。
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
    // 裁剪和追加后合并相邻同类区间，避免锁表碎片化。
    normalize_locks(entry);
    drop(locks);

    // 成功设置/释放锁后，阻塞等待者需要重新检查冲突状态。
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
        // F_GETLK 只报告会阻塞当前请求的其它 owner；当前 owner 自己的锁不冲突。
        let conflict = entry
            .iter()
            .filter(|existing| existing.l_pid != owner_pid && locks_conflict(existing, &probe))
            .min_by_key(|existing| (existing.l_start, existing.l_end));

        if let Some(existing) = conflict {
            // 返回值使用 SEEK_SET 坐标，让用户态拿到绝对冲突区间。
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
