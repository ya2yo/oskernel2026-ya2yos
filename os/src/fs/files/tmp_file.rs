//! In-memory anonymous regular file used for `O_TMPFILE`.
//!
//! Linux `O_TMPFILE` opens a regular file that is not linked into any
//! directory yet. The directory path passed to `openat` only selects the
//! filesystem/context; the returned fd owns the file until it is closed or
//! later materialized with `linkat("/proc/self/fd/<fd>", ...)`. This object
//! therefore stores data, offset, mode and owner in memory, and intentionally
//! does not create an ext4 directory entry.
use alloc::{sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;

use super::super::{File, Kstat, StMode};
use crate::mm::UserBuffer;
use crate::syscall::PollEvents;
use crate::utils::{SysErrNo, SyscallRet};

static NEXT_TMP_INO: AtomicUsize = AtomicUsize::new(0x7000_0000);

/// Anonymous regular file returned by `openat(..., O_TMPFILE, ...)`.
///
/// It deliberately implements only fd-visible state. A real filesystem inode is
/// created later only if userspace links `/proc/self/fd/<fd>` into a directory.
pub struct TmpFile {
    readable: bool,
    writable: bool,
    mode: u32,
    uid: u32,
    gid: u32,
    ino: usize,
    inner: Mutex<TmpFileInner>,
}

struct TmpFileInner {
    data: Vec<u8>,
    offset: usize,
}

impl TmpFile {
    pub fn new(readable: bool, writable: bool, mode: u32, uid: u32, gid: u32) -> Arc<Self> {
        Arc::new(Self {
            readable,
            writable,
            mode,
            uid,
            gid,
            ino: NEXT_TMP_INO.fetch_add(1, Ordering::Relaxed),
            inner: Mutex::new(TmpFileInner {
                data: Vec::new(),
                offset: 0,
            }),
        })
    }
}

impl File for TmpFile {
    fn readable(&self) -> bool {
        self.readable
    }

    fn writable(&self) -> bool {
        self.writable
    }

    fn read(&self, mut buf: UserBuffer) -> SyscallRet {
        let mut inner = self.inner.lock();
        if inner.offset >= inner.data.len() {
            return Ok(0);
        }
        let end = (inner.offset + buf.len()).min(inner.data.len());
        let read_len = buf.write(&inner.data[inner.offset..end]);
        inner.offset += read_len;
        Ok(read_len)
    }

    fn write(&self, buf: UserBuffer) -> SyscallRet {
        let bytes = buf.read_to_vec();
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
            st_mode: StMode::FREG.bits() | (self.mode & 0o7777),
            // Anonymous tmpfiles have no directory entry until linkat()
            // materializes them, so Linux reports link count 0 here.
            st_nlink: 0,
            st_uid: self.uid,
            st_gid: self.gid,
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
        if events.contains(PollEvents::IN) && self.readable {
            revents |= PollEvents::IN;
        }
        if events.contains(PollEvents::OUT) && self.writable {
            revents |= PollEvents::OUT;
        }
        revents
    }
}

trait UserBufferExt {
    fn read_to_vec(&self) -> Vec<u8>;
}

impl UserBufferExt for UserBuffer {
    /// Snapshot user buffer slices before taking the file lock, so writes can
    /// resize the backing Vec without keeping references into UserBuffer.
    fn read_to_vec(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        for slice in self.buffers.iter() {
            bytes.extend_from_slice(slice);
        }
        bytes
    }
}
