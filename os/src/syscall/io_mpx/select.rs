use alloc::sync::Arc;

use crate::{
    fs::File,
    mm::{get_data, put_data},
    signal::SigSet,
    syscall::{options::FdSet, PollEvents},
    task::{current_task, suspend_current_and_run_next},
    timer::{get_time_ms, Timespec},
    utils::SyscallRet,
};
use core::cmp::min;

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
    let token = proc_inner.get_locked_memory_set_read().token();

    // debug!("[sys_pselect6] nfds is {}, readfds is {}, writefds is {}, exceptfds is {}, timeout is {}, sigmask is {}",nfds,readfds,writefds,exceptfds,timeout,sigmask);

    let old_mask = inner.sig_mask;
    if sigmask != 0 {
        inner.sig_mask = get_data(token, sigmask as *const SigSet);
    }

    let nfds = min(nfds, proc_inner.fd_table.get_soft_limit());

    let mut using_readfds = if readfds != 0 {
        Some(get_data(token, readfds as *mut FdSet))
    } else {
        None
    };
    let mut using_writefds = if writefds != 0 {
        Some(get_data(token, writefds as *mut FdSet))
    } else {
        None
    };
    let mut using_exceptfds = if exceptfds != 0 {
        Some(get_data(token, exceptfds as *mut FdSet))
    } else {
        None
    };

    // pselect 不会更新 timeout 的值，而 select 会
    let waittime = if timeout == 0 {
        //为0则永远等待直到完成
        -1
    } else {
        // let timespec = translated_ref(token, timeout as *const Timespec);
        let timespec = get_data(token, timeout as *const Timespec);
        // debug!(
        //     "[sys_pselect6] waittime is {} sec, {} nsec",
        //     timespec.tv_sec, timespec.tv_nsec
        // );

        (timespec.tv_sec * 1_000_000_000 + timespec.tv_nsec) as isize
    };

    let begin = get_time_ms() * 1_000_000;

    //由于每次循环结束需要让出cpu，因此需要在每次循环时重新获得锁
    drop(proc_inner);
    drop(inner);
    drop(task);

    loop {
        let task = current_task().unwrap();
        let mut inner = task.inner_lock();
        let mut num = 0;

        // 如果设置了监视是否可读的 fd
        if let Some(readfds) = using_readfds.as_mut() {
            for i in 0..nfds {
                if readfds.got_fd(i) {
                    if let Some(file) = task.get_fd_table().try_get(i) {
                        let file: Arc<dyn File> = file.any();
                        let event = file.poll(PollEvents::IN);
                        if !event.contains(PollEvents::IN) {
                            readfds.mark_fd(i, false);
                        }
                        num += 1;
                    } else {
                        readfds.mark_fd(i, false);
                    }
                }
            }
        }
        // 如果设置了监视是否可写的 fd
        if let Some(writefds) = using_writefds.as_mut() {
            for i in 0..nfds {
                if writefds.got_fd(i) {
                    if let Some(file) = task.get_fd_table().try_get(i) {
                        let file: Arc<dyn File> = file.any();
                        let event = file.poll(PollEvents::OUT);
                        if !event.contains(PollEvents::OUT) {
                            writefds.mark_fd(i, false);
                        }
                        num += 1;
                    } else {
                        writefds.mark_fd(i, false);
                    }
                }
            }
        }

        // 如果设置了监视异常的 fd
        if let Some(exceptfds) = using_exceptfds.as_mut() {
            for i in 0..nfds {
                if exceptfds.got_fd(i) {
                    if let Some(file) = task.get_fd_table().try_get(i) {
                        let file: Arc<dyn File> = file.any();
                        let event = file.poll(PollEvents::ERR | PollEvents::HUP);
                        if !event.contains(PollEvents::ERR) && !event.contains(PollEvents::HUP) {
                            exceptfds.mark_fd(i, false);
                        }
                        num += 1;
                    } else {
                        exceptfds.mark_fd(i, false);
                    }
                }
            }
        }

        //如果有响应了则返回,或者如果时间是0，0（只监视一遍），也需要返回结果
        if num > 0 || waittime == 0 {
            // debug!("[sys_pselect6] ret for num:{},waittime:{}", num, waittime);
            if let Some(using_readfds) = using_readfds {
                put_data(token, readfds as *mut FdSet, using_readfds);
            }
            if let Some(using_writefds) = using_writefds {
                put_data(token, writefds as *mut FdSet, using_writefds);
            }
            if let Some(using_exceptfds) = using_exceptfds {
                // debug!("[sys_pselect6] exceptfds is {:?}", using_exceptfds);
                put_data(token, exceptfds as *mut FdSet, using_exceptfds);
            }
            if sigmask != 0 {
                inner.sig_mask = old_mask;
            }
            return Ok(num);
        }

        //或者时间到了也可以返回
        if waittime > 0 && get_time_ms() * 1000000 - begin >= waittime as usize {
            // debug!("[sys_pselect6] ret for timeout");
            if sigmask != 0 {
                inner.sig_mask = old_mask;
            }
            return Ok(0);
        }
        drop(inner);
        drop(task);
        suspend_current_and_run_next();
    }
}
