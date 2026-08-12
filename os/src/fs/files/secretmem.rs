//! Anonymous file object used by `memfd_secret(2)`.
use alloc::{sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;

use super::super::{File, Kstat, StMode};
use crate::mm::UserBuffer;
use crate::syscall::PollEvents;
use crate::utils::{SysErrNo, SyscallRet};

static NEXT_SECRET_INO: AtomicUsize = AtomicUsize::new(0x7100_0000);

pub struct SecretMemFile {
    inner: Mutex<SecretMemFileInner>,
    ino: usize,
}

struct SecretMemFileInner {
    data: Vec<u8>,
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
