use super::consts::*;
use crate::{
    fs::Socket,
    mm::{copy_from_user, copy_to_user},
    net::options::{Configurable, GetSocketOption, SetSocketOption},
    task::{current_task, tid_to_task::task_num},
    utils::{SysErrNo, SysResult, SyscallRet},
};
use alloc::sync::Arc;
use alloc::{vec, vec::Vec};
use linux_raw_sys::net::{
    group_req, group_source_req, socklen_t, tcp_info, IPV6_V6ONLY, IP_MSFILTER, IP_MTU,
    IP_MTU_DISCOVER, IP_MULTICAST_IF, IP_RECVERR, IP_RETOPTS, IP_TTL, MCAST_JOIN_GROUP,
    MCAST_LEAVE_GROUP, SOL_SOCKET, SO_DONTROUTE, SO_ERROR, SO_KEEPALIVE, SO_RCVBUF, SO_RCVTIMEO,
    SO_REUSEADDR, SO_SNDBUF, SO_SNDTIMEO, TCP_INFO, TCP_MAXSEG, TCP_NODELAY,
};
use log::{debug, error, warn};

use core::{mem::size_of, ptr::read_unaligned, time::Duration};

/// ABI 数据读写接口
pub trait AbiValue: Sized {
    /// ABI 占用字节数
    const SIZE: usize;
    /// 从字节流读取
    fn read_from(data: &[u8]) -> SysResult<Self>;
    /// 写入字节流
    fn write_to(self, data: &mut [u8]) -> SysResult<()>;
}

/// 检查 buffer 长度
#[inline]
fn ensure_len(data: &[u8], need: usize) -> SysResult<()> {
    if data.len() < need {
        Err(SysErrNo::EINVAL)
    } else {
        Ok(())
    }
}

impl AbiValue for bool {
    const SIZE: usize = 4;

    fn read_from(data: &[u8]) -> SysResult<Self> {
        ensure_len(data, Self::SIZE)?;

        let val = i32::from_ne_bytes(data[..4].try_into().unwrap());

        Ok(val != 0)
    }

    fn write_to(self, data: &mut [u8]) -> SysResult<()> {
        ensure_len(data, Self::SIZE)?;

        let val: i32 = if self { 1 } else { 0 };

        data[..4].copy_from_slice(&val.to_ne_bytes());

        Ok(())
    }
}

impl AbiValue for i32 {
    const SIZE: usize = 4;

    fn read_from(data: &[u8]) -> SysResult<Self> {
        ensure_len(data, Self::SIZE)?;

        Ok(i32::from_ne_bytes(data[..4].try_into().unwrap()))
    }

    fn write_to(self, data: &mut [u8]) -> SysResult<()> {
        ensure_len(data, Self::SIZE)?;

        data[..4].copy_from_slice(&self.to_ne_bytes());

        Ok(())
    }
}

impl AbiValue for usize {
    const SIZE: usize = 8;

    fn read_from(data: &[u8]) -> SysResult<Self> {
        ensure_len(data, Self::SIZE)?;

        Ok(u64::from_ne_bytes(data[..8].try_into().unwrap()) as usize)
    }

    fn write_to(self, data: &mut [u8]) -> SysResult<()> {
        ensure_len(data, Self::SIZE)?;

        data[..8].copy_from_slice(&(self as u64).to_ne_bytes());

        Ok(())
    }
}

impl AbiValue for Duration {
    const SIZE: usize = 16;

    fn read_from(data: &[u8]) -> SysResult<Self> {
        ensure_len(data, Self::SIZE)?;

        let sec = u64::from_ne_bytes(data[..8].try_into().unwrap());

        let usec = u64::from_ne_bytes(data[8..16].try_into().unwrap());

        Ok(Duration::from_secs(sec) + Duration::from_micros(usec))
    }

    fn write_to(self, data: &mut [u8]) -> SysResult<()> {
        ensure_len(data, Self::SIZE)?;

        let sec = self.as_secs();

        let usec = self.subsec_micros() as u64;

        data[..8].copy_from_slice(&sec.to_ne_bytes());

        data[8..16].copy_from_slice(&usec.to_ne_bytes());

        Ok(())
    }
}

impl AbiValue for group_source_req {
    const SIZE: usize = size_of::<group_source_req>();

    fn read_from(data: &[u8]) -> SysResult<Self> {
        ensure_len(data, Self::SIZE)?;
        Ok(unsafe { core::ptr::read_unaligned(data.as_ptr() as *const group_source_req) })
    }

    fn write_to(self, data: &mut [u8]) -> SysResult<()> {
        ensure_len(data, Self::SIZE)?;
        unsafe { core::ptr::write_unaligned(data.as_mut_ptr() as *mut group_source_req, self) };
        Ok(())
    }
}

impl AbiValue for group_req {
    const SIZE: usize = size_of::<group_req>();
    fn read_from(data: &[u8]) -> SysResult<Self> {
        ensure_len(data, Self::SIZE)?;
        Ok(unsafe { core::ptr::read_unaligned(data.as_ptr() as *const group_req) })
    }
    fn write_to(self, data: &mut [u8]) -> SysResult<()> {
        ensure_len(data, Self::SIZE)?;
        unsafe { core::ptr::write_unaligned(data.as_ptr() as *mut group_req, self) };
        Ok(())
    }
}

#[inline]
pub fn parse<T: AbiValue>(data: &[u8]) -> SysResult<T> {
    T::read_from(data)
}

#[inline]
pub fn write<T: AbiValue>(data: &mut [u8], val: T) -> SysResult<()> {
    val.write_to(data)
}

/// 参考 https://man7.org/linux/man-pages/man2/setsockopt.2.html
/// 参考了RocketOS里面的设计
pub fn sys_setsockopt(
    sockfd: usize,          // 文件描述符
    level: u32,             // 协议，level 都会设为 SOL_SOCKET
    optname: u32,           // 设定或取出的套接字选项
    user_optval: *const u8, // 指向缓冲区的指针，用来指定或者返回选项的值
    optlen: u32,            // 由 optval 所指向的缓冲区空间大小（字节数）
) -> SyscallRet {
    debug!(
        "[sys_setsockopt] fd: {}, level: {}, optname: {}, optval: {:?}, optlen: {}",
        sockfd, level, optname, user_optval, optlen
    );
    // bool compat = in_compat_syscall();
    // let compat: bool = false;// 目前只在64位上运行
    let task = current_task().unwrap();
    // debug!("strong count: {}", Arc::strong_count(&task));
    let fd_table = task.process.fd_table.clone();
    drop(task);
    let sock = fd_table.get(sockfd)?.socket()?;
    if optlen > 1024 {
        return Err(SysErrNo::EINVAL);
    }
    let mut kern_optval = vec![0; optlen as usize];
    let task = current_task().unwrap();
    let process = &task.process;
    let memory_set = process.memory_set_arc();
    copy_from_user(&memory_set, user_optval as usize, &mut kern_optval)?;
    drop(memory_set);
    drop(task);
    match level {
        SOL_SOCKET => match optname {
            SO_REUSEADDR => {
                let val = parse(&kern_optval)?;
                let opt = SetSocketOption::ReuseAddress(&val);
                sock.set_option(opt)
            }
            SO_SNDBUF => {
                let val: i32 = parse(&kern_optval)?;
                if val < 0 {
                    return Err(SysErrNo::EINVAL);
                }
                let val = val as usize;
                let opt = SetSocketOption::SendBuffer(&val);
                sock.set_option(opt)
            }
            SO_RCVBUF => {
                let val: i32 = parse(&kern_optval)?;
                if val < 0 {
                    return Err(SysErrNo::EINVAL);
                }
                let val = val as usize;
                let opt = SetSocketOption::ReceiveBuffer(&val);
                sock.set_option(opt)
            }
            SO_KEEPALIVE => {
                let val = parse(&kern_optval)?;
                let opt = SetSocketOption::KeepAlive(&val);
                sock.set_option(opt)
            }
            SO_DONTROUTE => {
                let val = parse(&kern_optval)?;
                let opt = SetSocketOption::DontRoute(&val);
                sock.set_option(opt)
            }
            SO_RCVTIMEO => {
                let val = parse(&kern_optval)?;
                let opt = SetSocketOption::ReceiveTimeout(&val);
                sock.set_option(opt)
            }
            SO_SNDTIMEO => {
                let val = parse(&kern_optval)?;
                let opt = SetSocketOption::SendTimeout(&val);
                sock.set_option(opt)
            }
            _ => {
                warn!("[sys_setsockopt] not available SOL_SOCKET protocol! level = {}, optname = {optname}", level);
                return Err(SysErrNo::ENOPROTOOPT);
            }
        },
        // IP 级
        IPPROTO_IP => match optname {
            IP_TTL => {
                let val: i32 = parse(&kern_optval)?;
                let val = val as u8;
                let opt = SetSocketOption::Ttl(&val);
                sock.set_option(opt)
            }
            IP_RECVERR | IP_MTU_DISCOVER => Ok(()),
            MCAST_JOIN_GROUP => {
                let val: group_req = parse(&kern_optval)?;
                let opt = SetSocketOption::JoinGroup(&val);
                sock.set_option(opt)
            }
            IP_MULTICAST_IF => Ok(()), // 目前只返回成功，不做实际操作
            MCAST_LEAVE_GROUP => {
                let val: group_req = parse(&kern_optval)?;
                let opt = SetSocketOption::LeaveGroup(&val);
                sock.set_option(opt)
            }
            _ => {
                warn!(
                    "[sys_setsockopt] not available IP protocol! level = {}, optname = {}",
                    level, optname
                );
                return Err(SysErrNo::ENOPROTOOPT);
            }
        },
        IPPROTO_IPV6 => match optname {
            IPV6_V6ONLY => {
                let _val: bool = parse(&kern_optval)?;
                Ok(())
            }
            _ => {
                warn!(
                    "[sys_setsockopt] not available IPV6 protocol! level = {}, optname = {}",
                    level, optname
                );
                return Err(SysErrNo::ENOPROTOOPT);
            }
        },
        // TCP 级
        IPPROTO_TCP => match optname {
            TCP_NODELAY => {
                let val = parse(&kern_optval)?;
                let opt = SetSocketOption::NoDelay(&val);
                sock.set_option(opt)
            }
            _ => {
                warn!(
                    "[sys_setsockopt] not available TCP protocol! level = {}, optname = {}",
                    level, optname
                );
                return Err(SysErrNo::ENOPROTOOPT);
            }
        },
        _ => {
            warn!(
                "[sys_setsockopt] unknown protocol! level = {}, optname = {}",
                level, optname
            );
            return Err(SysErrNo::ENOPROTOOPT);
        }
    }?;
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/setsockopt.2.html
pub fn sys_getsockopt(
    sockfd: usize,               // 文件描述符
    level: u32,                  // 协议，level 都会设为 SOL_SOCKET
    optname: u32,                // 设定或取出的套接字选项
    user_optval: *mut u8,        // 指向缓冲区的指针，用来指定或者返回选项的值
    user_optlen: *mut socklen_t, // 指向 optval 缓冲区长度的 value-result 指针
) -> SyscallRet {
    if user_optval.is_null() || user_optlen.is_null() {
        return Err(SysErrNo::EFAULT);
    }

    let task = current_task().unwrap();
    let process = &task.process;
    let fd_table = process.fd_table.clone();
    let optlen = {
        let memory_set = process.memory_set_arc();
        let mut optlen_bytes = [0u8; core::mem::size_of::<socklen_t>()];
        copy_from_user(&memory_set, user_optlen as usize, &mut optlen_bytes)?;
        socklen_t::from_ne_bytes(optlen_bytes)
    };
    drop(task);

    if optlen as usize > 1024 {
        return Err(SysErrNo::EINVAL);
    }

    let sock = fd_table.get(sockfd)?.socket()?;

    let mut kern_opt = Vec::new();
    let get_result: SysResult = match level {
        SOL_SOCKET => match optname {
            SO_REUSEADDR => {
                let mut val = false;
                let opt = GetSocketOption::ReuseAddress(&mut val);
                sock.get_option(opt)?;
                let val: i32 = if val { 1 } else { 0 };
                kern_opt.extend_from_slice(&val.to_ne_bytes());
                Ok(())
            }
            SO_SNDBUF => {
                let mut val = 0usize;
                let opt = GetSocketOption::SendBuffer(&mut val);
                sock.get_option(opt)?;
                kern_opt.extend_from_slice(&(val as i32).to_ne_bytes());
                Ok(())
            }
            SO_RCVBUF => {
                let mut val = 0usize;
                let opt = GetSocketOption::ReceiveBuffer(&mut val);
                sock.get_option(opt)?;
                kern_opt.extend_from_slice(&(val as i32).to_ne_bytes());
                Ok(())
            }
            SO_KEEPALIVE => {
                let mut val = false;
                let opt = GetSocketOption::KeepAlive(&mut val);
                sock.get_option(opt)?;
                let val: i32 = if val { 1 } else { 0 };
                kern_opt.extend_from_slice(&val.to_ne_bytes());
                Ok(())
            }
            SO_ERROR => {
                let mut val = 0i32;
                let opt = GetSocketOption::Error(&mut val);
                sock.get_option(opt)?;
                kern_opt.extend_from_slice(&val.to_ne_bytes());
                Ok(())
            }
            SO_DONTROUTE => {
                let mut val = false;
                let opt = GetSocketOption::DontRoute(&mut val);
                sock.get_option(opt)?;
                let val: i32 = if val { 1 } else { 0 };
                kern_opt.extend_from_slice(&val.to_ne_bytes());
                Ok(())
            }
            SO_RCVTIMEO => {
                let mut val = Duration::from_secs(0);
                let opt = GetSocketOption::ReceiveTimeout(&mut val);
                sock.get_option(opt)?;
                let sec = val.as_secs();
                let usec = val.subsec_micros() as u64;
                kern_opt.extend_from_slice(&sec.to_ne_bytes());
                kern_opt.extend_from_slice(&usec.to_ne_bytes());
                Ok(())
            }
            SO_SNDTIMEO => {
                let mut val = Duration::from_secs(0);
                let opt = GetSocketOption::SendTimeout(&mut val);
                sock.get_option(opt)?;
                let sec = val.as_secs();
                let usec = val.subsec_micros() as u64;
                kern_opt.extend_from_slice(&sec.to_ne_bytes());
                kern_opt.extend_from_slice(&usec.to_ne_bytes());
                Ok(())
            }
            _ => return Err(SysErrNo::ENOPROTOOPT),
        },
        // IP 级
        IPPROTO_IP => {
            if optname == IP_TTL {
                let mut val = 0u8;
                let opt = GetSocketOption::Ttl(&mut val);
                sock.get_option(opt)?;
                let val = val as i32;
                kern_opt.extend_from_slice(&val.to_ne_bytes());
                Ok(())
            } else if optname == IP_RECVERR || optname == IP_MTU_DISCOVER {
                let val: i32 = 0;
                kern_opt.extend_from_slice(&val.to_ne_bytes());
                Ok(())
            } else if optname == IP_MTU {
                let val: i32 = 1500;
                kern_opt.extend_from_slice(&val.to_ne_bytes());
                Ok(())
            } else {
                return Err(SysErrNo::ENOPROTOOPT);
            }
        }
        IPPROTO_IPV6 => {
            if optname == IPV6_V6ONLY {
                let val: i32 = 0;
                kern_opt.extend_from_slice(&val.to_ne_bytes());
                Ok(())
            } else {
                return Err(SysErrNo::ENOPROTOOPT);
            }
        }
        // TCP 级
        IPPROTO_TCP => {
            if optname == TCP_NODELAY {
                let mut val = false;
                let opt = GetSocketOption::NoDelay(&mut val);
                sock.get_option(opt)?;
                let val: i32 = if val { 1 } else { 0 };
                kern_opt.extend_from_slice(&val.to_ne_bytes());
                Ok(())
            } else if optname == TCP_MAXSEG {
                let mut val = 0usize;
                let opt = GetSocketOption::MaxSegment(&mut val);
                sock.get_option(opt)?;
                kern_opt.extend_from_slice(&(val as i32).to_ne_bytes());
                Ok(())
            } else if optname == TCP_INFO {
                let mut val: tcp_info = unsafe { core::mem::zeroed() };
                let opt = GetSocketOption::TcpInfo(&mut val);
                sock.get_option(opt)?;
                let bytes = unsafe {
                    core::slice::from_raw_parts(
                        &val as *const tcp_info as *const u8,
                        core::mem::size_of::<tcp_info>(),
                    )
                };
                kern_opt.extend_from_slice(bytes);
                Ok(())
            } else {
                return Err(SysErrNo::ENOPROTOOPT);
            }
        }
        _ => return Err(SysErrNo::ENOPROTOOPT),
    };
    get_result?;

    let copy_len = (optlen as usize).min(kern_opt.len());
    let task = current_task().unwrap();
    let process = &task.process;
    let memory_set = process.memory_set_arc();
    copy_to_user(&memory_set, user_optval as usize, &kern_opt[..copy_len])?;
    let actual_len = kern_opt.len() as socklen_t;
    copy_to_user(&memory_set, user_optlen as usize, &actual_len.to_ne_bytes())?;

    Ok(0)
}
