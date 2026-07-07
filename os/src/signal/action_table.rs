//! 进程级 signal action 表。
//!
//! `SigTable` 只保存每个信号当前安装的 handler/action。线程组退出状态、
//! stop/continue 状态和等待事件属于 `ProcessMeta`，不要放入这个共享表。

use core::array::from_fn;

use crate::sync::SyncUnsafeCell;

use super::{KSigAction, SIG_MAX_NUM};

pub struct SigTable {
    pub inner: SyncUnsafeCell<SigTableInner>,
}

impl SigTable {
    pub fn new() -> Self {
        Self {
            inner: SyncUnsafeCell::new(SigTableInner::new()),
        }
    }
    pub fn from_another(another: &SigTable) -> Self {
        Self {
            inner: SyncUnsafeCell::new(SigTableInner::from_another(another.get_ref())),
        }
    }
    pub fn get_ref(&self) -> &SigTableInner {
        self.inner.get_unchecked_ref()
    }
    pub fn get_mut(&self) -> &mut SigTableInner {
        self.inner.get_unchecked_mut()
    }

    pub fn action(&self, signo: usize) -> KSigAction {
        self.get_ref().actions[signo]
    }
    pub fn set_action(&self, signo: usize, act: KSigAction) {
        self.get_mut().actions[signo] = act
    }
}

pub struct SigTableInner {
    actions: [KSigAction; SIG_MAX_NUM + 1],
}

impl SigTableInner {
    pub fn new() -> Self {
        Self {
            actions: from_fn(|signo| KSigAction::new(signo, false)),
        }
    }
    pub fn from_another(other: &SigTableInner) -> Self {
        Self {
            actions: other.actions.clone(),
        }
    }
}
