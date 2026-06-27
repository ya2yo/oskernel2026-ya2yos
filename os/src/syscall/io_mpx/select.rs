use alloc::sync::Arc;

use crate::{
    fs::File,
    mm::{copy_from_user, copy_to_user},
    signal::SigSet,
    syscall::{
        options::{FdSet, FD_SET_LEN},
        PollEvents,
    },
    task::{current_task, suspend_current_and_run_next},
    timer::{get_time_ms, Timespec},
    utils::SyscallRet,
};
use core::cmp::min;

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
    let mut inner = task.inner_lock();
    let proc_inner = task.process.inner_lock();
    let memory_set = proc_inner.get_locked_memory_set_read();

    let old_mask = inner.sig_mask;
    if sigmask != 0 {
        let mut sigset = SigSet::default();
        copy_from_user(&memory_set, sigmask, unsafe {
            core::slice::from_raw_parts_mut(
                &mut sigset as *mut SigSet as *mut u8,
                core::mem::size_of::<SigSet>(),
            )
        })?;
        inner.sig_mask = sigset;
    }

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
    let mut using_exceptfds = if exceptfds != 0 {
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

    let begin = get_time_ms() * 1_000_000;

    //由于每次循环结束需要让出cpu，因此需要在每次循环时重新获得锁
    drop(memory_set);
    drop(proc_inner);
    drop(inner);
    drop(task);

    loop {
        let task = current_task().unwrap();
        let proc_inner = task.process.inner_lock();
        let mut inner = task.inner_lock();
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
                    copy_to_user(&memory_set, readfds, unsafe {
                        core::slice::from_raw_parts(
                            &ready_readfds as *const FdSet as *const u8,
                            core::mem::size_of::<FdSet>(),
                        )
                    })?;
                }
                if let Some(ready_writefds) = ready_writefds {
                    copy_to_user(&memory_set, writefds, unsafe {
                        core::slice::from_raw_parts(
                            &ready_writefds as *const FdSet as *const u8,
                            core::mem::size_of::<FdSet>(),
                        )
                    })?;
                }
                if let Some(ready_exceptfds) = ready_exceptfds {
                    copy_to_user(&memory_set, exceptfds, unsafe {
                        core::slice::from_raw_parts(
                            &ready_exceptfds as *const FdSet as *const u8,
                            core::mem::size_of::<FdSet>(),
                        )
                    })?;
                }
            }
            if sigmask != 0 {
                inner.sig_mask = old_mask;
            }
            return Ok(num);
        }

        //或者时间到了也可以返回
        if waittime > 0 && get_time_ms() * 1000000 - begin >= waittime as usize {
            if sigmask != 0 {
                inner.sig_mask = old_mask;
            }
            return Ok(0);
        }
        drop(inner);
        drop(proc_inner);
        drop(task);
        suspend_current_and_run_next();
    }
}
