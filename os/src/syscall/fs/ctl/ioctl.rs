use super::*;
use crate::mm::copy_from_user_val;

const FIONBIO: u32 = 0x5421;

/// 处理 `ioctl(2)` 文件控制请求。
///
/// 根据 `fd` 取得目标文件对象，并把命令号和用户参数转发给具体 `File::ioctl`
/// 实现。`LOOP_SET_FD` 需要额外校验参数 fd 存在，避免 loop 设备绑定坏 fd。
/// 参考 https://man7.org/linux/man-pages/man2/ioctl.2.html
pub fn sys_ioctl(fd: usize, cmd: usize, arg: usize) -> SyscallRet {
    debug!("[sys_ioctl] fd={}, cmd={}, arg={}", fd, cmd, arg);
    let task = current_task().unwrap();
    let proc = &task.process;
    let cmd = cmd as u32;
    if cmd == LOOP_SET_FD {
        proc.fd_table.get(arg)?;
    }
    let file = proc.fd_table.get(fd)?.any();
    let memory_set = proc.memory_set_arc();

    // Linux handles FIONBIO in the common VFS ioctl path.  It changes the
    // file status flag, rather than being a pipe- or tty-specific operation.
    if cmd == FIONBIO {
        let nonblocking = copy_from_user_val::<i32>(&memory_set, arg as *const i32)? != 0;
        file.set_nonblocking(nonblocking)?;
        if nonblocking {
            proc.fd_table.set_nonblock(fd)?;
        } else {
            proc.fd_table.unset_nonblock(fd)?;
        }
        return Ok(0);
    }

    file.ioctl(cmd, arg, &memory_set)
}
