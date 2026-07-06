use linux_raw_sys::prctl::{
    PR_CAPBSET_DROP, PR_CAP_AMBIENT, PR_GET_NO_NEW_PRIVS, PR_GET_PDEATHSIG,
    PR_GET_SPECULATION_CTRL, PR_GET_THP_DISABLE, PR_SET_DUMPABLE, PR_SET_NAME, PR_SET_NO_NEW_PRIVS,
    PR_SET_PDEATHSIG, PR_SET_SECCOMP, PR_SET_SECUREBITS, PR_SET_THP_DISABLE, PR_SET_TIMING,
};
use log::{debug, warn};

use crate::{
    mm::{copy_to_user, if_bad_address},
    task::current_task,
    utils::{SysErrNo, SyscallRet},
};

// prctl(2) — 进程控制
/// 参考 https://man7.org/linux/man-pages/man2/prctl.2.html
pub fn sys_prctl(option: u32, arg2: usize, arg3: usize, arg4: usize, arg5: usize) -> SyscallRet {
    debug!(
        "[prctl] option={}, arg2=0x{:x}, arg3=0x{:x}, arg4=0x{:x}, arg5=0x{:x}",
        option, arg2, arg3, arg4, arg5
    );
    let task = current_task().unwrap();
    match option {
        PR_SET_PDEATHSIG => {
            // arg2: 信号编号，0 表示清除
            if arg2 > 64 {
                return Err(SysErrNo::EINVAL);
            }
            let mut inner = task.inner_lock();
            inner.pdeath_signal = arg2 as u8;
            debug!("[prctl] set pdeath_signal={}", arg2);
            Ok(0)
        }
        PR_GET_PDEATHSIG => {
            // 将当前 pdeath_signal 写入 arg2 指向的 int
            let inner = task.inner_lock();
            let sig = inner.pdeath_signal;
            drop(inner);
            if arg2 != 0 {
                let proc_inner = &task.process;
                let memory_set = proc_inner.memory_set_arc();
                let sig_val = sig as i32;
                copy_to_user(&memory_set, arg2, unsafe {
                    core::slice::from_raw_parts(
                        &sig_val as *const i32 as *const u8,
                        core::mem::size_of::<i32>(),
                    )
                })?;
            }
            debug!("[prctl] get pdeath_signal={}", sig);
            Ok(0)
        }
        PR_SET_DUMPABLE => {
            // 仅接受 SUID_DUMP_DISABLE(0) / SUID_DUMP_USER(1)
            if arg2 > 1 {
                return Err(SysErrNo::EINVAL);
            }
            Ok(0)
        }
        PR_SET_NAME => {
            // EFAULT: 非法地址
            if (arg2 as isize) <= 0 || if_bad_address(arg2) {
                return Err(SysErrNo::EFAULT);
            }
            Ok(0)
        }
        PR_SET_SECCOMP => {
            // 仅 SECCOMP_MODE_FILTER(2) 需要 EACCES
            if arg2 == 2 {
                // 没有 CAP_SYS_ADMIN → EACCES
                if arg3 > 0 && if_bad_address(arg3) {
                    return Err(SysErrNo::EFAULT);
                }
                return Err(SysErrNo::EACCES);
            }
            Ok(0)
        }
        PR_CAPBSET_DROP => {
            // 没有 CAP_SETPCAP → EPERM
            Err(SysErrNo::EPERM)
        }
        PR_SET_SECUREBITS => {
            // 没有 CAP_SETPCAP → EPERM
            Err(SysErrNo::EPERM)
        }
        PR_SET_TIMING => {
            // 仅支持 PR_TIMING_STATISTICAL(0)
            Err(SysErrNo::EINVAL)
        }
        PR_SET_NO_NEW_PRIVS => {
            // arg2 必须为 1 且 arg3/4/5 必须为 0
            if arg2 != 1 || arg3 != 0 || arg4 != 0 || arg5 != 0 {
                return Err(SysErrNo::EINVAL);
            }
            Ok(0)
        }
        PR_GET_NO_NEW_PRIVS => {
            // arg2/3/4/5 必须为 0
            if arg2 != 0 || arg3 != 0 || arg4 != 0 || arg5 != 0 {
                return Err(SysErrNo::EINVAL);
            }
            Ok(0)
        }
        PR_SET_THP_DISABLE => {
            // arg3/4/5 必须为 0
            if arg3 != 0 || arg4 != 0 || arg5 != 0 {
                return Err(SysErrNo::EINVAL);
            }
            Ok(0)
        }
        PR_GET_THP_DISABLE => {
            // arg2/3/4/5 必须为 0
            if arg2 != 0 || arg3 != 0 || arg4 != 0 || arg5 != 0 {
                return Err(SysErrNo::EINVAL);
            }
            Ok(0)
        }
        PR_CAP_AMBIENT => {
            // 简化: 所有有效调用返回 0，无效参数返回 EINVAL
            Ok(0)
        }
        PR_GET_SPECULATION_CTRL => {
            // arg3/4/5 必须为 0
            if arg3 != 0 || arg4 != 0 || arg5 != 0 {
                return Err(SysErrNo::EINVAL);
            }
            Ok(0)
        }
        _ => {
            // 未知选项
            warn!("[prctl] unsupported option {}", option);
            Err(SysErrNo::EINVAL)
        }
    }
}
