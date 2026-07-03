use crate::{
    fs::{FsIndex, Kstat, FILE_PAGE_CACHE, SEEK_CUR, SEEK_END, SEEK_SET},
    mm::{copy_from_user, copy_to_user, MemorySet, UserBuffer},
    syscall::PollEvents,
    utils::{SysErrNo, SyscallRet},
};

use super::super::{File, Inode};
use alloc::{collections::BTreeMap, string::String, sync::Arc};
use linux_raw_sys::{
    general::FS_IMMUTABLE_FL,
    ioctl::{FS_IOC32_GETFLAGS, FS_IOC32_SETFLAGS, FS_IOC_GETFLAGS, FS_IOC_SETFLAGS},
};
use spin::{Lazy, Mutex};

static WRITE_OPEN_COUNTS: Lazy<Mutex<BTreeMap<String, usize>>> =
    Lazy::new(|| Mutex::new(BTreeMap::new()));
static FILE_FLAGS: Lazy<Mutex<BTreeMap<String, u32>>> = Lazy::new(|| Mutex::new(BTreeMap::new()));

fn register_write_open(path: &str) {
    let mut counts = WRITE_OPEN_COUNTS.lock();
    *counts.entry(String::from(path)).or_insert(0) += 1;
}

fn unregister_write_open(path: &str) {
    let mut counts = WRITE_OPEN_COUNTS.lock();
    if let Some(count) = counts.get_mut(path) {
        *count -= 1;
        if *count == 0 {
            counts.remove(path);
        }
    }
}

fn get_file_flags(path: &str) -> u32 {
    FILE_FLAGS.lock().get(path).copied().unwrap_or(0)
}

fn set_file_flags(path: &str, flags: u32) {
    let mut attrs = FILE_FLAGS.lock();
    if flags == 0 {
        attrs.remove(path);
    } else {
        attrs.insert(String::from(path), flags);
    }
}

fn is_immutable_path(path: &str) -> bool {
    get_file_flags(path) & FS_IMMUTABLE_FL != 0
}

/// “普通”的文件类
/// 区别于管道、设备、套接字等特殊文件
pub struct OSFile {
    readable: bool, // 该文件是否允许通过 sys_read 进行读
    writable: bool, // 该文件是否允许通过 sys_write 进行写
    append: bool,   // O_APPEND: 每次 write 前都定位到文件末尾
    pub inode: Arc<dyn Inode>,
    write_path: Option<String>,
    inner: Mutex<OSFileInner>,
}
struct OSFileInner {
    offset: usize, // 偏移量
}

impl OSFile {
    pub fn new(readable: bool, writable: bool, append: bool, inode: Arc<dyn Inode>) -> Self {
        let write_path = if writable {
            let path = inode.path();
            register_write_open(&path);
            Some(path)
        } else {
            None
        };
        Self {
            readable,
            writable,
            append,
            inode,
            write_path,
            inner: Mutex::new(OSFileInner { offset: 0 }),
        }
    }

    pub fn is_write_open_path(path: &str) -> bool {
        WRITE_OPEN_COUNTS.lock().get(path).copied().unwrap_or(0) != 0
    }

    pub fn is_immutable_path(path: &str) -> bool {
        is_immutable_path(path)
    }

    pub fn set_offset(&self, offset: usize) {
        self.inner.lock().offset = offset;
    }

    /// Directory streams store opaque cookies here too, so callers such as
    /// getdents64 must read it directly instead of routing through lseek().
    pub fn offset(&self) -> usize {
        self.inner.lock().offset
    }
}

impl Drop for OSFile {
    fn drop(&mut self) {
        if let Some(path) = self.write_path.as_deref() {
            unregister_write_open(path);
        }
    }
}

// 为 OSFile 实现 File Trait
impl File for OSFile {
    fn readable(&self) -> bool {
        self.readable
    }

    fn writable(&self) -> bool {
        self.writable
    }

    fn read(&self, mut buf: UserBuffer) -> SyscallRet {
        let mut inner = self.inner.lock();
        let mut total_read_size = 0usize;

        if self.inode.size() <= inner.offset {
            //读取位置超过文件大小，返回结果为EOF
            return Ok(0);
        }

        // 这边要使用 iter_mut()，因为要将数据写入
        for slice in buf.buffers.iter_mut() {
            let read_size = self.inode.read_at(inner.offset, slice)?;
            if read_size == 0 {
                break;
            }
            inner.offset += read_size;
            total_read_size += read_size;
        }
        Ok(total_read_size)
    }

    fn write(&self, buf: UserBuffer) -> SyscallRet {
        let mut inner = self.inner.lock();
        if is_immutable_path(&self.inode.path()) {
            return Err(SysErrNo::EPERM);
        }
        if self.append {
            inner.offset = self.inode.size();
        }
        let mut total_write_size = 0usize;
        for slice in buf.buffers.iter() {
            let write_offset = inner.offset;
            let write_size = self.inode.write_at(inner.offset, *slice)?;
            assert_eq!(write_size, slice.len());
            FILE_PAGE_CACHE.invalidate_path_range(&self.inode.path(), write_offset, write_size);
            inner.offset += write_size;
            total_write_size += write_size;
        }
        Ok(total_write_size)
    }

    fn fstat(&self) -> Kstat {
        self.inode.fstat()
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
    fn lseek(&self, offset: isize, whence: usize) -> SyscallRet {
        if whence > 2 {
            return Err(SysErrNo::EINVAL);
        }
        let inode_type =
            FsIndex::special_node_type(&self.inode.path()).unwrap_or_else(|| self.inode.types());
        if inode_type.is_fifo() || inode_type.is_socket() {
            return Err(SysErrNo::ESPIPE);
        }
        let mut inner = self.inner.lock();
        if whence == SEEK_SET {
            if offset < 0 {
                return Err(SysErrNo::EINVAL);
            }
            inner.offset = offset as usize;
        } else if whence == SEEK_CUR {
            let newoff = inner.offset as isize + offset;
            if newoff < 0 {
                return Err(SysErrNo::EINVAL);
            }
            inner.offset = newoff as usize;
        } else if whence == SEEK_END {
            let newoff = self.inode.size() as isize + offset;
            if newoff < 0 {
                return Err(SysErrNo::EINVAL);
            }
            inner.offset = newoff as usize;
        }
        Ok(inner.offset)
    }

    fn ioctl(&self, cmd: u32, arg: usize, memory_set: &MemorySet) -> SyscallRet {
        match cmd {
            FS_IOC_GETFLAGS | FS_IOC32_GETFLAGS => {
                let flags = get_file_flags(&self.inode.path());
                copy_to_user(memory_set, arg, unsafe {
                    core::slice::from_raw_parts(
                        &flags as *const u32 as *const u8,
                        core::mem::size_of::<u32>(),
                    )
                })?;
                Ok(0)
            }
            FS_IOC_SETFLAGS | FS_IOC32_SETFLAGS => {
                let mut flags: u32 = 0;
                copy_from_user(memory_set, arg, unsafe {
                    core::slice::from_raw_parts_mut(
                        &mut flags as *mut u32 as *mut u8,
                        core::mem::size_of::<u32>(),
                    )
                })?;
                set_file_flags(&self.inode.path(), flags);
                Ok(0)
            }
            _ => Err(SysErrNo::ENOTTY),
        }
    }
}
