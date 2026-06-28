use alloc::sync::Arc;

use crate::{
    fs::File,
    mm::{copy_from_user, copy_from_user_val, copy_to_user},
    signal::{SigOp, SigSet, SIGCHLD, SIG_IGN},
    syscall::{
        options::{FdSet, FD_SET_LEN},
        PollEvents,
    },
    task::{current_task, suspend_current_and_run_next},
    timer::{get_time_ms, Timespec},
    utils::{SysErrNo, SyscallRet},
};
use core::cmp::min;

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

    // pselect 不会更新 timeout 的值，而 select 会
    let waittime = if timeout == 0 {
        -1
    } else {
        let mut timespec = Timespec::new(0, 0);
        copy_from_user(&memory_set, timeout, unsafe {
            core::slice::from_raw_parts_mut(
                &mut timespec as *mut Timespec as *mut u8,
                core::mem::size_of::<Timespec>(),
            )
        })?;
        (timespec.tv_sec * 1_000_000_000 + timespec.tv_nsec) as isize
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

    let begin = get_time_ms() * 1_000_000;

    //由于每次循环结束需要让出cpu，因此需要在每次循环时重新获得锁
    drop(memory_set);
    drop(proc_inner);
    drop(task);

    loop {
        let task = current_task().unwrap();
        let proc_inner = task.process.inner_lock();
        let mut num = 0;
        let mut ready_readfds = using_readfds.as_ref().map(|_| empty_fdset());
        let mut ready_writefds = using_writefds.as_ref().map(|_| empty_fdset());
        let mut ready_exceptfds = using_exceptfds.as_ref().map(|_| empty_fdset());

        // 如果设置了监视是否可读的 fd
        if let Some(readfds) = using_readfds.as_ref() {
            for i in 0..nfds {
                if readfds.got_fd(i) {
                    if let Some(file) = proc_inner.fd_table.try_get(i) {
                        let file: Arc<dyn File> = file.any();
                        let event = file.poll(PollEvents::IN);
                        if event.contains(PollEvents::IN) {
                            ready_readfds.as_mut().unwrap().mark_fd(i, true);
                            num += 1;
                        }
                    }
                }
            }
        }
        // 如果设置了监视是否可写的 fd
        if let Some(writefds) = using_writefds.as_ref() {
            for i in 0..nfds {
                if writefds.got_fd(i) {
                    if let Some(file) = proc_inner.fd_table.try_get(i) {
                        let file: Arc<dyn File> = file.any();
                        let event = file.poll(PollEvents::OUT);
                        if event.contains(PollEvents::OUT) {
                            ready_writefds.as_mut().unwrap().mark_fd(i, true);
                            num += 1;
                        }
                    }
                }
            }
        }

        // 如果设置了监视异常的 fd
        if let Some(exceptfds) = using_exceptfds.as_ref() {
            for i in 0..nfds {
                if exceptfds.got_fd(i) {
                    if let Some(file) = proc_inner.fd_table.try_get(i) {
                        let file: Arc<dyn File> = file.any();
                        let event = file.poll(PollEvents::ERR | PollEvents::HUP);
                        if event.intersects(PollEvents::ERR | PollEvents::HUP) {
                            ready_exceptfds.as_mut().unwrap().mark_fd(i, true);
                            num += 1;
                        }
                    }
                }
            }
        }

        //如果有响应了则返回,或者如果时间是0，0（只监视一遍），也需要返回结果
        if num > 0 || waittime == 0 {
            // 重新获取 memory_set 以写回结果
            {
                let memory_set = proc_inner.get_locked_memory_set_read();
                if let Some(ready_readfds) = ready_readfds {
                    if let Err(errno) = copy_to_user(&memory_set, readfds, unsafe {
                        core::slice::from_raw_parts(
                            &ready_readfds as *const FdSet as *const u8,
                            core::mem::size_of::<FdSet>(),
                        )
                    }) {
                        if mask_changed {
                            task.inner_lock().sig_mask = old_mask;
                        }
                        return Err(errno);
                    }
                }
                if let Some(ready_writefds) = ready_writefds {
                    if let Err(errno) = copy_to_user(&memory_set, writefds, unsafe {
                        core::slice::from_raw_parts(
                            &ready_writefds as *const FdSet as *const u8,
                            core::mem::size_of::<FdSet>(),
                        )
                    }) {
                        if mask_changed {
                            task.inner_lock().sig_mask = old_mask;
                        }
                        return Err(errno);
                    }
                }
                if let Some(ready_exceptfds) = ready_exceptfds {
                    if let Err(errno) = copy_to_user(&memory_set, exceptfds, unsafe {
                        core::slice::from_raw_parts(
                            &ready_exceptfds as *const FdSet as *const u8,
                            core::mem::size_of::<FdSet>(),
                        )
                    }) {
                        if mask_changed {
                            task.inner_lock().sig_mask = old_mask;
                        }
                        return Err(errno);
                    }
                }
            }
            if mask_changed {
                task.inner_lock().sig_mask = old_mask;
            }
            return Ok(num);
        }

        //或者时间到了也可以返回
        if waittime > 0 && get_time_ms() * 1000000 - begin >= waittime as usize {
            {
                let memory_set = proc_inner.get_locked_memory_set_read();
                if let Some(ready_readfds) = ready_readfds {
                    if let Err(errno) = copy_to_user(&memory_set, readfds, unsafe {
                        core::slice::from_raw_parts(
                            &ready_readfds as *const FdSet as *const u8,
                            core::mem::size_of::<FdSet>(),
                        )
                    }) {
                        if mask_changed {
                            task.inner_lock().sig_mask = old_mask;
                        }
                        return Err(errno);
                    }
                }
                if let Some(ready_writefds) = ready_writefds {
                    if let Err(errno) = copy_to_user(&memory_set, writefds, unsafe {
                        core::slice::from_raw_parts(
                            &ready_writefds as *const FdSet as *const u8,
                            core::mem::size_of::<FdSet>(),
                        )
                    }) {
                        if mask_changed {
                            task.inner_lock().sig_mask = old_mask;
                        }
                        return Err(errno);
                    }
                }
                if let Some(ready_exceptfds) = ready_exceptfds {
                    if let Err(errno) = copy_to_user(&memory_set, exceptfds, unsafe {
                        core::slice::from_raw_parts(
                            &ready_exceptfds as *const FdSet as *const u8,
                            core::mem::size_of::<FdSet>(),
                        )
                    }) {
                        if mask_changed {
                            task.inner_lock().sig_mask = old_mask;
                        }
                        return Err(errno);
                    }
                }
            }
            if mask_changed {
                task.inner_lock().sig_mask = old_mask;
            }
            return Ok(0);
        }
        {
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
                    if mask_changed {
                        inner.sig_mask = old_mask;
                    }
                    return Err(SysErrNo::EINTR);
                }
            }
        }
        drop(proc_inner);
        drop(task);
        suspend_current_and_run_next();
    }
}
