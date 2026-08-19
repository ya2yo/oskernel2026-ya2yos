//! 套接字文件对象适配层。
//!
//! [`Socket`] 将网络子系统中的 [`SocketInner`] 包装成文件系统能够识别的
//! [`File`]。文件接口中的读、写、非阻塞和轮询操作都直接委托给网络对象，
//! 因而不会在文件层重复维护套接字状态。套接字没有磁盘内容，`fstat` 和
//! `path` 只提供符合 proc 风格和 socket 文件类型的描述信息。

use alloc::{borrow::Cow, format, sync::Arc};
use core::{ffi::c_int, ops::Deref, task::Context};

use crate::mm::UserBuffer;
use crate::net::{
    options::{Configurable, GetSocketOption, SetSocketOption},
    RecvOptions, SendOptions, Socket as SocketInner, SocketOps,
};
use crate::task::current_task;
use crate::utils::{SysErrNo, SysResult};
// use axpoll::{IoEvents, Pollable};
use crate::syscall::PollEvents;
pub const S_IFSOCK: u32 = 49152;

use super::super::{File, Kstat};
pub type IoDst<'a> = &'a mut [u8]; // 用于 Read，数据写入这里
pub type IoSrc<'a> = &'a [u8]; // 用于 Write，从这里读出数据

/// 网络套接字在文件描述符表中的包装对象。
///
/// 内部值负责协议状态与收发缓冲区，本层仅实现文件接口适配。
pub struct Socket(pub SocketInner);

impl Deref for Socket {
    type Target = SocketInner;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Socket {
    pub fn from_fd(fd: usize) -> SysResult<Arc<Self>>
    where
        Self: Sized + 'static,
    {
        let task = current_task().unwrap();
        let proc_inner = &task.process;
        proc_inner.fd_table.get(fd)?.socket()
    }
}

impl File for Socket {
    fn read(&self, dst: UserBuffer) -> SysResult<usize> {
        self.recv(dst, RecvOptions::default())
    }

    fn write(&self, src: UserBuffer) -> SysResult<usize> {
        self.send(src, SendOptions::default())
    }

    fn fstat(&self) -> Kstat {
        let mode = S_IFSOCK | 0o666;
        Kstat {
            st_mode: mode,
            st_nlink: 1,      // 即使是虚拟文件，链接数也至少为 1
            st_size: 0,       // Socket 大小通常返回 0
            st_blksize: 4096, // 标准块大小
            st_blocks: 0,     // 未占用磁盘块
            // 如果有条件，可以填充时间戳，否则保持 Default(0)
            ..Kstat::default()
        }
    }

    fn nonblocking(&self) -> bool {
        let mut result = false;
        self.get_option(GetSocketOption::NonBlocking(&mut result))
            .unwrap();
        result
    }

    fn set_nonblocking(&self, nonblocking: bool) -> SysResult<()> {
        self.0
            .set_option(SetSocketOption::NonBlocking(&nonblocking))
    }

    fn path(&self) -> Cow<'_, str> {
        format!("socket:[{}]", self as *const _ as usize).into()
    }

    fn poll(&self, events: PollEvents) -> PollEvents {
        self.0.poll(events)
    }
    fn register(&self, context: &mut Context<'_>, events: PollEvents) {
        self.0.register(context, events);
    }
}
