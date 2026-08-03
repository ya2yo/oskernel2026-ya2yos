//! Signal 子系统门面模块。
//!
//! 这个目录按职责拆分 signal 相关逻辑，外部代码仍通过 `crate::signal::*`
//! 使用原有接口，避免 syscall、task、fs 等调用方感知内部文件布局。

pub mod action_table;
pub mod delivery;
pub mod frame;
pub mod pending;
pub mod timer;
pub mod types;

use linux_raw_sys::general::SIGEV_MAX_SIZE;

pub use action_table::*;
pub use delivery::*;
pub use frame::*;
pub use pending::*;
pub use timer::*;
pub use types::*;

pub const SIG_MAX_NUM: usize = SIGEV_MAX_SIZE as usize;
pub const SIG_DFL: usize = 0;
pub const SIG_IGN: usize = 1;
