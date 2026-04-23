use alloc::{borrow::Cow, format, sync::Arc};
use core::{ffi::c_int, ops::Deref, task::Context};

use crate::utils::{SysErrNo, SysResult};
use crate::net::{
    RecvOptions, SendOptions, Socket as SocketInner, SocketOps,
    options::{Configurable, GetSocketOption, SetSocketOption},
};
// use axpoll::{IoEvents, Pollable};
use crate::syscall::PollEvents;
pub const S_IFSOCK: u32 = 49152;

use super::super::{File, Kstat};
pub type IoDst<'a> = &'a mut [u8]; // 用于 Read，数据写入这里
pub type IoSrc<'a> = &'a [u8];     // 用于 Write，从这里读出数据

pub struct Socket(pub SocketInner);

impl Deref for Socket {
    type Target = SocketInner;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl File for Socket {
    fn read(&self, dst: &mut IoDst) -> SysResult<usize> {
        self.recv(dst, RecvOptions::default())
    }

    fn write(&self, src: &mut IoSrc) -> SysResult<usize> {
        self.send(src, SendOptions::default())
    }

    fn fstat(&self) -> SysResult<Kstat> {
        let mode = S_IFSOCK | 0o666; 
        Ok(Kstat {
            st_mode: mode as u32,
            st_nlink: 1,          // 即使是虚拟文件，链接数也至少为 1
            st_size: 0,           // Socket 大小通常返回 0
            st_blksize: 4096,     // 标准块大小
            st_blocks: 0,         // 未占用磁盘块
            // 如果有条件，可以填充时间戳，否则保持 Default(0)
            ..Kstat::default()
        })
    }

    // fn nonblocking(&self) -> bool {
    //     let mut result = false;
    //     self.get_option(GetSocketOption::NonBlocking(&mut result))
    //         .unwrap();
    //     result
    // }

    // fn set_nonblocking(&self, nonblocking: bool) -> SysResult<()> {
    //     self.0
    //         .set_option(SetSocketOption::NonBlocking(&nonblocking))
    // }

    // fn path(&self) -> Cow<'_, str> {
    //     format!("socket:[{}]", self as *const _ as usize).into()
    // }

    // fn from_fd(fd: c_int) -> SysResult<Arc<Self>>
    // where
    //     Self: Sized + 'static,
    // {
    //     get_file_like(fd)?
    //         .downcast_arc()
    //         .map_err(|_| SysErrNo::ENOTSOCK)
    // }

    fn poll(&self, events: PollEvents) -> PollEvents {
        self.0.poll(events)
    }
    fn register(&self, context: &mut Context<'_>, events: PollEvents) {
        self.0.register(context, events);
    }

}
