use super::*;

/// 处理 `ioctl(2)` 文件控制请求。
///
/// 根据 `fd` 取得目标文件对象，并把命令号和用户参数转发给具体 `File::ioctl`
/// 实现。`LOOP_SET_FD` 需要额外校验参数 fd 存在，避免 loop 设备绑定坏 fd。
/// 参考 https://man7.org/linux/man-pages/man2/ioctl.2.html
pub fn sys_ioctl(fd: usize, cmd: usize, arg: usize) -> SyscallRet {
    debug!("[sys_ioctl] fd={}, cmd={}, arg={}", fd, cmd, arg);
    let task = current_task().unwrap();
    let proc = &task.process;
    if cmd as u32 == LOOP_SET_FD {
        proc.fd_table.get(arg)?;
    }
    let file = proc.fd_table.get(fd)?.any();
    let memory_set = proc.memory_set_arc();
    file.ioctl(cmd as u32, arg, &memory_set)
}
