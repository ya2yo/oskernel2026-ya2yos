use alloc::sync::Arc;

use crate::{
    task::{current_task, Process},
    utils::{SysErrNo, SyscallRet},
};

/// kcmp 比较类型，参考 https://www.man7.org/linux/man-pages/man2/kcmp.2.html
const KCMP_FILE: i32 = 0;
const KCMP_VM: i32 = 1;
const KCMP_FILES: i32 = 2;
const KCMP_FS: i32 = 3;
const KCMP_SIGHAND: i32 = 4;
const KCMP_IO: i32 = 5;
const KCMP_SYSVSEM: i32 = 6;
const KCMP_EPOLL_TFD: i32 = 7;

/// 参考 https://www.man7.org/linux/man-pages/man2/kcmp.2.html
///
/// 比较两个进程是否共享内核资源。
/// 返回值: 0=共享, 1=不共享, 2=进程不存在, 3=fd 不存在
pub fn sys_kcmp(pid1: usize, pid2: usize, typ: i32, idx1: usize, idx2: usize) -> SyscallRet {
    // 提前校验 type
    if typ < KCMP_FILE || typ > KCMP_EPOLL_TFD {
        return Err(SysErrNo::EINVAL);
    }

    // pid==0 表示当前进程
    let curr_task = current_task().unwrap();
    let curr_pid = curr_task.pid();
    let pid1 = if pid1 == 0 { curr_pid } else { pid1 };
    let pid2 = if pid2 == 0 { curr_pid } else { pid2 };

    // 查找进程
    let proc1 = Process::get_process_arc_by_pid(pid1);
    let proc2 = Process::get_process_arc_by_pid(pid2);

    if proc1.is_none() || proc2.is_none() {
        return Ok(2); // 进程不存在
    }

    let proc1 = proc1.unwrap();
    let proc2 = proc2.unwrap();

    // 同一进程必然共享所有资源
    if pid1 == pid2 {
        if typ == KCMP_FILE {
            let fd_exists = proc1.inner_lock().fd_table.try_get(idx1).is_some();
            if !fd_exists {
                return Ok(3);
            }
        }
        return Ok(0);
    }

    match typ {
        KCMP_VM => {
            let mem1 = Arc::clone(&proc1.inner_lock().memory_set);
            let mem2 = Arc::clone(&proc2.inner_lock().memory_set);
            if Arc::ptr_eq(&mem1, &mem2) {
                Ok(0)
            } else {
                Ok(1)
            }
        }
        KCMP_FILES => {
            let fd1 = Arc::clone(&proc1.inner_lock().fd_table);
            let fd2 = Arc::clone(&proc2.inner_lock().fd_table);
            if Arc::ptr_eq(&fd1, &fd2) {
                Ok(0)
            } else {
                Ok(1)
            }
        }
        KCMP_FS => {
            let fs1 = Arc::clone(&proc1.inner_lock().fs_info);
            let fs2 = Arc::clone(&proc2.inner_lock().fs_info);
            if Arc::ptr_eq(&fs1, &fs2) {
                Ok(0)
            } else {
                Ok(1)
            }
        }
        KCMP_SIGHAND => {
            let sig1 = Arc::clone(&proc1.inner_lock().sig_table);
            let sig2 = Arc::clone(&proc2.inner_lock().sig_table);
            if Arc::ptr_eq(&sig1, &sig2) {
                Ok(0)
            } else {
                Ok(1)
            }
        }
        KCMP_FILE => {
            // 分别获取 fd，避免同时持有两把锁
            let fd1 = proc1.inner_lock().fd_table.try_get(idx1);
            let fd2 = proc2.inner_lock().fd_table.try_get(idx2);

            match (fd1, fd2) {
                (None, _) | (_, None) => Ok(3),
                (Some(file_desc1), Some(file_desc2)) => {
                    let f1 = file_desc1.any();
                    let f2 = file_desc2.any();
                    // Arc::ptr_eq 比较底层分配是否相同
                    if Arc::ptr_eq(&f1, &f2) {
                        Ok(0)
                    } else {
                        Ok(1)
                    }
                }
            }
        }
        KCMP_IO | KCMP_SYSVSEM | KCMP_EPOLL_TFD => {
            // 尚未实现
            Err(SysErrNo::EOPNOTSUPP)
        }
        _ => unreachable!(), // 已在函数开头校验
    }
}
