//! Advisory file-locking facade.
//!
//! 文件锁入口按语义拆分到子模块中：
//! - `types`: 用户态 ABI 类型，如 `struct flock`。
//! - `posix`: `fcntl(F_GETLK/F_SETLK/F_SETLKW)` 字节范围记录锁。
//! - `lease`: `fcntl(F_SETLEASE/F_GETLEASE)` 文件租约。
//! - `bsd_flock`: `flock(2)` 整文件锁。
//!
//! 对外仍通过 `file_lock::...` 暴露，避免调用方关心内部拆分。

mod bsd_flock;
mod lease;
mod posix;
mod types;

pub use self::bsd_flock::{flock_register_waker, flock_try_lock, flock_unlock};
pub use self::lease::{
    get_file_lease, release_file_leases, release_file_leases_by_owner, set_file_lease,
};
pub use self::posix::{
    clear_wait, conflicting_owners, getlk, record_wait, register_posix_waker, release_posix_locks,
    release_posix_locks_by_owner, setlk, would_deadlock,
};
pub use self::types::Flock;
