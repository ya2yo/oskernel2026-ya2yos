use alloc::{
    sync::{Arc, Weak},
    vec,
    vec::Vec,
};
use core::{
    future::poll_fn,
    slice,
    task::{Context, Poll},
    time::Duration,
};

use crate::{
    fs::File,
    mm::{copy_from_user, copy_to_user},
    signal::{SigOp, SigSet, SIGCHLD},
    syscall::{options::PollFd, PollEvents},
    task::{block_on, current_task, timeout as timeout_future},
    timer::Timespec,
    utils::{SysErrNo, SyscallRet},
};

struct PpollSigMaskGuard {
    // A ppoll can be interrupted by a fatal signal.  Exit abandons the
    // current kernel stack instead of unwinding it, so an owning Arc here
    // would survive forever when Drop is skipped.  The TCB is only needed to
    // restore the mask while the task is still alive.
    task: Weak<crate::task::TaskControlBlock>,
    old_mask: Option<SigSet>,
}

impl PpollSigMaskGuard {
    fn replace(task: Arc<crate::task::TaskControlBlock>, new_mask: Option<SigSet>) -> Self {
        let old_mask = new_mask.map(|new_mask| {
            let mut inner = task.inner_lock();
            let old_mask = inner.sig_mask;
            inner.sig_mask = new_mask;
            old_mask
        });
        Self {
            task: Arc::downgrade(&task),
            old_mask,
        }
    }
}

impl Drop for PpollSigMaskGuard {
    fn drop(&mut self) {
        if let Some(old_mask) = self.old_mask {
            if let Some(task) = self.task.upgrade() {
                task.inner_lock().sig_mask = old_mask;
            }
        }
    }
}

/// A stable fd snapshot for one ppoll invocation. Keeping the `Arc` avoids
/// repeatedly taking the process fd-table lock after every wakeup.
struct PollWatchEntry {
    interests: PollEvents,
    file: Option<Arc<dyn File>>,
}

fn collect_watch_entries(fds: &[PollFd]) -> Vec<PollWatchEntry> {
    let task = current_task().unwrap();
    let fd_table = &task.process.fd_table;

    fds.iter()
        .map(|pfd| PollWatchEntry {
            interests: pfd.events,
            file: (pfd.fd >= 0)
                .then(|| fd_table.try_get(pfd.fd as usize).map(|file| file.any()))
                .flatten(),
        })
        .collect()
}

/// Scan the fd snapshot once. Invalid descriptors are reported as POLLNVAL,
/// while negative descriptors are ignored as required by poll(2).
fn poll_ready(entries: &[PollWatchEntry], fds: &mut [PollFd]) -> usize {
    let mut ready = 0;

    for (pfd, entry) in fds.iter_mut().zip(entries) {
        pfd.revents = PollEvents::empty();
        if pfd.fd < 0 {
            continue;
        }

        pfd.revents = match entry.file.as_ref() {
            Some(file) => file.poll(entry.interests),
            None => PollEvents::INVAL,
        };
        if !pfd.revents.is_empty() {
            ready += 1;
        }
    }

    ready
}

fn register_watch_entries(entries: &[PollWatchEntry], cx: &mut Context<'_>) {
    for entry in entries {
        if let Some(file) = entry.file.as_ref() {
            file.register(cx, entry.interests);
        }
    }
}

/// Consume ignored signals and report a visible one to ppoll.
fn check_pending_signal() -> Result<(), SysErrNo> {
    loop {
        let task = current_task().unwrap();
        let pending_signo = {
            let inner = task.inner_lock();
            inner.sig_pending.difference(inner.sig_mask).peek_front()
        };
        let Some(signo) = pending_signo else {
            return Ok(());
        };

        let signal = SigSet::from_sig(signo);
        let sig_action = task
            .process
            .with_sigtable(|sigtable| sigtable.action(signo));
        let ignorable = signo == SIGCHLD
            || sig_action.is_ignored()
            || (!sig_action.is_handler() && signal.default_op() == SigOp::Ignore);
        if !ignorable {
            return Err(SysErrNo::EINTR);
        }

        task.inner_lock().sig_pending.remove(signal);
    }
}

/// 参考 https://man7.org/linux/man-pages/man2/ppoll.2.html
pub fn sys_ppoll(
    fds_ptr: usize,
    nfds: usize,
    tmo_p: usize,
    sigmask_ptr: usize,
    sigsetsize: usize,
) -> SyscallRet {
    let task = current_task().unwrap();
    let process = &task.process;
    let memory_set = process.memory_set_arc();

    if fds_ptr == 0 && nfds != 0 {
        return Err(SysErrNo::EINVAL);
    }

    let nfds = nfds.min(process.fd_table.get_soft_limit());
    let mut kernel_fds = vec![PollFd::new(); nfds];
    if nfds != 0 {
        let bytes = unsafe {
            slice::from_raw_parts_mut(
                kernel_fds.as_mut_ptr().cast::<u8>(),
                core::mem::size_of_val(kernel_fds.as_slice()),
            )
        };
        copy_from_user(&memory_set, fds_ptr, bytes)?;
    }

    let timeout = if tmo_p == 0 {
        None
    } else {
        let mut timespec = Timespec::new(0, 0);
        copy_from_user(&memory_set, tmo_p, unsafe {
            slice::from_raw_parts_mut(
                (&mut timespec as *mut Timespec).cast::<u8>(),
                core::mem::size_of::<Timespec>(),
            )
        })?;
        if timespec.tv_nsec >= 1_000_000_000 {
            return Err(SysErrNo::EINVAL);
        }
        Some(Duration::new(
            timespec.tv_sec as u64,
            timespec.tv_nsec as u32,
        ))
    };

    let new_mask = if sigmask_ptr == 0 {
        None
    } else {
        if sigsetsize != core::mem::size_of::<SigSet>() {
            return Err(SysErrNo::EINVAL);
        }
        let mut sigset = SigSet::default();
        copy_from_user(&memory_set, sigmask_ptr, unsafe {
            slice::from_raw_parts_mut(
                (&mut sigset as *mut SigSet).cast::<u8>(),
                core::mem::size_of::<SigSet>(),
            )
        })?;
        sigset.remove(SigSet::SIGKILL | SigSet::SIGSTOP);
        Some(sigset)
    };

    let entries = collect_watch_entries(&kernel_fds);
    let _sigmask_guard = PpollSigMaskGuard::replace(Arc::clone(&task), new_mask);
    drop(memory_set);
    drop(task);

    let ready = if timeout.map(|duration| duration.is_zero()).unwrap_or(false) {
        check_pending_signal()?;
        poll_ready(&entries, &mut kernel_fds)
    } else {
        let wait_for_ready = poll_fn(|cx| {
            let ready = poll_ready(&entries, &mut kernel_fds);
            if ready > 0 {
                return Poll::Ready(Ok(ready));
            }
            if let Err(errno) = check_pending_signal() {
                return Poll::Ready(Err(errno));
            }

            register_watch_entries(&entries, cx);
            let ready = poll_ready(&entries, &mut kernel_fds);
            if ready > 0 {
                Poll::Ready(Ok(ready))
            } else if let Err(errno) = check_pending_signal() {
                Poll::Ready(Err(errno))
            } else {
                Poll::Pending
            }
        });

        match block_on(timeout_future(timeout, wait_for_ready)) {
            Ok(result) => result?,
            Err(_) => 0,
        }
    };

    if nfds != 0 {
        let bytes = unsafe {
            slice::from_raw_parts(
                kernel_fds.as_ptr().cast::<u8>(),
                core::mem::size_of_val(kernel_fds.as_slice()),
            )
        };
        let memory_set = current_task().unwrap().process.memory_set_arc();
        copy_to_user(&memory_set, fds_ptr, bytes)?;
    }
    Ok(ready)
}
