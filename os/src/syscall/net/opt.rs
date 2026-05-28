use super::consts::*;
use crate::{
    fs::Socket,
    mm::copy_from_user,
    net::options::{Configurable, GetSocketOption, SetSocketOption},
    task::{current_task, tid_to_task::task_num},
    utils::{SysErrNo, SysResult, SyscallRet},
};
use alloc::sync::Arc;
use alloc::vec;
use linux_raw_sys::net::{
    IP_MSFILTER, IP_MULTICAST_IF, IP_RETOPTS, IP_TTL, MCAST_JOIN_GROUP, SOL_SOCKET, SO_KEEPALIVE,
    SO_RCVBUF, SO_RCVTIMEO, SO_REUSEADDR, SO_SNDBUF, SO_SNDTIMEO, TCP_NODELAY,
};
use log::{debug, warn};

use core::{mem::size_of, time::Duration};

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
    // debug!(
    //     "sys_setsockopt <= fd: {}, level: {}, optname: {}, optval: {:?}, optlen: {}",
    //     sockfd, level, optname, user_optval, optlen
    // );
    // bool compat = in_compat_syscall();
    // let compat: bool = false;// 目前只在64位上运行
    let task = current_task().unwrap();
    // debug!("strong count: {}", Arc::strong_count(&task));
    let fd_table = task.get_fd_table();
    drop(task);
    let sock = fd_table.get(sockfd)?.socket()?;
    if optlen > 1024 {
        return Err(SysErrNo::EINVAL);
    }
    let mut kern_optval = vec![0; optlen as usize];
    let task = current_task().unwrap();
    let process = task.process.inner_lock();
    let memory_set = process.get_locked_memory_set_read();
    copy_from_user(&memory_set, user_optval as usize, &mut kern_optval)?;
    drop(memory_set);
    drop(process);
    drop(task);
    match level {
        SOL_SOCKET => match optname {
            SO_REUSEADDR => {
                let val = parse(&kern_optval)?;
                let opt = SetSocketOption::ReuseAddress(&val);
                sock.set_option(opt)
            }
            SO_SNDBUF => {
                let val = parse(&kern_optval)?;
                let opt = SetSocketOption::SendBuffer(&val);
                sock.set_option(opt)
            }
            SO_RCVBUF => {
                let val = parse(&kern_optval)?;
                let opt = SetSocketOption::ReceiveBuffer(&val);
                sock.set_option(opt)
            }
            SO_KEEPALIVE => {
                let val = parse(&kern_optval)?;
                let opt = SetSocketOption::KeepAlive(&val);
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
            _ => return Err(SysErrNo::ENOPROTOOPT),
        },
        // IP 级
        IPPROTO_IP => match optname {
            IP_TTL => {
                let val: i32 = parse(&kern_optval)?;
                let val = val as u8;
                let opt = SetSocketOption::Ttl(&val);
                sock.set_option(opt)
            }
            MCAST_JOIN_GROUP | IP_MULTICAST_IF => Ok(()),
            _ => return Err(SysErrNo::ENOPROTOOPT),
        },
        // TCP 级
        IPPROTO_TCP => match optname {
            TCP_NODELAY => {
                let val = parse(&kern_optval)?;
                let opt = SetSocketOption::NoDelay(&val);
                sock.set_option(opt)
            }
            _ => return Err(SysErrNo::ENOPROTOOPT),
        },
        _ => return Err(SysErrNo::ENOPROTOOPT),
    };
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/setsockopt.2.html
pub fn sys_getsockopt(
    sockfd: usize,          // 文件描述符
    level: u32,             // 协议，level 都会设为 SOL_SOCKET
    optname: u32,           // 设定或取出的套接字选项
    user_optval: *const u8, // 指向缓冲区的指针，用来指定或者返回选项的值
    optlen: u32,            // 由 optval 所指向的缓冲区空间大小（字节数）
) -> SyscallRet {
    // debug!(
    //     "[getsockopt]syscall sockfd: {}, level: {}, optname: {}, user_optval: {}, optlen: {}",
    //     sockfd, level, optname, user_optval as usize, optlen
    // );
    let task = current_task().unwrap();
    let fd_table = task.get_fd_table();
    drop(task);
    let sock = fd_table.get(sockfd)?.socket()?;
    let mut kern_opt = vec![0; optlen as usize];
    match level {
        SOL_SOCKET => match optname {
            SO_REUSEADDR => {
                let mut val = parse(&kern_opt)?;
                let opt = GetSocketOption::ReuseAddress(&mut val);
                sock.get_option(opt)
            }
            SO_SNDBUF => {
                let mut val = parse(&kern_opt)?;
                let opt = GetSocketOption::SendBuffer(&mut val);
                sock.get_option(opt)
            }
            SO_RCVBUF => {
                let mut val = parse(&kern_opt)?;
                let opt = GetSocketOption::ReceiveBuffer(&mut val);
                sock.get_option(opt)
            }
            SO_KEEPALIVE => {
                let mut val = parse(&kern_opt)?;
                let opt = GetSocketOption::KeepAlive(&mut val);
                sock.get_option(opt)
            }
            SO_RCVTIMEO => {
                let mut val = parse(&kern_opt)?;
                let opt = GetSocketOption::ReceiveTimeout(&mut val);
                sock.get_option(opt)
            }
            SO_SNDTIMEO => {
                let mut val = parse(&kern_opt)?;
                let opt = GetSocketOption::SendTimeout(&mut val);
                sock.get_option(opt)
            }
            _ => return Err(SysErrNo::ENOPROTOOPT),
        },
        // IP 级
        IPPROTO_IP => {
            if optname == IP_TTL {
                let mut val = kern_opt[0];
                let opt = GetSocketOption::Ttl(&mut val);
                sock.get_option(opt)
            } else {
                return Err(SysErrNo::ENOPROTOOPT);
            }
        }
        // TCP 级
        IPPROTO_TCP => {
            if optname == TCP_NODELAY {
                let mut val = parse(&mut kern_opt)?;
                let opt = GetSocketOption::NoDelay(&mut val);
                sock.get_option(opt)
            } else {
                return Err(SysErrNo::ENOPROTOOPT);
            }
        }
        _ => return Err(SysErrNo::ENOPROTOOPT),
    };

    Ok(0)
}
