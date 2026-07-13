use super::{syscall, SYSCALL_BIND, SYSCALL_RECVFROM, SYSCALL_SENDTO, SYSCALL_SOCKET};

pub fn sys_socket(domain: isize, tp: isize, proto: isize) -> isize {
    syscall(SYSCALL_SOCKET, [domain, tp, proto, 0, 0, 0])
}

pub fn sys_bind(sockfd: usize, addr: *const u8, addrlen: u32) -> isize {
    syscall(
        SYSCALL_BIND,
        [sockfd as isize, addr as isize, addrlen as isize, 0, 0, 0],
    )
}

pub fn sys_sendto(
    sockfd: usize,
    buf: *const u8,
    len: usize,
    flags: u32,
    dest_addr: *const u8,
    addrlen: u32,
) -> isize {
    syscall(
        SYSCALL_SENDTO,
        [
            sockfd as isize,
            buf as isize,
            len as isize,
            flags as isize,
            dest_addr as isize,
            addrlen as isize,
        ],
    )
}

pub fn sys_recvfrom(
    sockfd: usize,
    buf: *mut u8,
    len: usize,
    flags: u32,
    src_addr: *mut u8,
    addrlen: *mut u32,
) -> isize {
    syscall(
        SYSCALL_RECVFROM,
        [
            sockfd as isize,
            buf as isize,
            len as isize,
            flags as isize,
            src_addr as isize,
            addrlen as isize,
        ],
    )
}
