use alloc::{sync::Arc, vec::Vec};
use log::debug;

use crate::{
    fs::File,
    mm::{translated_ref, translated_refmut},
    syscall::{options::PollFd, PollEvents},
    task::{current_task, suspend_current_and_run_next},
    timer::{get_time_ms, Timespec},
    utils::{SysErrNo, SyscallRet},
};

/// 参考 https://man7.org/linux/man-pages/man2/ppoll.2.html
pub fn sys_ppoll(fds_ptr: usize, nfds: usize, tmo_p: usize, mask: usize) -> SyscallRet {
    let task = current_task().unwrap();
    let inner = task.inner_lock();
    let token = task
        .process
        .inner_lock()
        .get_locked_memory_set_read()
        .token();

    // debug!(
    //     "[sys_ppoll] fds_ptr is {}, nfds is {}, tmo_p is {}, mask is {}",
    //     fds_ptr, nfds, tmo_p, mask
    // );

    if fds_ptr == 0 {
        return Err(SysErrNo::EINVAL);
    }

    let mut fds = Vec::new();
    let ptr = fds_ptr as *mut PollFd;
    for i in 0..nfds {
        fds.push(translated_refmut(token, unsafe { ptr.add(i) } as *mut PollFd));
    }

    let waittime = if tmo_p == 0 {
        //为0则永远等待直到完成
        -1
    } else {
        let timespec = translated_ref(token, tmo_p as *const Timespec);
        (timespec.tv_sec * 1000000000 + timespec.tv_nsec) as isize
    };
    if waittime == 0 {
        return Ok(0);
    }

    let begin = get_time_ms() * 1000000;

    //由于每次循环结束需要让出cpu，因此需要在每次循环时重新获得锁
    drop(inner);
    drop(task);

    loop {
        let task = current_task().unwrap();
        let inner = task.inner_lock();
        let mut resnum = 0;
        for i in 0..nfds {
            if fds[i].fd < 0 {
                fds[i].revents = PollEvents::empty();
                continue;
            }
            if let Some(file) = task.get_fd_table().try_get(fds[i].fd as usize) {
                let file: Arc<dyn File> = file.any();
                let res = file.poll(fds[i].events);
                if !res.is_empty() {
                    resnum += 1;
                }
                fds[i].revents = res;
            } else {
                fds[i].revents = PollEvents::INVAL;
            }
        }
        //有响应了就可以返回
        if resnum > 0 {
            return Ok(resnum);
        }
        //或者时间到了也可以返回
        if waittime > 0 && get_time_ms() * 1000000 - begin >= waittime as usize {
            return Ok(0);
        }
        drop(inner);
        drop(task);
        suspend_current_and_run_next();
    }
}
