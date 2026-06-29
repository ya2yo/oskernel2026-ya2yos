use alloc::{sync::Arc, vec::Vec};

use crate::{
    fs::File,
    mm::{copy_from_user, copy_from_user_val, copy_to_user},
    signal::{enter_pselect_itimer_wait, SigOp, SigSet, SIGCHLD, SIG_IGN},
    syscall::{
        options::{FdSet, FD_SET_LEN},
        PollEvents,
    },
    task::{block_on, current_task, timeout as timeout_future},
    timer::Timespec,
    utils::{SysErrNo, SyscallRet},
};
use core::{
    cmp::min,
    future::poll_fn,
    task::{Context, Poll},
    time::Duration,
};

#[repr(C)]
#[derive(Clone, Copy)]
struct Pselect6SigMask {
    ss: usize,
    ss_len: usize,
}

fn empty_fdset() -> FdSet {
    FdSet {
        fds_bits: [0; FD_SET_LEN],
    }
}

struct SelectResult {
    num: usize,
    readfds: Option<FdSet>,
    writefds: Option<FdSet>,
    exceptfds: Option<FdSet>,
}

impl SelectResult {
    fn empty(readfds: Option<&FdSet>, writefds: Option<&FdSet>, exceptfds: Option<&FdSet>) -> Self {
        Self {
            num: 0,
            readfds: readfds.map(|_| empty_fdset()),
            writefds: writefds.map(|_| empty_fdset()),
            exceptfds: exceptfds.map(|_| empty_fdset()),
        }
    }
}

/// 保存一次 select/pselect 需要监听的 fd 和事件。
///
/// 这里提前持有 `Arc<dyn File>`，后续 poll/register 时就不需要再持有
/// 进程 fd_table 锁；TCP/loopback 的 poll 路径可能同步 wake 当前任务，
/// 因此不能带着 task inner 或进程锁进入。
struct WatchEntry {
    fd: usize,
    interests: PollEvents,
    file: Arc<dyn File>,
}

fn fdset_contains(fdset: Option<&FdSet>, fd: usize) -> bool {
    fdset.map(|set| set.got_fd(fd)).unwrap_or(false)
}

fn collect_watch_entries(
    nfds: usize,
    readfds: Option<&FdSet>,
    writefds: Option<&FdSet>,
    exceptfds: Option<&FdSet>,
) -> Result<Vec<WatchEntry>, SysErrNo> {
    let task = current_task().unwrap();
    let proc_inner = task.process.inner_lock();
    let mut entries = Vec::new();

    for fd in 0..nfds {
        let wants_read = fdset_contains(readfds, fd);
        let wants_write = fdset_contains(writefds, fd);
        let wants_except = fdset_contains(exceptfds, fd);
        if !(wants_read || wants_write || wants_except) {
            continue;
        }

        let Some(file) = proc_inner.fd_table.try_get(fd) else {
            return Err(SysErrNo::EBADF);
        };
        let mut interests = PollEvents::empty();
        if wants_read {
            interests |= PollEvents::IN;
        }
        if wants_write {
            interests |= PollEvents::OUT;
        }
        if wants_except {
            interests |= PollEvents::ERR | PollEvents::HUP;
        }
        entries.push(WatchEntry {
            fd,
            interests,
            file: file.any(),
        });
    }

    Ok(entries)
}

/// 扫描当前 fd ready 状态并生成要回写给用户态的 fdset。
///
/// 该函数只做一次非阻塞 poll；真正睡眠由下方 poll_fn 在 Pending 前
/// 调用 `register_watch_entries()` 完成。
fn poll_ready(
    entries: &[WatchEntry],
    readfds: Option<&FdSet>,
    writefds: Option<&FdSet>,
    exceptfds: Option<&FdSet>,
) -> SelectResult {
    let mut result = SelectResult::empty(readfds, writefds, exceptfds);

    for entry in entries {
        let events = entry.file.poll(entry.interests);
        if fdset_contains(readfds, entry.fd) && events.contains(PollEvents::IN) {
            result.readfds.as_mut().unwrap().mark_fd(entry.fd, true);
            result.num += 1;
        }
        if fdset_contains(writefds, entry.fd) && events.contains(PollEvents::OUT) {
            result.writefds.as_mut().unwrap().mark_fd(entry.fd, true);
            result.num += 1;
        }
        if fdset_contains(exceptfds, entry.fd) && events.intersects(PollEvents::ERR | PollEvents::HUP)
        {
            result.exceptfds.as_mut().unwrap().mark_fd(entry.fd, true);
            result.num += 1;
        }
    }

    result
}

/// 将当前 pselect future 的 waker 注册到每个被监听 fd。
///
/// 注册后会立即二次 poll，避免事件刚好发生在第一次 poll 和注册之间
/// 导致丢失唤醒。
fn register_watch_entries(entries: &[WatchEntry], cx: &mut Context<'_>) {
    for entry in entries {
        entry.file.register(cx, entry.interests);
    }
}

fn write_result_to_user(
    result: &SelectResult,
    readfds: usize,
    writefds: usize,
    exceptfds: usize,
) -> SyscallRet {
    let task = current_task().unwrap();
    let proc_inner = task.process.inner_lock();
    let memory_set = proc_inner.get_locked_memory_set_read();

    if let Some(ready_readfds) = result.readfds.as_ref() {
        copy_to_user(&memory_set, readfds, unsafe {
            core::slice::from_raw_parts(
                ready_readfds as *const FdSet as *const u8,
                core::mem::size_of::<FdSet>(),
            )
        })?;
    }
    if let Some(ready_writefds) = result.writefds.as_ref() {
        copy_to_user(&memory_set, writefds, unsafe {
            core::slice::from_raw_parts(
                ready_writefds as *const FdSet as *const u8,
                core::mem::size_of::<FdSet>(),
            )
        })?;
    }
    if let Some(ready_exceptfds) = result.exceptfds.as_ref() {
        copy_to_user(&memory_set, exceptfds, unsafe {
            core::slice::from_raw_parts(
                ready_exceptfds as *const FdSet as *const u8,
                core::mem::size_of::<FdSet>(),
            )
        })?;
    }

    Ok(result.num)
}

/// 参考 https://man7.org/linux/man-pages/man2/pselect6.2.html
pub fn sys_pselect6(
    nfds: usize,
    readfds: usize,
    writefds: usize,
    exceptfds: usize,
    timeout: usize,
    sigmask: usize,
) -> SyscallRet {
    let task = current_task().unwrap();
    let proc_inner = task.process.inner_lock();
    let memory_set = proc_inner.get_locked_memory_set_read();

    let new_mask = if sigmask != 0 {
        // Linux raw pselect6 passes a pointer to { sigset_t *ss, size_t ss_len },
        // not a direct sigset_t pointer.
        let arg: Pselect6SigMask =
            copy_from_user_val(&memory_set, sigmask as *const Pselect6SigMask)?;
        if arg.ss != 0 {
            if arg.ss_len != core::mem::size_of::<SigSet>() {
                return Err(SysErrNo::EINVAL);
            }
            let mut sigset = SigSet::default();
            copy_from_user(&memory_set, arg.ss, unsafe {
                core::slice::from_raw_parts_mut(
                    &mut sigset as *mut SigSet as *mut u8,
                    core::mem::size_of::<SigSet>(),
                )
            })?;
            sigset.remove(SigSet::SIGKILL | SigSet::SIGSTOP);
            Some(sigset)
        } else {
            None
        }
    } else {
        None
    };

    let nfds = min(nfds, proc_inner.fd_table.get_soft_limit());

    let using_readfds = if readfds != 0 {
        let mut fdset = empty_fdset();
        copy_from_user(&memory_set, readfds, unsafe {
            core::slice::from_raw_parts_mut(
                &mut fdset as *mut FdSet as *mut u8,
                core::mem::size_of::<FdSet>(),
            )
        })?;
        Some(fdset)
    } else {
        None
    };
    let using_writefds = if writefds != 0 {
        let mut fdset = empty_fdset();
        copy_from_user(&memory_set, writefds, unsafe {
            core::slice::from_raw_parts_mut(
                &mut fdset as *mut FdSet as *mut u8,
                core::mem::size_of::<FdSet>(),
            )
        })?;
        Some(fdset)
    } else {
        None
    };
    let using_exceptfds = if exceptfds != 0 {
        let mut fdset = empty_fdset();
        copy_from_user(&memory_set, exceptfds, unsafe {
            core::slice::from_raw_parts_mut(
                &mut fdset as *mut FdSet as *mut u8,
                core::mem::size_of::<FdSet>(),
            )
        })?;
        Some(fdset)
    } else {
        None
    };

    let wait_duration = if timeout == 0 {
        None
    } else {
        let mut timespec = Timespec::new(0, 0);
        copy_from_user(&memory_set, timeout, unsafe {
            core::slice::from_raw_parts_mut(
                &mut timespec as *mut Timespec as *mut u8,
                core::mem::size_of::<Timespec>(),
            )
        })?;
        if timespec.tv_nsec >= 1_000_000_000 {
            return Err(SysErrNo::EINVAL);
        }
        Some(Duration::new(timespec.tv_sec as u64, timespec.tv_nsec as u32))
    };

    let old_mask = {
        let mut inner = task.inner_lock();
        let old_mask = inner.sig_mask;
        if let Some(sigset) = new_mask {
            inner.sig_mask = sigset;
        }
        old_mask
    };
    let mask_changed = new_mask.is_some();

    drop(memory_set);
    drop(proc_inner);
    drop(task);

    // pselect6 is a hot path for netperf/lmbench. Collect and validate the
    // watched files once, then reuse the Arc-backed entries across wakeups
    // instead of locking the fd table and allocating a new Vec on every poll.
    let entries = match collect_watch_entries(
        nfds,
        using_readfds.as_ref(),
        using_writefds.as_ref(),
        using_exceptfds.as_ref(),
    ) {
        Ok(entries) => entries,
        Err(errno) => {
            if mask_changed {
                current_task().unwrap().inner_lock().sig_mask = old_mask;
            }
            return Err(errno);
        }
    };

    let may_block = !wait_duration.map(|d| d.is_zero()).unwrap_or(false);
    let _pselect_itimer_guard =
        may_block.then(|| enter_pselect_itimer_wait(&current_task().unwrap()));

    let select_once = || -> SelectResult {
        poll_ready(
            &entries,
            using_readfds.as_ref(),
            using_writefds.as_ref(),
            using_exceptfds.as_ref(),
        )
    };

    let select_result = if wait_duration.map(|d| d.is_zero()).unwrap_or(false) {
        Ok(select_once())
    } else {
        let select_future = poll_fn(|cx| {
            let ready = poll_ready(
                &entries,
                using_readfds.as_ref(),
                using_writefds.as_ref(),
                using_exceptfds.as_ref(),
            );
            if ready.num > 0 {
                return Poll::Ready(Ok(ready));
            }

            {
                // 这里沿用原 pselect6 语义：默认忽略或显式忽略的信号被消费
                // 后继续等待；其它可见信号让 syscall 返回 EINTR。不能直接
                // 使用 task::interruptible()，因为它只响应 task.interrupt()
                // waker，不会替我们区分这些 pending signal 语义。
                let task = current_task().unwrap();
                let proc_inner = task.process.inner_lock();
                let mut inner = task.inner_lock();
                if let Some(signo) = inner.sig_pending.difference(inner.sig_mask).peek_front() {
                    let signal = SigSet::from_sig(signo);
                    let sig_action = proc_inner.get_locked_sigtable().action(signo);
                    let ignorable = signo == SIGCHLD
                        || sig_action.act.sa_handler == SIG_IGN
                        || (!sig_action.customed && signal.default_op() == SigOp::Ignore);
                    if ignorable {
                        inner.sig_pending.remove(signal);
                    } else {
                        drop(inner);
                        drop(proc_inner);
                        drop(task);
                        let ready = poll_ready(
                            &entries,
                            using_readfds.as_ref(),
                            using_writefds.as_ref(),
                            using_exceptfds.as_ref(),
                        );
                        if ready.num > 0 {
                            return Poll::Ready(Ok(ready));
                        }
                        return Poll::Ready(Err(SysErrNo::EINTR));
                    }
                }
            }

            register_watch_entries(&entries, cx);

            // Avoid missing an event that arrives between the first poll and
            // waker registration.
            let ready = poll_ready(
                &entries,
                using_readfds.as_ref(),
                using_writefds.as_ref(),
                using_exceptfds.as_ref(),
            );
            if ready.num > 0 {
                Poll::Ready(Ok(ready))
            } else {
                Poll::Pending
            }
        });

        if let Some(duration) = wait_duration {
            match block_on(timeout_future(Some(duration), select_future)) {
                Ok(result) => result,
                Err(_) => Ok(SelectResult::empty(
                    using_readfds.as_ref(),
                    using_writefds.as_ref(),
                    using_exceptfds.as_ref(),
                )),
            }
        } else {
            block_on(select_future)
        }
    };

    if mask_changed {
        current_task().unwrap().inner_lock().sig_mask = old_mask;
    }
    let select_result = select_result?;
    write_result_to_user(&select_result, readfds, writefds, exceptfds)
}
