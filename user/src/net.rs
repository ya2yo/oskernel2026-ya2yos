use crate::syscall::sys_socket;

pub const AF_INET: isize = 2;
pub const SOCK_STREAM: isize = 1;
pub const SOCK_DGRAM: isize = 2;

pub fn socket(domain: isize, tp: isize, proto: isize) -> isize {
    sys_socket(domain, tp, proto)
}

