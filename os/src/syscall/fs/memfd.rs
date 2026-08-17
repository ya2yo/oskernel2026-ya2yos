//! `memfd_create(2)` and `memfd_secret(2)` syscall frontends.
use crate::{
    fs::{FileClass, FileDescriptor, OpenFlags, SecretMemFile, TmpFile},
    mm::{if_bad_address, read_user_cstr_with_limit},
    task::current_task,
    utils::{SysErrNo, SyscallRet},
};
use linux_raw_sys::general::{MFD_ALLOW_SEALING, MFD_CLOEXEC, MFD_HUGETLB, MFD_NOEXEC_SEAL};

pub fn sys_memfd_create(name: *const u8, flags: u32) -> SyscallRet {
    const MFD_KNOWN_FLAGS: u32 = MFD_CLOEXEC | MFD_ALLOW_SEALING | MFD_HUGETLB | MFD_NOEXEC_SEAL;
    const MAX_MEMFD_NAME: usize = 249;
    if flags & !MFD_KNOWN_FLAGS != 0 {
        return Err(SysErrNo::EINVAL);
    }
    if flags & MFD_HUGETLB != 0 {
        return Err(SysErrNo::EINVAL);
    }
    if name.is_null() || if_bad_address(name as usize) {
        return Err(SysErrNo::EFAULT);
    }
    let task = current_task().unwrap();
    let memory_set = task.process.memory_set_arc();
    read_user_cstr_with_limit(&memory_set, name, MAX_MEMFD_NAME + 1).map_err(|err| {
        if err == SysErrNo::E2BIG {
            SysErrNo::ENAMETOOLONG
        } else {
            err
        }
    })?;
    let inner = task.inner_lock();
    let (uid, gid) = (inner.effective_uid, inner.effective_gid);
    drop(inner);
    let fd = task.process.fd_table.alloc_fd()?;
    let open_flags = if flags & MFD_CLOEXEC != 0 {
        OpenFlags::O_CLOEXEC
    } else {
        OpenFlags::empty()
    };
    task.process.fd_table.set(
        fd,
        FileDescriptor::new(
            open_flags,
            FileClass::Abs(TmpFile::new_memfd(
                true,
                true,
                0o666,
                uid,
                gid,
                flags & (MFD_ALLOW_SEALING | MFD_NOEXEC_SEAL) != 0,
                flags & MFD_NOEXEC_SEAL != 0,
            )),
        ),
    );
    Ok(fd)
}

pub fn sys_memfd_secret(flags: u32) -> SyscallRet {
    const MFD_SECRET_EXCLUSIVE: u32 = 1;
    if flags & !MFD_SECRET_EXCLUSIVE != 0 {
        return Err(SysErrNo::EINVAL);
    }
    let task = current_task().unwrap();
    let fd = task.process.fd_table.alloc_fd()?;
    task.process.fd_table.set(
        fd,
        FileDescriptor::new(OpenFlags::empty(), FileClass::Abs(SecretMemFile::new())),
    );
    Ok(fd)
}
