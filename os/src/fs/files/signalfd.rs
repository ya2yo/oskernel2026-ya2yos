use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::{
    fs::{vfs::File, Kstat},
    mm::UserBuffer,
    signal::{SigInfo, SigSet},
    syscall::PollEvents,
    task::{block_on, TaskControlBlock, WeakTaskRef},
    utils::{PollSet, SysErrNo, SysResult, SyscallRet},
};

/// The per-task signal queue exposed by signalfd(2).
pub struct SignalFd {
    task: WeakTaskRef,
    mask: spin::Mutex<SigSet>,
    non_blocking: AtomicBool,
    poll_rx: PollSet,
}

impl SignalFd {
    pub fn new(task: Arc<TaskControlBlock>, mask: SigSet) -> Arc<Self> {
        Arc::new(Self {
            task: Arc::downgrade(&task),
            mask: spin::Mutex::new(mask),
            non_blocking: AtomicBool::new(false),
            poll_rx: PollSet::new(),
        })
    }

    pub fn set_mask(&self, mask: SigSet) {
        *self.mask.lock() = mask;
        self.poll_rx.wake();
    }

    fn pending(&self) -> SigSet {
        let Some(task) = self.task.upgrade() else {
            return SigSet::empty();
        };
        let inner = task.inner_lock();
        inner.sig_pending & *self.mask.lock()
    }

    fn try_read(&self, dst: &mut UserBuffer) -> SyscallRet {
        const SIGINFO_SIZE: usize = core::mem::size_of::<SigInfo>();
        if dst.len() < SIGINFO_SIZE {
            return Err(SysErrNo::EINVAL);
        }
        let signal = self.pending().peek_front().ok_or(SysErrNo::EAGAIN)?;
        let info = {
            let task = self.task.upgrade().ok_or(SysErrNo::ESRCH)?;
            let mut inner = task.inner_lock();
            let signal_set = SigSet::from_sig(signal);
            if !inner.sig_pending.contains(signal_set) {
                return Err(SysErrNo::EAGAIN);
            }
            inner.sig_pending.remove(signal_set);
            inner.sig_pending_info[signal]
                .take()
                .unwrap_or_else(|| SigInfo::new(signal as u32, 0, 0, 0))
        };
        dst.write(unsafe {
            core::slice::from_raw_parts(&info as *const SigInfo as *const u8, SIGINFO_SIZE)
        });
        Ok(SIGINFO_SIZE)
    }
}

impl File for SignalFd {
    fn update_signal_mask(&self, mask: SigSet) -> bool {
        self.set_mask(mask);
        true
    }
    fn readable(&self) -> bool {
        true
    }
    fn writable(&self) -> bool {
        false
    }

    fn read(&self, mut dst: UserBuffer) -> SyscallRet {
        let nonblocking = self.nonblocking();
        block_on(core::future::poll_fn(|cx| match self.try_read(&mut dst) {
            Ok(value) => core::task::Poll::Ready(Ok(value)),
            Err(SysErrNo::EAGAIN) if !nonblocking => {
                self.register(cx, PollEvents::IN);
                match self.try_read(&mut dst) {
                    Ok(value) => core::task::Poll::Ready(Ok(value)),
                    Err(SysErrNo::EAGAIN) => core::task::Poll::Pending,
                    Err(err) => core::task::Poll::Ready(Err(err)),
                }
            }
            Err(err) => core::task::Poll::Ready(Err(err)),
        }))
    }

    fn write(&self, _buf: UserBuffer) -> SyscallRet {
        Err(SysErrNo::EINVAL)
    }
    fn fstat(&self) -> Kstat {
        Kstat::default()
    }
    fn nonblocking(&self) -> bool {
        self.non_blocking.load(Ordering::Acquire)
    }
    fn set_nonblocking(&self, value: bool) -> SysResult {
        self.non_blocking.store(value, Ordering::Release);
        Ok(())
    }
    fn poll(&self, _events: PollEvents) -> PollEvents {
        let mut events = PollEvents::empty();
        events.set(PollEvents::IN, !self.pending().is_empty());
        events
    }
    fn register(&self, cx: &mut core::task::Context<'_>, events: PollEvents) {
        if events.contains(PollEvents::IN) {
            self.poll_rx.register(cx.waker());
            if let Some(task) = self.task.upgrade() {
                task.interrupt_waker.register(cx.waker());
            }
        }
    }
}
