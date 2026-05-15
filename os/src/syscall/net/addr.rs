//! Wrapper for [`sockaddr`]. Using trait to convert between [`SocketAddr`] and
//! [`sockaddr`] types.

use alloc::{slice, vec::Vec};
use core::{
    mem::size_of,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6},
};

use crate::{net::SocketAddrEx, syscall::net::{AF_INET, AF_INET6, AF_UNIX}, utils::{SysErrNo, SysResult}};
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
    let family = unsafe{*addr.cast::<u16>()};
    Ok(family)
}
unsafe fn cast_to_slice<T>(value: &T) -> &[u8] {
    unsafe { core::slice::from_raw_parts(value as *const T as *const u8, size_of::<T>()) }
}
fn fill_addr(addr: *mut u8, addrlen: &mut u32, data: &[u8]) -> SysResult<()> {
    let len = (*addrlen as usize).min(data.len());
    unsafe {slice::from_raw_parts_mut(addr, len)}.copy_from_slice(&data[..len]);
    *addrlen = data.len() as _;
    Ok(())
}

impl SocketAddrExt for SocketAddr {
    fn read_from_user(addr: * const u8, addrlen: u32) -> SysResult<Self> {
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
        let addr_in = unsafe{*addr.cast::<sockaddr_in>()};
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
        let addr_in6 = unsafe{ *addr.cast::<sockaddr_in6>() };
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
            // AF_UNIX => UnixSocketAddr::read_from_user(addr, addrlen).map(Self::Unix),
            _ => Err(SysErrNo::EAFNOSUPPORT),
        }
    }

    fn write_to_user(&self, addr: *mut u8, addrlen: &mut u32) -> SysResult<()> {
        match self {
            SocketAddrEx::Ip(ip_addr) => ip_addr.write_to_user(addr, addrlen),
        }
    }

    fn family(&self) -> u16 {
        AF_INET as u16
    }
}
