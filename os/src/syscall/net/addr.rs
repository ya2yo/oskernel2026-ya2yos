//! Wrapper for [`sockaddr`]. Using trait to convert between [`SocketAddr`] and
//! [`sockaddr`] types.

use alloc::vec::Vec;
use core::{
    mem::size_of,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6},
};

use crate::{
    mm::{copy_from_user, copy_to_user},
    task::current_task,
};
use crate::{
    net::{SocketAddrEx, UnixSocketAddr},
    utils::{SysErrNo, SysResult},
};
use linux_raw_sys::net::*;

/// Trait to extend [`SocketAddr`] and its variants with methods for reading
/// from and writing to user space.
pub trait SocketAddrExt: Sized {
    /// This method attempts to interpret the data pointed to by `addr` with the
    /// given `addrlen` as a valid socket address of the implementing type.
    fn read_from_user(addr: *const u8, addrlen: u32) -> SysResult<Self>;

    /// This method serializes the current socket address instance into the
    /// [`sockaddr`] structure pointed to by `addr` in user space.
    fn write_to_user(&self, addr: *mut u8, addrlen: &mut u32) -> SysResult<()>;

    /// Gets the address family of the socket address.
    #[allow(dead_code)]
    fn family(&self) -> u16;
}

fn read_family(addr: *const u8, addrlen: u32) -> SysResult<u16> {
    if size_of::<u16>() > addrlen as usize {
        return Err(SysErrNo::EINVAL);
    }
    let task = current_task().unwrap();
    let process = &task.process;
    let memory_set = process.get_locked_memory_set_read();
    let mut buf = [0u8; size_of::<u16>()];
    copy_from_user(&memory_set, addr as usize, &mut buf).map(|_| ())?;
    Ok(u16::from_ne_bytes(buf))
}
unsafe fn cast_to_slice<T>(value: &T) -> &[u8] {
    core::slice::from_raw_parts(value as *const T as *const u8, size_of::<T>())
}
fn fill_addr(addr: *mut u8, addrlen: &mut u32, data: &[u8]) -> SysResult<()> {
    let len = (*addrlen as usize).min(data.len());
    let task = current_task().unwrap();
    let process = &task.process;
    let memory_set = process.get_locked_memory_set_read();
    copy_to_user(&memory_set, addr as usize, &data[..len]).map(|_| ())?;
    *addrlen = data.len() as _;
    Ok(())
}

impl SocketAddrExt for SocketAddr {
    fn read_from_user(addr: *const u8, addrlen: u32) -> SysResult<Self> {
        match read_family(addr, addrlen)? as u32 {
            AF_INET => SocketAddrV4::read_from_user(addr, addrlen).map(Self::V4),
            AF_INET6 => SocketAddrV6::read_from_user(addr, addrlen).map(Self::V6),
            _ => Err(SysErrNo::EAFNOSUPPORT),
        }
    }

    fn write_to_user(&self, addr: *mut u8, addrlen: &mut u32) -> SysResult<()> {
        match self {
            SocketAddr::V4(v4) => v4.write_to_user(addr, addrlen),
            SocketAddr::V6(v6) => v6.write_to_user(addr, addrlen),
        }
    }

    fn family(&self) -> u16 {
        match self {
            SocketAddr::V4(v4) => v4.family(),
            SocketAddr::V6(v6) => v6.family(),
        }
    }
}

impl SocketAddrExt for SocketAddrV4 {
    fn read_from_user(addr: *const u8, addrlen: u32) -> SysResult<Self> {
        if addrlen != size_of::<sockaddr_in>() as u32 {
            return Err(SysErrNo::EINVAL);
        }
        let task = current_task().unwrap();
        let process = &task.process;
        let memory_set = process.get_locked_memory_set_read();
        let mut buf = [0u8; size_of::<sockaddr_in>()];
        copy_from_user(&memory_set, addr as usize, &mut buf).map(|_| ())?;
        let addr_in: sockaddr_in = unsafe { *buf.as_ptr().cast() };
        if addr_in.sin_family as u32 != AF_INET {
            return Err(SysErrNo::EAFNOSUPPORT);
        }

        Ok(SocketAddrV4::new(
            Ipv4Addr::from_bits(u32::from_be(addr_in.sin_addr.s_addr)),
            u16::from_be(addr_in.sin_port),
        ))
    }

    fn write_to_user(&self, addr: *mut u8, addrlen: &mut u32) -> SysResult<()> {
        let sockin_addr = sockaddr_in {
            sin_family: AF_INET as _,
            sin_port: self.port().to_be(),
            sin_addr: in_addr {
                s_addr: u32::from_ne_bytes(self.ip().octets()),
            },
            __pad: [0_u8; 8],
        };
        fill_addr(addr, addrlen, unsafe { cast_to_slice(&sockin_addr) })
    }

    fn family(&self) -> u16 {
        AF_INET as u16
    }
}

impl SocketAddrExt for SocketAddrV6 {
    fn read_from_user(addr: *const u8, addrlen: u32) -> SysResult<Self> {
        if addrlen != size_of::<sockaddr_in6>() as u32 {
            return Err(SysErrNo::EINVAL);
        }
        let task = current_task().unwrap();
        let process = &task.process;
        let memory_set = process.get_locked_memory_set_read();
        let mut buf = [0u8; size_of::<sockaddr_in6>()];
        copy_from_user(&memory_set, addr as usize, &mut buf).map(|_| ())?;
        let addr_in6: sockaddr_in6 = unsafe { *buf.as_ptr().cast() };
        if addr_in6.sin6_family as u32 != AF_INET6 {
            return Err(SysErrNo::EAFNOSUPPORT);
        }

        Ok(SocketAddrV6::new(
            Ipv6Addr::from(unsafe { addr_in6.sin6_addr.in6_u.u6_addr8 }),
            u16::from_be(addr_in6.sin6_port),
            u32::from_be(addr_in6.sin6_flowinfo),
            addr_in6.sin6_scope_id,
        ))
    }

    fn write_to_user(&self, addr: *mut u8, addrlen: &mut u32) -> SysResult<()> {
        let sockin_addr = sockaddr_in6 {
            sin6_family: AF_INET6 as _,
            sin6_port: self.port().to_be(),
            sin6_flowinfo: self.flowinfo().to_be(),
            sin6_addr: in6_addr {
                in6_u: linux_raw_sys::net::in6_addr__bindgen_ty_1 {
                    u6_addr8: self.ip().octets(),
                },
            },
            sin6_scope_id: self.scope_id(),
        };
        fill_addr(addr, addrlen, unsafe { cast_to_slice(&sockin_addr) })
    }

    fn family(&self) -> u16 {
        AF_INET6 as u16
    }
}

impl SocketAddrExt for SocketAddrEx {
    fn read_from_user(addr: *const u8, addrlen: u32) -> SysResult<Self> {
        match read_family(addr, addrlen)? as u32 {
            AF_INET | AF_INET6 => SocketAddr::read_from_user(addr, addrlen).map(Self::Ip),
            AF_UNIX => UnixSocketAddr::read_from_user(addr, addrlen).map(Self::Unix),
            _ => Err(SysErrNo::EAFNOSUPPORT),
        }
    }

    fn write_to_user(&self, addr: *mut u8, addrlen: &mut u32) -> SysResult<()> {
        match self {
            SocketAddrEx::Ip(ip_addr) => ip_addr.write_to_user(addr, addrlen),
            SocketAddrEx::Unix(unix_addr) => unix_addr.write_to_user(addr, addrlen),
        }
    }

    fn family(&self) -> u16 {
        match self {
            SocketAddrEx::Ip(_) => AF_INET as u16,
            SocketAddrEx::Unix(_) => AF_UNIX as u16,
        }
    }
}

impl SocketAddrExt for UnixSocketAddr {
    fn read_from_user(addr: *const u8, addrlen: u32) -> SysResult<Self> {
        let family_size = size_of::<u16>();
        if addrlen < family_size as u32 {
            return Err(SysErrNo::EINVAL);
        }
        let path_len = (addrlen as usize).saturating_sub(family_size).min(108);
        if path_len == 0 {
            return Ok(UnixSocketAddr::Unnamed);
        }

        let task = current_task().unwrap();
        let process = &task.process;
        let memory_set = process.get_locked_memory_set_read();
        let mut path = [0u8; 108];
        copy_from_user(
            &memory_set,
            addr as usize + family_size,
            &mut path[..path_len],
        )
        .map(|_| ())?;

        if path[0] == 0 {
            if path_len == 1 {
                return Ok(UnixSocketAddr::Unnamed);
            }
            return Ok(UnixSocketAddr::Abstract(path[1..path_len].to_vec()));
        }

        let end = path[..path_len]
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(path_len);
        let path = core::str::from_utf8(&path[..end]).map_err(|_| SysErrNo::EINVAL)?;
        Ok(UnixSocketAddr::Path(path.into()))
    }

    fn write_to_user(&self, addr: *mut u8, addrlen: &mut u32) -> SysResult<()> {
        let family_size = size_of::<u16>();
        let mut data = [0u8; 110];
        data[..family_size].copy_from_slice(&(AF_UNIX as u16).to_ne_bytes());
        let used = match self {
            UnixSocketAddr::Unnamed => family_size,
            UnixSocketAddr::Abstract(name) => {
                let len = name.len().min(107);
                data[family_size] = 0;
                data[family_size + 1..family_size + 1 + len].copy_from_slice(&name[..len]);
                family_size + 1 + len
            }
            UnixSocketAddr::Path(path) => {
                let bytes = path.as_bytes();
                let len = bytes.len().min(107);
                data[family_size..family_size + len].copy_from_slice(&bytes[..len]);
                data[family_size + len] = 0;
                family_size + len + 1
            }
        };

        let copy_len = (*addrlen as usize).min(used);
        let task = current_task().unwrap();
        let process = &task.process;
        let memory_set = process.get_locked_memory_set_read();
        copy_to_user(&memory_set, addr as usize, &data[..copy_len]).map(|_| ())?;
        *addrlen = used as u32;
        Ok(())
    }

    fn family(&self) -> u16 {
        AF_UNIX as u16
    }
}
