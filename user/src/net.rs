use core::mem::size_of;

use crate::syscall::{sys_bind, sys_recvfrom, sys_sendto, sys_socket};

pub const AF_INET: isize = 2;
pub const SOCK_STREAM: isize = 1;
pub const SOCK_DGRAM: isize = 2;
pub const SOCK_NONBLOCK: isize = 0x800;

/// Linux `sockaddr_in` ABI used by the IPv4 socket syscalls.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SockAddrIn {
    sin_family: u16,
    sin_port: u16,
    sin_addr: [u8; 4],
    sin_zero: [u8; 8],
}

impl SockAddrIn {
    pub const fn new(ip: [u8; 4], port: u16) -> Self {
        Self {
            sin_family: AF_INET as u16,
            sin_port: port.to_be(),
            sin_addr: ip,
            sin_zero: [0; 8],
        }
    }

    pub const fn ip(&self) -> [u8; 4] {
        self.sin_addr
    }

    pub const fn port(&self) -> u16 {
        u16::from_be(self.sin_port)
    }
}

pub fn socket(domain: isize, tp: isize, proto: isize) -> isize {
    sys_socket(domain, tp, proto)
}

pub fn bind(sockfd: usize, addr: &SockAddrIn) -> isize {
    sys_bind(
        sockfd,
        addr as *const SockAddrIn as *const u8,
        size_of::<SockAddrIn>() as u32,
    )
}

pub fn send_to(sockfd: usize, buf: &[u8], flags: u32, dest_addr: &SockAddrIn) -> isize {
    sys_sendto(
        sockfd,
        buf.as_ptr(),
        buf.len(),
        flags,
        dest_addr as *const SockAddrIn as *const u8,
        size_of::<SockAddrIn>() as u32,
    )
}

pub fn recv_from(sockfd: usize, buf: &mut [u8], flags: u32, src_addr: &mut SockAddrIn) -> isize {
    let mut addrlen = size_of::<SockAddrIn>() as u32;
    sys_recvfrom(
        sockfd,
        buf.as_mut_ptr(),
        buf.len(),
        flags,
        src_addr as *mut SockAddrIn as *mut u8,
        &mut addrlen,
    )
}
