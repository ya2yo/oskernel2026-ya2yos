//! `flock(2)` 整文件 advisory lock 实现。
//!
//! 与 fcntl 记录锁不同，flock 锁：
//! - 作用于整个文件（非字节范围）
//! - 与打开的文件描述关联（非进程）
//! - 同一文件描述的重复加锁会进行锁转换
//! - LOCK_NB 未设置时阻塞等待直到锁可用

use crate::syscall::fs::fcntl::LOCK_EX;
use crate::utils::SysErrNo;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::task::Waker;
use futures_util::task::AtomicWaker;
use spin::{Lazy, RwLock};

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
