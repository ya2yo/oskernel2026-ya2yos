use alloc::{sync::Arc, vec::Vec};
use log::debug;

use crate::{
    mm::put_data,
    task::{current_task, suspend_current_and_run_next, Process},
    utils::{SysErrNo, SyscallRet},
};

/// input.pid<-1: 等待一个子进程，其pgid==abs(input.pid)。这里的pgid指的是进程组id
/// input.pid=-1: 等待任一一个子进程的结束。
///     这里子进程指的是调用者task所处的进程（线程组）的子进程
///     （由本线程组的task调用sys_clone但选择不把新线程置于本线程组时创建的新线程组）。
/// input.pid=0 : 等待与调用者同进程组的任一子进程的结束
/// input.pid>0 : 等待pid==input.pid的子进程的结束。从内核的视角看，这里的pid指的是线程组id(tgid)，而不是线程id(tid)。
///
/// 参考 https://man7.org/linux/man-pages/man2/wait4.2.html
pub fn sys_wait4(mut pid: isize, wstatus: *mut i32, _options: i32) -> SyscallRet {
    if pid < -1 {
        // 需要进程组功能
        panic!(
            "[sys_wait4] We cannot handle input.pid<-1 (input.pid={})",
            pid
        );
    }
    // 由于我们假设所有进程均属于同一个进程组，我们视pid=0为pid=-1
    if pid == 0 {
        pid = -1;
    }
    // 现在只有两种情况：pid=-1表示等待任意子进程结束，pid>0表示等待特定子进程结束

    // 新实现
    loop {
        debug!("Wait4 loop begin");
        let task = current_task().unwrap();
        let mut process_meta = task.process.meta_lock();
        // 取子进程集合

        let children: Vec<Arc<Process>> = process_meta
            .children
            .clone()
            .iter()
            .filter_map(|x| x.upgrade())
            .collect();
        debug!("Wait4 len={}", children.len());
        if children.len() == 0 {
            return Err(SysErrNo::ECHILD);
        }
        // 如果是等待特定进程，但是自己根本没有这个子进程，则退出
        if pid != -1 && children.iter().all(|proc| proc.pid != pid as usize) {
            return Err(SysErrNo::ECHILD);
        }

        let pair = children
            .iter()
            .enumerate()
            .find(|(_, p)| {
                // ++++ temporarily access child PCB exclusively
                p.all_tasks_exited() && (pid == -1 || pid as usize == p.pid)
                // ++++ release child PCB
            })
            .map(|(idx, p)| (idx, Arc::clone(p)));
        drop(children);
        if let Some((idx, child)) = pair {
            let found_pid = child.pid.clone();
            let exit_code = child.inner_lock().get_locked_sigtable().exit_code();

            if wstatus as usize != 0x0 {
                debug!(
                    "[sys_wait4] wait pid {}: child {} exit with code {}, wstatus= {:#x}",
                    pid, found_pid, exit_code, wstatus as usize
                );
                let token = task
                    .process
                    .inner_lock()
                    .get_locked_memory_set_read()
                    .token();
                if exit_code >= 128 && exit_code <= 255 {
                    //表示由于信号而退出的
                    put_data(token, wstatus, exit_code);
                } else {
                    put_data(token, wstatus, exit_code << 8);
                }
            }
            // drop(child_inner);
            process_meta.children.remove(idx);
            // 从全局进程映射中移除
            // 在移除前，我们得先把手上的这个Arc给丢掉
            drop(child);
            Process::remove_from_global_map(found_pid);
            return Ok(found_pid);
        } else {
            drop(process_meta);
            drop(task);

            debug!("Wait4 suspend");
            suspend_current_and_run_next();
            debug!("Wait4 wakeup");
        }
    }
}
