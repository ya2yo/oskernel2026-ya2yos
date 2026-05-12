use super::{SYSCALL_SOCKET, syscall};
pub fn sys_socket(domain:isize, tp: isize, proto:isize )->isize {
    syscall(SYSCALL_SOCKET,[domain, tp , proto, 0, 0, 0])
} 