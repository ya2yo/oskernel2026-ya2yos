//! `memfd_secret(2)` 创建的匿名内存文件对象。
//!
//! 该实现以受内核锁保护的字节向量保存内容，并为每个打开对象维护独立文件
//! 偏移。它不创建目录项，主要用于为 secret memory syscall 提供可读写、可
//! 截断和可 seek 的 `File` 接口；真正的页隔离策略由内存管理层负责。
use alloc::{sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;

use super::super::{File, Kstat, StMode};
use crate::mm::UserBuffer;
use crate::syscall::PollEvents;
use crate::utils::{SysErrNo, SyscallRet};

static NEXT_SECRET_INO: AtomicUsize = AtomicUsize::new(0x7100_0000);

/// `memfd_secret` 返回的匿名文件描述符对象。
///
/// `inner` 保护文件内容和描述符偏移；`ino` 是仅用于 `fstat` 的稳定匿名 inode
/// 编号，不对应磁盘上的目录项。
pub struct SecretMemFile {
    /// 内容与当前读写偏移。
    inner: Mutex<SecretMemFileInner>,
    /// 自动分配的匿名 inode 编号。
    ino: usize,
}

/// secretmem 文件的可变状态。
struct SecretMemFileInner {
    /// 文件字节内容。
    data: Vec<u8>,
    /// 下次 read/write 使用的文件偏移。
    offset: usize,
}

impl SecretMemFile {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(SecretMemFileInner {
                data: Vec::new(),
                offset: 0,
            }),
            ino: NEXT_SECRET_INO.fetch_add(1, Ordering::Relaxed),
        })
    }
}

impl File for SecretMemFile {
    /// secretmem 始终允许读取；实际 EOF 由内存内容长度决定。
    fn readable(&self) -> bool {
        true
    }
    fn writable(&self) -> bool {
        true
    }

    fn read(&self, mut buf: UserBuffer) -> SyscallRet {
        let mut inner = self.inner.lock();
        if inner.offset >= inner.data.len() {
            return Ok(0);
        }
        let end = inner.offset.saturating_add(buf.len()).min(inner.data.len());
        let read_len = buf.write(&inner.data[inner.offset..end]);
        inner.offset += read_len;
        Ok(read_len)
    }

    fn write(&self, buf: UserBuffer) -> SyscallRet {
        let bytes = buf
            .buffers
            .iter()
            .flat_map(|slice| slice.iter().copied())
            .collect::<Vec<_>>();
        let mut inner = self.inner.lock();
        let end = inner
            .offset
            .checked_add(bytes.len())
            .ok_or(SysErrNo::EFBIG)?;
        if end > inner.data.len() {
            inner.data.resize(end, 0);
        }
        let offset = inner.offset;
        inner.data[offset..end].copy_from_slice(&bytes);
        inner.offset = end;
        Ok(bytes.len())
    }

    fn truncate(&self, size: usize) -> SyscallRet {
        let mut inner = self.inner.lock();
        inner.data.resize(size, 0);
        if inner.offset > size {
            inner.offset = size;
        }
        Ok(0)
    }

    fn fstat(&self) -> Kstat {
        let inner = self.inner.lock();
        Kstat {
            st_ino: self.ino,
            st_mode: StMode::FREG.bits() | 0o600,
            st_nlink: 0,
            st_size: inner.data.len() as isize,
            st_blksize: 512,
            st_blocks: ((inner.data.len() + 511) / 512) as isize,
            ..Kstat::default()
        }
    }

    fn lseek(&self, offset: isize, whence: usize) -> SyscallRet {
        let mut inner = self.inner.lock();
        let base = match whence {
            0 => 0isize,
            1 => inner.offset as isize,
            2 => inner.data.len() as isize,
            _ => return Err(SysErrNo::EINVAL),
        };
        let new_offset = base.checked_add(offset).ok_or(SysErrNo::EINVAL)?;
        if new_offset < 0 {
            return Err(SysErrNo::EINVAL);
        }
        inner.offset = new_offset as usize;
        Ok(inner.offset)
    }

    fn poll(&self, events: PollEvents) -> PollEvents {
        let mut revents = PollEvents::empty();
        if events.contains(PollEvents::IN) {
            revents |= PollEvents::IN;
        }
        if events.contains(PollEvents::OUT) {
            revents |= PollEvents::OUT;
        }
        revents
    }
}
