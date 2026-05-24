use log::error;

use crate::task::{
    current_task, exit_current_and_run_next, exit_current_group_and_run_next, ready_queue,
    TaskStatus,
};
use crate::utils::SyscallRet;
/// 参考 https://man7.org/linux/man-pages/man2/exit.2.html
pub fn sys_exit(exit_code: i32) -> ! {
    exit_current_and_run_next(exit_code);
    unreachable!();
}

/// 参考 https://man7.org/linux/man-pages/man2/exit_group.2.html
pub fn sys_exit_group(exit_code: i32) -> SyscallRet {
    exit_current_group_and_run_next(exit_code);
    unreachable!();
}
