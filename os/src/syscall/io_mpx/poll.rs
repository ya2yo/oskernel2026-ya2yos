use crate::{
    fs::File,
    mm::{copy_from_user, copy_to_user},
    signal::{SigOp, SigSet, SIGCHLD, SIG_IGN},
    syscall::{options::PollFd, PollEvents},
    task::{current_task, suspend_current_and_run_next},
    timer::{get_time_ms, Timespec},
    utils::{SysErrNo, SyscallRet},
};
use alloc::vec;
use alloc::{sync::Arc, vec::Vec};
use core::cmp::min;

/// 参考 https://man7.org/linux/man-pages/man2/ppoll.2.html
pub fn sys_ppoll(fds_ptr: usize, nfds: usize, tmo_p: usize, _mask: usize) -> SyscallRet {
    let task = current_task().unwrap();
    let proc_inner = &task.process;
    let memory_set = proc_inner.memory_set_arc();

    if fds_ptr == 0 && nfds != 0 {
        return Err(SysErrNo::EINVAL);
    }

    // 限制 nfds 不超过进程的 fd 软限制，防止恶意或异常的 nfds 值
    // 导致巨量内存分配（参考 sys_pselect6 的做法）
    let nfds = min(nfds, proc_inner.fd_table.get_soft_limit());

    let user_fds_ptr = fds_ptr;
    let pollfd_size = core::mem::size_of::<PollFd>();
    let total_size = nfds * pollfd_size;
    let mut kernel_fds = vec![0u8; total_size];
    if nfds != 0 {
        copy_from_user(&memory_set, user_fds_ptr, &mut kernel_fds)?;
    }

    let fds_ptr = kernel_fds.as_mut_ptr() as *mut PollFd;

    let waittime = if tmo_p == 0 {
        //为0则永远等待直到完成
        -1
    } else {
        let mut timespec = Timespec::new(0, 0);
        copy_from_user(&memory_set, tmo_p, unsafe {
            core::slice::from_raw_parts_mut(
                &mut timespec as *mut Timespec as *mut u8,
                core::mem::size_of::<Timespec>(),
            )
        })?;
        (timespec.tv_sec * 1000000000 + timespec.tv_nsec) as isize
    };
    if waittime == 0 {
        return Ok(0);
    }

    let begin = get_time_ms() * 1000000;

    //由于每次循环结束需要让出cpu，因此需要在每次循环时重新获得锁
    drop(memory_set);

    loop {
        let task = current_task().unwrap();
        let proc_inner = &task.process;
        let mut resnum = 0;
        for i in 0..nfds {
            let pfd = unsafe { &mut *fds_ptr.add(i) };
            if pfd.fd < 0 {
                pfd.revents = PollEvents::empty();
                continue;
            }
            if let Some(file) = proc_inner.fd_table.try_get(pfd.fd as usize) {
                let file: Arc<dyn File> = file.any();
                let res = file.poll(pfd.events);
                if !res.is_empty() {
                    resnum += 1;
                }
                pfd.revents = res;
            } else {
                pfd.revents = PollEvents::INVAL;
            }
        }
        //有响应了就可以返回
        if resnum > 0 {
            let mem_set = proc_inner.memory_set_arc();
            copy_to_user(&mem_set, user_fds_ptr, &kernel_fds)?;
            return Ok(resnum);
        }
        //或者时间到了也可以返回
        if waittime > 0 && get_time_ms() * 1000000 - begin >= waittime as usize {
            return Ok(0);
        }
        {
            let mut task_inner = task.inner_lock();
            if let Some(signo) = task_inner
                .sig_pending
                .difference(task_inner.sig_mask)
                .peek_front()
            {
                let signal = SigSet::from_sig(signo);
                let sig_action = proc_inner.with_sigtable(|sigtable| sigtable.action(signo));
                let ignorable = signo == SIGCHLD
                    || sig_action.act.sa_handler == SIG_IGN
                    || (!sig_action.customed && signal.default_op() == SigOp::Ignore);
                if ignorable {
                    task_inner.sig_pending.remove(signal);
                } else {
                    return Err(SysErrNo::EINTR);
                }
            }
        }
        drop(task);
        suspend_current_and_run_next();
    }
}
