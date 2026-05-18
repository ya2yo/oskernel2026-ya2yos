use alloc::sync::Arc;
use log::{debug, warn};

use crate::{fs::Socket, task::current_task, utils::{SysErrNo, SyscallRet}};

fn do_sock_setsockopt(sock: Arc<Socket>, compat: bool, level: u32,
		       optname: u32, optval: *const u8, optlen: u32) -> SyscallRet
{
	const struct proto_ops *ops;
	char *kernel_optval = NULL;
	int err;

	if optlen < 0 {
		return Err(SysErrNo::EINVAL);
    }


	ops = READ_ONCE(sock->ops);
	if (level == SOL_SOCKET && !sock_use_custom_sol_socket(sock))
		err = sock_setsockopt(sock, level, optname, optval, optlen);
	else if (unlikely(!ops->setsockopt))
		err = -EOPNOTSUPP;
	else
		err = ops->setsockopt(sock, level, optname, optval,
					    optlen);
	kfree(kernel_optval);
}

/// 参考 https://man7.org/linux/man-pages/man2/setsockopt.2.html
pub fn sys_setsockopt(
    sockfd: usize,  // 文件描述符
    level: u32,     // 协议，level 都会设为 SOL_SOCKET
    optname: u32,   // 设定或取出的套接字选项
    user_optval: *const u8,// 指向缓冲区的指针，用来指定或者返回选项的值
    optlen: u32,    // 由 optval 所指向的缓冲区空间大小（字节数）
) -> SyscallRet {
    debug!(
        "sys_setsockopt <= fd: {}, level: {}, optname: {}, optval: {:?}, optlen: {}",
        sockfd,
        level,
        optname,
        user_optval,
        optlen
    );
	// bool compat = in_compat_syscall();
    let compat: bool = false;// 目前只在64位上运行
    let sock: *mut Socket;
	let fd_table=current_task().unwrap().get_fd_table();
    let sock = fd_table.get(sockfd)?.socket()?;
	do_sock_setsockopt(sock, compat, level, optname, user_optval, optlen)
}

/// 参考 https://man7.org/linux/man-pages/man2/setsockopt.2.html
pub fn sys_getsockopt(
    sockfd: usize,  // 文件描述符
    level: u32,     // 协议，level 都会设为 SOL_SOCKET
    optname: u32,   // 设定或取出的套接字选项
    user_optval: *const u8,// 指向缓冲区的指针，用来指定或者返回选项的值
    optlen: u32,    // 由 optval 所指向的缓冲区空间大小（字节数）
) -> SyscallRet {
    Ok(0)
}