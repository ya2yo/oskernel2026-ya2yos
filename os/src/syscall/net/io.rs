//! 参考 StarryOS: kernel/src/syscall/net/io.rs
//! 按照 man 手册进行一定程度的完善
use crate::fs::Socket;
use crate::mm::{copy_from_user, copy_to_user, translated_byte_buffer, UserBuffer};
use crate::net::{
    CMsgData, RecvFlags, RecvOptions, SendFlags, SendOptions, SocketAddrEx, SocketOps,
};
use crate::syscall::net::addr::SocketAddrExt;
use crate::syscall::net::CMsg;
use crate::task::{current_task, current_token};
use crate::utils::SyscallRet;
use crate::utils::{SysErrNo, SysResult};
use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use linux_raw_sys::general::iovec;
use linux_raw_sys::net::{cmsghdr, msghdr, socklen_t, MSG_PEEK, MSG_TRUNC};
use log::debug;

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
    debug!("[sys_send]fd: {sockfd}, flags: {flags}, addr: {addr:?}");
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

fn read_user_value<T: Copy>(addr: usize) -> SysResult<T> {
    let task = current_task().unwrap();
    let process = task.process.inner_lock();
    let memory_set = process.get_locked_memory_set_read();
    let mut buf = vec![0u8; size_of::<T>()];
    copy_from_user(&memory_set, addr, &mut buf).map(|_| ())?;
    Ok(unsafe { core::ptr::read_unaligned(buf.as_ptr().cast()) })
}

fn write_user_value<T>(addr: usize, value: &T) -> SysResult {
    let task = current_task().unwrap();
    let process = task.process.inner_lock();
    let memory_set = process.get_locked_memory_set_read();
    let bytes =
        unsafe { core::slice::from_raw_parts(value as *const T as *const u8, size_of::<T>()) };
    copy_to_user(&memory_set, addr, bytes).map(|_| ())
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

// ====================== 以下是 recv 的实现逻辑 ============================

fn recv_impl(
    sockfd: usize,
    dst: UserBuffer,
    flags: u32,
    addr: *mut u8,
    addrlen: Option<&mut socklen_t>,
) -> SyscallRet {
    let mut recv_flags = RecvFlags::default();
    if flags & MSG_PEEK != 0 {
        recv_flags |= RecvFlags::PEEK;
    }
    if flags & MSG_TRUNC != 0 {
        recv_flags |= RecvFlags::TRUNCATE;
    }

    let mut remote_addr = if addr.is_null() || addrlen.is_none() {
        None
    } else {
        Some(SocketAddrEx::Ip((Ipv4Addr::UNSPECIFIED, 0).into()))
    };

    let socket = Socket::from_fd(sockfd)?;
    let recv = socket.recv(
        dst,
        RecvOptions {
            from: remote_addr.as_mut(),
            flags: recv_flags,
            cmsg: None,
        },
    )?;

    if let (Some(remote_addr), Some(addrlen)) = (remote_addr, addrlen) {
        remote_addr.write_to_user(addr, addrlen)?;
    }

    Ok(recv)
}

/// 参考 https://man7.org/linux/man-pages/man2/recvfrom.2.html
/// 系统调用的返回值和前三个参数与read()和write()中的返回值和相应参数是一样的
/// 第四个参数flags 是一个位掩码，它控制着了socket 特定的I/O 特性
/// src_addr 和 addrlen 参数被用来获取或指定与之通信的对等 socket 的地址
pub fn sys_recvfrom(
    sockfd: usize,
    buf: *mut u8,
    len: usize,
    flags: u32,
    src_addr: *mut u8,
    addrlen_ptr: *mut socklen_t,
) -> SyscallRet {
    debug!(
        "
        [sys_recvfrom] sockfd: {sockfd}, 
        buf: {}, 
        len: {}, 
        flags: {}, 
        src_addr: {}, 
        addr_len: {}.",
        buf as usize, len, flags, src_addr as usize, addrlen_ptr as usize
    );
    let token = current_token();
    let buffer = UserBuffer::new(translated_byte_buffer(token, buf, len).ok_or(SysErrNo::EFAULT)?);
    let mut addrlen = if src_addr.is_null() || addrlen_ptr.is_null() {
        None
    } else {
        Some(read_user_value::<socklen_t>(addrlen_ptr as usize)?)
    };
    let recv = recv_impl(sockfd, buffer, flags, src_addr, addrlen.as_mut())?;
    if let Some(addrlen) = addrlen {
        write_user_value(addrlen_ptr as usize, &addrlen)?;
    }
    Ok(recv)
}

use core::mem::size_of;
use core::net::Ipv4Addr;

/// CMSG 对齐辅助函数
const CMSG_ALIGN_SIZE: usize = size_of::<usize>();
fn cmsg_align(len: usize) -> usize {
    (len + CMSG_ALIGN_SIZE - 1) & !(CMSG_ALIGN_SIZE - 1)
}

/// 参考 https://man7.org/linux/man-pages/man2/recvmsg.2.html
pub fn sys_recvmsg(sockfd: usize, msg_ptr: *mut msghdr, flags: u32) -> SyscallRet {
    if msg_ptr.is_null() {
        return Err(SysErrNo::EINVAL);
    }

    let task = current_task().unwrap();
    let process = task.process.inner_lock();
    let memory_set = process.get_locked_memory_set_read();
    let token = memory_set.token();

    let mut msg_buf = [0u8; size_of::<msghdr>()];
    copy_from_user(&memory_set, msg_ptr as usize, &mut msg_buf).map(|_| ())?;
    let mut msg: msghdr = unsafe { *msg_buf.as_ptr().cast() };

    let mut iov_slices = Vec::new();
    if msg.msg_iovlen > 0 {
        if msg.msg_iov.is_null() {
            return Err(SysErrNo::EFAULT);
        }
        let iovs_size = msg
            .msg_iovlen
            .checked_mul(size_of::<iovec>())
            .ok_or(SysErrNo::EINVAL)?;
        let mut iovs_buf = vec![0u8; iovs_size];
        copy_from_user(&memory_set, msg.msg_iov as usize, &mut iovs_buf).map(|_| ())?;
        let iovs: &[iovec] =
            unsafe { core::slice::from_raw_parts(iovs_buf.as_ptr().cast(), msg.msg_iovlen) };
        for iov in iovs {
            if iov.iov_len > 0 {
                if iov.iov_base.is_null() {
                    return Err(SysErrNo::EFAULT);
                }
                let buffers =
                    translated_byte_buffer(token, iov.iov_base as *mut u8, iov.iov_len as usize)
                        .ok_or(SysErrNo::EFAULT)?;
                iov_slices.extend(buffers);
            }
        }
    }

    drop(memory_set);
    drop(process);
    drop(task);

    let mut msg_namelen = msg.msg_namelen as socklen_t;
    let recv = recv_impl(
        sockfd,
        UserBuffer::new(iov_slices),
        flags,
        msg.msg_name as *mut u8,
        Some(&mut msg_namelen),
    )?;

    msg.msg_namelen = msg_namelen as _;
    msg.msg_controllen = 0;
    let task = current_task().unwrap();
    let process = task.process.inner_lock();
    let memory_set = process.get_locked_memory_set_read();
    let msg_bytes = unsafe {
        core::slice::from_raw_parts(&msg as *const msghdr as *const u8, size_of::<msghdr>())
    };
    copy_to_user(&memory_set, msg_ptr as usize, msg_bytes).map(|_| ())?;

    Ok(recv)
}
