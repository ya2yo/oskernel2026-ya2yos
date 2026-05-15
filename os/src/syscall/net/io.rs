//! 参考 StarryOS: kernel/src/syscall/net/io.rs
use core::slice;

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use linux_raw_sys::general::iovec;
use linux_raw_sys::net::{cmsghdr, msghdr, sockaddr, socklen_t};
use log::{debug, warn};
use crate::fs::Socket;
use crate::mm::UserBuffer;
use crate::net::{CMsgData, SendFlags, SendOptions, SocketAddrEx, SocketOps};
use crate::syscall::net::CMsg;
use crate::utils::SysErrNo;
use crate::{fs::File, utils::SyscallRet};
use crate::syscall::net::addr::SocketAddrExt;

fn send_impl(
    sockfd: usize,
    src: UserBuffer,
    flags: u32,
    addr: *const u8,
    addrlen: socklen_t,
    cmsg: Vec<CMsgData>,
) -> SyscallRet {
    let addr = if addr.is_null() || addrlen == 0 {
        None
    } else {
        Some(SocketAddrEx::read_from_user(addr, addrlen)?)
    };

    debug!("sys_send <= fd: {sockfd}, flags: {flags}, addr: {addr:?}");

    let socket = Socket::from_fd(sockfd)?;
    let sent = socket.send(
        src,
        SendOptions {
            to: addr,
            flags: SendFlags::default(),
            cmsg,
        },
    )?;

    Ok(sent)
}

/// 参考 https://man7.org/linux/man-pages/man2/sendto.2.html
pub fn sys_sendto(
    sockfd: usize,
    buf: *const u8,
    len: usize,
    flags: u32,
    dest_addr: *const u8,
    addrlen: u32,
) -> SyscallRet {
    let slice: &'static mut [u8] = unsafe {
        core::slice::from_raw_parts_mut(buf as *mut u8, len)
    };
    let buffer = vec![slice];
    send_impl(sockfd, UserBuffer::new(buffer), flags, dest_addr, addrlen, Vec::new())
}

/// 参考 https://man7.org/linux/man-pages/man2/recvfrom.2.html
pub fn sys_recvfrom(
    _sockfd: usize,
    _buf: *mut u8,
    _len: usize,
    _flags: u32,
    _src_addr: *const u8,
    _addrlen: u32,
) -> SyscallRet {
    debug!("ENTER recvfrom");
    todo!("recvfrom")
}

use core::mem::size_of;

/// CMSG 对齐辅助函数
const CMSG_ALIGN_SIZE: usize = size_of::<usize>();
fn cmsg_align(len: usize) -> usize {
    (len + CMSG_ALIGN_SIZE - 1) & !(CMSG_ALIGN_SIZE - 1)
}

/// 参考 https://man7.org/linux/man-pages/man2/sendmsg.2.html
pub fn sys_sendmsg(sockfd: usize, msg_ptr: *const msghdr, flags: u32) -> SyscallRet {
    if msg_ptr.is_null() {
        return Err(SysErrNo::EINVAL);
    }
    let hdr_size = size_of::<cmsghdr>();
    let msg = unsafe { &*msg_ptr };
    let mut iov_slices = Vec::new();
    if msg.msg_iovlen > 0 && !msg.msg_iov.is_null() {
        let iovs = unsafe {
            core::slice::from_raw_parts(msg.msg_iov as *const iovec, msg.msg_iovlen as usize)
        };
        for iov in iovs {
            if iov.iov_len > 0 && !iov.iov_base.is_null() {
                // 这里假设你的 UserBuffer 构造函数接受 Vec<&'static mut [u8]>
                // 注意：在内核中这通常是 unsafe 的，需确保 UserBuffer 内部处理了虚实映射
                let slice = unsafe {
                    core::slice::from_raw_parts_mut(iov.iov_base as *mut u8, iov.iov_len as usize)
                };
                iov_slices.push(slice);
            }
        }
    }
    let user_buffer = UserBuffer::new(iov_slices);

    let mut cmsgs = Vec::new();
    if !msg.msg_control.is_null() && msg.msg_controllen >= size_of::<cmsghdr>() {
        let control_base = msg.msg_control as usize;
        let control_len = msg.msg_controllen as usize;
        let mut offset = 0;

        while offset + size_of::<cmsghdr>() <= control_len {
            let hdr_ptr = (control_base + offset) as *const cmsghdr;
            let hdr = unsafe { &*hdr_ptr };

            if hdr.cmsg_len < size_of::<cmsghdr>() as _ || 
               (offset + hdr.cmsg_len as usize) > control_len {
                break;
            }

            let data_ptr = (hdr_ptr as usize + hdr_size) as *const u8;
            let data_len = hdr.cmsg_len - hdr_size;
            let data_slice = unsafe { core::slice::from_raw_parts(data_ptr, data_len) };

            cmsgs.push(Box::new(CMsg::parse(hdr,data_slice)?) as CMsgData);
            offset += cmsg_align(hdr.cmsg_len as usize);
        }
    }
    let addr_ptr = msg.msg_name as *const u8;
    let addr_len = msg.msg_namelen as u32;

    send_impl(
        sockfd,
        user_buffer,
        flags,
        addr_ptr,
        addr_len,
        cmsgs,
    )
}

/// 参考 https://man7.org/linux/man-pages/man2/recvmsg.2.html
pub fn sys_recvmsg(_sockfd: usize, msg_ptr: *mut msghdr, _flags: u32) -> SyscallRet {
    if msg_ptr.is_null() {
        return Err(SysErrNo::EINVAL);
    }
    let msg = unsafe { &mut *msg_ptr };

    // 1. 准备接收数据的 UserBuffer
    let mut iov_slices = Vec::new();
    let iovs = unsafe {
        core::slice::from_raw_parts(msg.msg_iov as *const iovec, msg.msg_iovlen as usize)
    };
    for iov in iovs {
        let slice = unsafe {
            core::slice::from_raw_parts_mut(iov.iov_base as *mut u8, iov.iov_len as usize)
        };
        iov_slices.push(slice);
    }
    todo!("实现 recvmsg 的具体返回逻辑")
}