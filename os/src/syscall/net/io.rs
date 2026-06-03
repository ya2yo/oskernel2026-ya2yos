// 以下函数使用cursor完成
// StarryOS 原先的实现过于简单，实际linux实现相当复杂
// 以下根据 StarryOS 原先 net 模块的实现进行扩充
use crate::fs::Socket;
use crate::mm::{copy_from_user, copy_to_user, safe_translated_byte_buffer, UserBuffer};
use crate::net::{
    CMsgData, RecvFlags, RecvOptions, SendFlags, SendOptions, SocketAddrEx, SocketOps,
};
use crate::syscall::net::addr::SocketAddrExt;
use crate::syscall::net::CMsg;
use crate::task::current_task;
use crate::utils::{SysErrNo, SysResult, SyscallRet};
use alloc::vec;
use alloc::vec::Vec;
use core::{mem::size_of, net::Ipv4Addr};
use linux_raw_sys::general::iovec;
use linux_raw_sys::net::{
    msghdr, socklen_t, MSG_CMSG_CLOEXEC, MSG_CONFIRM, MSG_DONTROUTE, MSG_DONTWAIT, MSG_EOR,
    MSG_MORE, MSG_NOSIGNAL, MSG_OOB, MSG_PEEK, MSG_TRUNC, MSG_WAITALL,
};
use log::debug;

const MAX_IOV: usize = 1024;

fn copy_msghdr_from_user(ptr: *const msghdr) -> SysResult<msghdr> {
    if ptr.is_null() {
        return Err(SysErrNo::EFAULT);
    }
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let process = task.process.inner_lock();
    let memory_set = process.get_locked_memory_set_read();
    let mut bytes = vec![0u8; size_of::<msghdr>()];
    copy_from_user(&memory_set, ptr as usize, &mut bytes).map(|_| ())?;
    Ok(unsafe { core::ptr::read_unaligned(bytes.as_ptr().cast::<msghdr>()) })
}

fn copy_msghdr_to_user(ptr: *mut msghdr, msg: &msghdr) -> SysResult {
    if ptr.is_null() {
        return Err(SysErrNo::EFAULT);
    }
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let process = task.process.inner_lock();
    let memory_set = process.get_locked_memory_set_read();
    let bytes = unsafe {
        core::slice::from_raw_parts(msg as *const msghdr as *const u8, size_of::<msghdr>())
    };
    copy_to_user(&memory_set, ptr as usize, bytes).map(|_| ())
}

fn copy_iovec_from_user(ptr: *const iovec) -> SysResult<iovec> {
    if ptr.is_null() {
        return Err(SysErrNo::EFAULT);
    }
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let process = task.process.inner_lock();
    let memory_set = process.get_locked_memory_set_read();
    let mut bytes = vec![0u8; size_of::<iovec>()];
    copy_from_user(&memory_set, ptr as usize, &mut bytes).map(|_| ())?;
    Ok(unsafe { core::ptr::read_unaligned(bytes.as_ptr().cast::<iovec>()) })
}

fn copy_socklen_from_user(ptr: *const socklen_t) -> SysResult<socklen_t> {
    if ptr.is_null() {
        return Err(SysErrNo::EFAULT);
    }
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let process = task.process.inner_lock();
    let memory_set = process.get_locked_memory_set_read();
    let mut bytes = [0u8; size_of::<socklen_t>()];
    copy_from_user(&memory_set, ptr as usize, &mut bytes).map(|_| ())?;
    Ok(socklen_t::from_ne_bytes(bytes))
}

fn copy_socklen_to_user(ptr: *mut socklen_t, value: socklen_t) -> SysResult {
    if ptr.is_null() {
        return Err(SysErrNo::EFAULT);
    }
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let process = task.process.inner_lock();
    let memory_set = process.get_locked_memory_set_read();
    copy_to_user(&memory_set, ptr as usize, &value.to_ne_bytes()).map(|_| ())
}

fn send_flags(flags: u32) -> SendFlags {
    let mut result = SendFlags::default();
    if flags & MSG_OOB != 0 {
        result |= SendFlags::OOB;
    }
    if flags & MSG_DONTROUTE != 0 {
        result |= SendFlags::DONTROUTE;
    }
    if flags & MSG_DONTWAIT != 0 {
        result |= SendFlags::DONTWAIT;
    }
    if flags & MSG_EOR != 0 {
        result |= SendFlags::EOR;
    }
    if flags & MSG_CONFIRM != 0 {
        result |= SendFlags::CONFIRM;
    }
    if flags & MSG_NOSIGNAL != 0 {
        result |= SendFlags::NOSIGNAL;
    }
    if flags & MSG_MORE != 0 {
        result |= SendFlags::MORE;
    }
    result
}

fn recv_flags(flags: u32) -> RecvFlags {
    let mut result = RecvFlags::default();
    if flags & MSG_OOB != 0 {
        result |= RecvFlags::OOB;
    }
    if flags & MSG_PEEK != 0 {
        result |= RecvFlags::PEEK;
    }
    if flags & MSG_DONTWAIT != 0 {
        result |= RecvFlags::DONTWAIT;
    }
    if flags & MSG_WAITALL != 0 {
        result |= RecvFlags::WAITALL;
    }
    if flags & MSG_TRUNC != 0 {
        result |= RecvFlags::TRUNCATE;
    }
    if flags & MSG_CMSG_CLOEXEC != 0 {
        result |= RecvFlags::CMSG_CLOEXEC;
    }
    result
}

fn read_iovecs(msg: &msghdr) -> SysResult<Vec<iovec>> {
    let iov_len = msg.msg_iovlen;
    if iov_len == 0 {
        return Ok(Vec::new());
    }
    if msg.msg_iov.is_null() {
        return Err(SysErrNo::EFAULT);
    }
    if iov_len > MAX_IOV {
        return Err(SysErrNo::EINVAL);
    }

    let mut iovs = Vec::with_capacity(iov_len);
    let base = msg.msg_iov as usize;
    for idx in 0..iov_len {
        let offset = idx
            .checked_mul(size_of::<iovec>())
            .ok_or(SysErrNo::EINVAL)?;
        iovs.push(copy_iovec_from_user((base + offset) as *const iovec)?);
    }
    Ok(iovs)
}

fn iovecs_to_user_buffer(iovs: &[iovec]) -> SysResult<UserBuffer> {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let process = task.process.inner_lock();
    let memory_set = process.get_locked_memory_set_read();
    let mut slices = Vec::new();
    for iov in iovs {
        let len = iov.iov_len as usize;
        if len == 0 {
            continue;
        }
        if iov.iov_base.is_null() {
            return Err(SysErrNo::EFAULT);
        }
        let buffers = safe_translated_byte_buffer(&memory_set, iov.iov_base as *const u8, len)
            .ok_or(SysErrNo::EFAULT)?;
        slices.extend(buffers);
    }
    Ok(UserBuffer::new(slices))
}

fn parse_cmsgs(msg: &msghdr) -> SysResult<Vec<CMsgData>> {
    if msg.msg_control.is_null() || msg.msg_controllen == 0 {
        return Ok(Vec::new());
    }
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let process = task.process.inner_lock();
    let memory_set = process.get_locked_memory_set_read();
    let mut control = vec![0u8; msg.msg_controllen as usize];
    copy_from_user(&memory_set, msg.msg_control as usize, &mut control).map(|_| ())?;
    CMsg::parse_control_messages(&control)
}

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
    // debug!("[sys_send]fd: {sockfd}, flags: {flags}, addr: {addr:?}");
    let socket = Socket::from_fd(sockfd)?;
    let sent = socket.send(
        src,
        SendOptions {
            to: addr,
            flags: send_flags(flags),
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
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let process = task.process.inner_lock();
    let memory_set = process.get_locked_memory_set_read();
    let buffer = UserBuffer::new(
        safe_translated_byte_buffer(&memory_set, buf, len).ok_or(SysErrNo::EFAULT)?,
    );
    drop(memory_set);
    drop(process);
    send_impl(sockfd, buffer, flags, dest_addr, addrlen, Vec::new())
}

/// 参考 https://man7.org/linux/man-pages/man2/sendmsg.2.html
pub fn sys_sendmsg(sockfd: usize, msg_ptr: *const msghdr, flags: u32) -> SyscallRet {
    let msg = copy_msghdr_from_user(msg_ptr)?;
    let user_buffer = iovecs_to_user_buffer(&read_iovecs(&msg)?)?;
    let cmsgs = parse_cmsgs(&msg)?;
    send_impl(
        sockfd,
        user_buffer,
        flags,
        msg.msg_name as *const u8,
        msg.msg_namelen as socklen_t,
        cmsgs,
    )
}

// ====================== 以下是 recv 的实现逻辑 ============================

fn recv_impl(
    sockfd: usize,
    dst: UserBuffer,
    flags: u32,
    addr: *mut u8,
    addrlen: Option<&mut socklen_t>,
) -> SyscallRet {
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
            flags: recv_flags(flags),
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
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let process = task.process.inner_lock();
    let memory_set = process.get_locked_memory_set_read();
    let buffer = UserBuffer::new(
        safe_translated_byte_buffer(&memory_set, buf, len).ok_or(SysErrNo::EFAULT)?,
    );
    drop(memory_set);
    drop(process);
    let mut addrlen = if src_addr.is_null() || addrlen_ptr.is_null() {
        None
    } else {
        Some(copy_socklen_from_user(addrlen_ptr as *const socklen_t)?)
    };
    let recv = recv_impl(sockfd, buffer, flags, src_addr, addrlen.as_mut())?;
    if let Some(addrlen) = addrlen {
        copy_socklen_to_user(addrlen_ptr, addrlen)?;
    }
    Ok(recv)
}

/// 参考 https://man7.org/linux/man-pages/man2/recvmsg.2.html
pub fn sys_recvmsg(sockfd: usize, msg_ptr: *mut msghdr, flags: u32) -> SyscallRet {
    let mut msg = copy_msghdr_from_user(msg_ptr as *const msghdr)?;
    let user_buffer = iovecs_to_user_buffer(&read_iovecs(&msg)?)?;

    let mut msg_namelen = if msg.msg_name.is_null() {
        0
    } else {
        msg.msg_namelen as socklen_t
    };
    let recv = recv_impl(
        sockfd,
        user_buffer,
        flags,
        msg.msg_name as *mut u8,
        (!msg.msg_name.is_null()).then_some(&mut msg_namelen),
    )?;

    msg.msg_namelen = msg_namelen as _;
    msg.msg_controllen = 0;
    msg.msg_flags = 0;
    copy_msghdr_to_user(msg_ptr, &msg)?;

    Ok(recv)
}
