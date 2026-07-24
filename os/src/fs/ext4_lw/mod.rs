//! lwext4-backed EXT4 文件系统适配层。
//!
//! 该模块把第三方 `lwext4_rust` wrapper 暴露的路径式文件系统接口适配成
//! Ya2yOS 内部的 VFS trait：
//!
//! - [`inode`]：将 `Ext4File` 包装成 VFS [`Inode`](crate::fs::Inode)。
//! - [`sb`]：维护全局超级块、挂载点状态和块设备读写回调。
//!
//! 上层 syscall/VFS 只依赖 `superblock_*` helper 和 `Inode` trait，不直接接触
//! lwext4 的 C 风格接口。

mod inode;
mod sb;

/// lwext4 shares one mounted block cache and does not provide SMP-safe internal
/// locking. Keep every call into its path/file API serialized until the wrapper
/// gains per-superblock concurrency support.
///
/// Do not use `kspin::SpinNoIrq` here: its atomic lock field is compiled out
/// unless the dependency's `smp` feature is explicitly enabled. `spin::Mutex`
/// is configured in this kernel and remains a real atomic lock on every build.
pub(super) struct Ext4OpLock {
    inner: spin::Mutex<()>,
}

pub(super) struct Ext4OpGuard<'a> {
    _guard: spin::MutexGuard<'a, ()>,
    wait_ticks: usize,
    acquired_at: usize,
}

impl Ext4OpLock {
    pub const fn new() -> Self {
        Self {
            inner: spin::Mutex::new(()),
        }
    }

    pub fn lock(&self) -> Ext4OpGuard<'_> {
        let wait_start = crate::arch::time::get_ticks();
        let guard = self.inner.lock();
        let acquired_at = crate::arch::time::get_ticks();
        Ext4OpGuard {
            _guard: guard,
            wait_ticks: acquired_at.saturating_sub(wait_start),
            acquired_at,
        }
    }
}

impl Drop for Ext4OpGuard<'_> {
    fn drop(&mut self) {
        #[cfg(feature = "perf")]
        let released_at = crate::arch::time::get_ticks();
        #[cfg(feature = "perf")]
        crate::utils::perf::record_ext4_lock(
            self.wait_ticks,
            released_at.saturating_sub(self.acquired_at),
        );
    }
}

pub(super) static EXT4_OP_LOCK: Ext4OpLock = Ext4OpLock::new();

pub use inode::*;
pub use sb::{superblock_fs_stat, superblock_ls, superblock_root_inode, superblock_sync};
