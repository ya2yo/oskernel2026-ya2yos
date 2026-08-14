//! Linux-compatible timerfd file object backed by the shared monotonic timer wheel.

use alloc::{collections::BTreeMap, sync::{Arc, Weak}};
use core::{future::Future, pin::Pin, task::Context};
use spin::{Lazy, Mutex};

use crate::{
    fs::{vfs::File, Kstat},
    mm::UserBuffer,
    syscall::PollEvents,
    task::{block_on, poll_io, TimerFuture},
    timer::{get_time_spec, Timespec},
    utils::{SysErrNo, SysResult, SyscallRet},
};

#[derive(Clone, Copy, Default)]
pub struct TimerFdSpec {
    pub interval: Timespec,
    pub value: Timespec,
}

struct TimerFdState {
    next_expiry: Option<Timespec>,
    interval: Timespec,
    expirations: u64,
    timer: Option<TimerFuture>,
}

impl TimerFdState {
    fn new() -> Self {
        Self {
            next_expiry: None,
            interval: Timespec::default(),
            expirations: 0,
            timer: None,
        }
    }

    fn interval_is_zero(&self) -> bool {
        self.interval.tv_sec == 0 && self.interval.tv_nsec == 0
    }

    fn update_expirations(&mut self) {
        let Some(next) = self.next_expiry else {
            return;
        };
        let now = get_time_spec();
        if now < next {
            return;
        }

        self.timer = None;
        if self.interval_is_zero() {
            self.expirations = self.expirations.saturating_add(1);
            self.next_expiry = None;
            return;
        }

        let interval_ticks = self.interval.to_tick().max(1);
        let elapsed_ticks = now.to_tick().saturating_sub(next.to_tick());
        let elapsed = elapsed_ticks / interval_ticks + 1;
        self.expirations = self.expirations.saturating_add(elapsed as u64);
        let next_tick = next
            .to_tick()
            .saturating_add(elapsed.saturating_mul(interval_ticks));
        let freq = crate::arch::time::get_clock_freq() as u128;
        let nanos = (next_tick as u128)
            .saturating_mul(1_000_000_000u128)
            .checked_div(freq)
            .unwrap_or(u128::MAX)
            .min(u64::MAX as u128) as u64;
        self.next_expiry = Some(Timespec::from_nanos(nanos));
    }

    fn remaining(&mut self) -> TimerFdSpec {
        self.update_expirations();
        let value = self
            .next_expiry
            .map(|next| timespec_sub(next, get_time_spec()))
            .unwrap_or_default();
        TimerFdSpec {
            interval: self.interval,
            value,
        }
    }
}

/// A timerfd descriptor. Its retained [`TimerFuture`] makes the existing
/// timer wheel wake ppoll/epoll waiters at the programmed deadline.
pub struct TimerFd {
    non_blocking: bool,
    state: Mutex<TimerFdState>,
}

static TIMERFD_TABLE: Lazy<Mutex<BTreeMap<usize, Weak<TimerFd>>>> =
    Lazy::new(|| Mutex::new(BTreeMap::new()));

impl TimerFd {
    pub fn new(non_blocking: bool) -> Arc<Self> {
        Arc::new(Self {
            non_blocking,
            state: Mutex::new(TimerFdState::new()),
        })
    }

    pub fn register_fd(fd: usize, timerfd: &Arc<Self>) {
        TIMERFD_TABLE.lock().insert(fd, Arc::downgrade(timerfd));
    }

    pub fn lookup(fd: usize) -> Result<Arc<Self>, SysErrNo> {
        let mut table = TIMERFD_TABLE.lock();
        match table.get(&fd).and_then(Weak::upgrade) {
            Some(timerfd) => Ok(timerfd),
            None => {
                table.remove(&fd);
                Err(SysErrNo::EINVAL)
            }
        }
    }

    pub fn set_time(&self, absolute: bool, spec: TimerFdSpec) {
        let mut state = self.state.lock();
        state.timer = None;
        state.interval = spec.interval;
        state.expirations = 0;
        state.next_expiry = if spec.value.tv_sec == 0 && spec.value.tv_nsec == 0 {
            None
        } else if absolute {
            Some(spec.value)
        } else {
            Some(get_time_spec() + spec.value)
        };
    }

    pub fn current_spec(&self) -> TimerFdSpec {
        self.state.lock().remaining()
    }

    fn take_expirations(&self) -> Result<u64, SysErrNo> {
        let mut state = self.state.lock();
        state.update_expirations();
        if state.expirations == 0 {
            return Err(SysErrNo::EAGAIN);
        }
        Ok(core::mem::take(&mut state.expirations))
    }
}

impl File for TimerFd {
    fn readable(&self) -> bool {
        true
    }

    fn writable(&self) -> bool {
        false
    }

    fn read(&self, mut dst: UserBuffer) -> SyscallRet {
        if dst.len() < core::mem::size_of::<u64>() {
            return Err(SysErrNo::EINVAL);
        }
        let expirations = block_on(poll_io(self, PollEvents::IN, self.non_blocking, || {
            self.take_expirations()
        }))?;
        dst.write(&expirations.to_ne_bytes());
        Ok(core::mem::size_of::<u64>())
    }

    fn write(&self, _src: UserBuffer) -> SyscallRet {
        Err(SysErrNo::EINVAL)
    }

    fn fstat(&self) -> Kstat {
        Kstat::default()
    }

    fn poll(&self, events: PollEvents) -> PollEvents {
        let mut revents = PollEvents::empty();
        if events.contains(PollEvents::IN) {
            let mut state = self.state.lock();
            state.update_expirations();
            revents.set(PollEvents::IN, state.expirations != 0);
        }
        revents
    }

    fn register(&self, cx: &mut Context<'_>, events: PollEvents) {
        if !events.contains(PollEvents::IN) {
            return;
        }
        let mut state = self.state.lock();
        state.update_expirations();
        if state.expirations != 0 || state.next_expiry.is_none() {
            return;
        }
        if state.timer.is_none() {
            state.timer = Some(TimerFuture::new(state.next_expiry.unwrap()));
        }
        if let Some(timer) = state.timer.as_mut() {
            if Pin::new(timer).poll(cx).is_ready() {
                state.timer = None;
                state.update_expirations();
            }
        }
    }

    fn nonblocking(&self) -> bool {
        self.non_blocking
    }

    fn set_nonblocking(&self, _nonblocking: bool) -> SysResult {
        Ok(())
    }
}

fn timespec_sub(end: Timespec, start: Timespec) -> Timespec {
    if end <= start {
        return Timespec::default();
    }
    let mut sec = end.tv_sec - start.tv_sec;
    let nsec = if end.tv_nsec >= start.tv_nsec {
        end.tv_nsec - start.tv_nsec
    } else {
        sec -= 1;
        end.tv_nsec + 1_000_000_000 - start.tv_nsec
    };
    Timespec::new(sec, nsec)
}
