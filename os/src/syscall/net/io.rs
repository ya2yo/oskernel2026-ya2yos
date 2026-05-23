//! 参考 StarryOS: kernel/src/syscall/net/io.rs
use crate::fs::Socket;
use crate::mm::{copy_from_user, translated_byte_buffer, UserBuffer};
use crate::net::{CMsgData, SendFlags, SendOptions, SocketAddrEx, SocketOps};
use crate::syscall::net::addr::SocketAddrExt;
use crate::syscall::net::CMsg;
use crate::task::{current_task, current_token};
use crate::utils::SysErrNo;
use crate::{fs::File, utils::SyscallRet};
use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use linux_raw_sys::general::iovec;
use linux_raw_sys::net::{cmsghdr, msghdr, sockaddr, socklen_t};
use log::{debug, warn};

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
    let token = current_token();
    let buffer = UserBuffer::new(translated_byte_buffer(token, buf, len).ok_or(SysErrNo::EFAULT)?);
    send_impl(sockfd, buffer, flags, dest_addr, addrlen, Vec::new())
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
    let task = current_task().unwrap();
    let process = task.process.inner_lock();
    let memory_set = process.get_locked_memory_set_read();
    let token = memory_set.token();
    let hdr_size = size_of::<cmsghdr>();

    // 通过 copy_from_user 读取 msghdr
    let mut msg_buf = [0u8; size_of::<msghdr>()];
    copy_from_user(&memory_set, msg_ptr as usize, &mut msg_buf).map(|_| ())?;
    let msg: msghdr = unsafe { *msg_buf.as_ptr().cast() };

    let mut iov_slices = Vec::new();
    if msg.msg_iovlen > 0 && !msg.msg_iov.is_null() {
        // 通过 copy_from_user 读取 iovec 数组
        let iovs_size = msg.msg_iovlen as usize * size_of::<iovec>();
        let mut iovs_buf = vec![0u8; iovs_size];
        copy_from_user(&memory_set, msg.msg_iov as usize, &mut iovs_buf).map(|_| ())?;
        let iovs: &[iovec] = unsafe {
            core::slice::from_raw_parts(iovs_buf.as_ptr().cast(), msg.msg_iovlen as usize)
        };
        for iov in iovs {
            if iov.iov_len > 0 && !iov.iov_base.is_null() {
                let buffers =
                    translated_byte_buffer(token, iov.iov_base as *const u8, iov.iov_len as usize)
                        .ok_or(SysErrNo::EFAULT)?;
                iov_slices.extend(buffers);
            }
        }
    }
    let user_buffer = UserBuffer::new(iov_slices);

    let mut cmsgs = Vec::new();
    if !msg.msg_control.is_null() && msg.msg_controllen >= size_of::<cmsghdr>() {
        let control_base = msg.msg_control as usize;
        let control_len = msg.msg_controllen as usize;
        // 通过 copy_from_user 读取整个 control 缓冲区
        let mut control_buf = vec![0u8; control_len];
        copy_from_user(&memory_set, control_base, &mut control_buf).map(|_| ())?;

        let mut offset = 0;
        while offset + size_of::<cmsghdr>() <= control_len {
            let hdr = unsafe { &*(control_buf.as_ptr().add(offset).cast::<cmsghdr>()) };

            if hdr.cmsg_len < size_of::<cmsghdr>() as _
                || (offset + hdr.cmsg_len as usize) > control_len
            {
                break;
            }

            let data_start = offset + hdr_size;
            let data_len = hdr.cmsg_len as usize - hdr_size;
            let data_slice = &control_buf[data_start..data_start + data_len];

            cmsgs.push(Box::new(CMsg::parse(hdr, data_slice)?) as CMsgData);
            offset += cmsg_align(hdr.cmsg_len as usize);
        }
    }
    let addr_ptr = msg.msg_name as *const u8;
    let addr_len = msg.msg_namelen as u32;
    drop(memory_set);
    drop(process);
    drop(task);

    send_impl(sockfd, user_buffer, flags, addr_ptr, addr_len, cmsgs)
}

/// 参考 https://man7.org/linux/man-pages/man2/recvmsg.2.html
pub fn sys_recvmsg(_sockfd: usize, msg_ptr: *mut msghdr, _flags: u32) -> SyscallRet {
    if msg_ptr.is_null() {
        return Err(SysErrNo::EINVAL);
    }
    let task = current_task().unwrap();
    let process = task.process.inner_lock();
    let memory_set = process.get_locked_memory_set_read();
    let token = memory_set.token();

    // 通过 copy_from_user 读取 msghdr
    let mut msg_buf = [0u8; size_of::<msghdr>()];
    copy_from_user(&memory_set, msg_ptr as usize, &mut msg_buf).map(|_| ())?;
    let msg: msghdr = unsafe { *msg_buf.as_ptr().cast() };

    // 1. 准备接收数据的 UserBuffer
    let mut iov_slices = Vec::new();
    if msg.msg_iovlen > 0 && !msg.msg_iov.is_null() {
        let iovs_size = msg.msg_iovlen as usize * size_of::<iovec>();
        let mut iovs_buf = vec![0u8; iovs_size];
        copy_from_user(&memory_set, msg.msg_iov as usize, &mut iovs_buf).map(|_| ())?;
        let iovs: &[iovec] = unsafe {
            core::slice::from_raw_parts(iovs_buf.as_ptr().cast(), msg.msg_iovlen as usize)
        };
        for iov in iovs {
            if iov.iov_len > 0 && !iov.iov_base.is_null() {
                let buffers =
                    translated_byte_buffer(token, iov.iov_base as *const u8, iov.iov_len as usize)
                        .ok_or(SysErrNo::EFAULT)?;
                iov_slices.extend(buffers);
            }
        }
    }
    todo!("实现 recvmsg 的具体返回逻辑")
}
