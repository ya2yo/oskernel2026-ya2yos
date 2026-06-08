//! POSIX 消息队列 (mqueue) 系统调用实现

use log::warn;

use crate::fs::mqueue::{Mqueue, NAME_REGISTRY};
use crate::fs::{FileClass, OpenFlags};
use crate::mm::{copy_from_user, copy_to_user, read_user_cstr};
use crate::task::current_task;
use crate::utils::{SysErrNo, SyscallRet};

// ---------------------------------------------------------------------------
// mq_open
// ---------------------------------------------------------------------------

/// https://man7.org/linux/man-pages/man3/mq_open.3.html
pub fn sys_mq_open(name: *const u8, oflag: i32, _mode: u32, attr: *const u8) -> SyscallRet {
    let flags = OpenFlags::from_bits_truncate(oflag as u32);
    let create = flags.contains(OpenFlags::O_CREATE);
    let exclusive = flags.contains(OpenFlags::O_EXCL);
    let nonblock = flags.contains(OpenFlags::O_NONBLOCK);

    // 读取用户空间字符串和属性（在锁内完成）
    let (name_str, maxmsg, msgsize) = {
        let task = current_task().unwrap();
        let process = task.process.inner_lock();
        let memory_set = process.get_locked_memory_set_read();
        let name_str = read_user_cstr(&memory_set, name)?;

        if !name_str.starts_with('/') {
            return Err(SysErrNo::EINVAL);
        }

        let mut maxmsg = 10usize;
        let mut msgsize = 8192usize;
        if !attr.is_null() {
            let mut buf = [0u8; 16];
            copy_from_user(&memory_set, (attr as usize).wrapping_add(8), &mut buf[..8])?;
            copy_from_user(&memory_set, (attr as usize).wrapping_add(16), &mut buf[8..])?;
            let maxmsg_val = i64::from_ne_bytes(buf[..8].try_into().unwrap());
            let msgsize_val = i64::from_ne_bytes(buf[8..].try_into().unwrap());
            if maxmsg_val > 0 {
                maxmsg = maxmsg_val as usize;
            }
            if msgsize_val > 0 {
                msgsize = msgsize_val as usize;
            }
        }
        (name_str, maxmsg, msgsize)
    };

    // O_CREAT | O_EXCL: 队列已存在则失败
    if create && exclusive && NAME_REGISTRY.lock().contains_key(&name_str) {
        return Err(SysErrNo::EEXIST);
    }

    let mq: alloc::sync::Arc<Mqueue> = if create {
        if let Some(existing) = NAME_REGISTRY.lock().get(&name_str) {
            existing.clone()
        } else {
            let mq = Mqueue::new(name_str.clone(), maxmsg, msgsize);
            NAME_REGISTRY.lock().insert(name_str.clone(), mq.clone());
            mq
        }
    } else {
        NAME_REGISTRY
            .lock()
            .get(&name_str)
            .cloned()
            .ok_or(SysErrNo::ENOENT)?
    };

    if nonblock {
        mq.set_nonblocking(true);
    }

    // 分配 fd 并存入 fd 表
    let task = current_task().unwrap();
    let proc_inner = task.process.inner_lock();
    let newfd = proc_inner.fd_table.alloc_fd()?;
    proc_inner.fd_table.set(
        newfd,
        crate::fs::FileDescriptor::new(OpenFlags::empty(), FileClass::Abs(mq.clone())),
    )?;
    Mqueue::register_fd(newfd, &mq);

    Ok(newfd)
}

// ---------------------------------------------------------------------------
// mq_unlink
// ---------------------------------------------------------------------------

/// https://man7.org/linux/man-pages/man3/mq_unlink.3.html
pub fn sys_mq_unlink(name: *const u8) -> SyscallRet {
    let task = current_task().unwrap();
    let process = task.process.inner_lock();
    let memory_set = process.get_locked_memory_set_read();
    let name_str = read_user_cstr(&memory_set, name)?;
    if !name_str.starts_with('/') {
        return Err(SysErrNo::EINVAL);
    }
    if Mqueue::unlink_name(&name_str) {
        Ok(0)
    } else {
        Err(SysErrNo::ENOENT)
    }
}

// ---------------------------------------------------------------------------
// mq_timedsend
// ---------------------------------------------------------------------------

/// https://man7.org/linux/man-pages/man3/mq_timedsend.3.html
pub fn sys_mq_timedsend(
    mqdes: usize,
    msg_ptr: *const u8,
    msg_len: usize,
    msg_prio: u32,
    _abs_timeout: *const u8,
) -> SyscallRet {
    if msg_len == 0 {
        return Err(SysErrNo::EINVAL);
    }
    if msg_ptr.is_null() {
        return Err(SysErrNo::EFAULT);
    }

    let mq = Mqueue::lookup(mqdes)?;

    let task = current_task().unwrap();
    let process = task.process.inner_lock();
    let memory_set = process.get_locked_memory_set_read();
    let mut buf = alloc::vec![0u8; msg_len];
    copy_from_user(&memory_set, msg_ptr as usize, &mut buf)?;

    mq.try_send(&buf, msg_prio)
}

// ---------------------------------------------------------------------------
// mq_timedreceive
// ---------------------------------------------------------------------------

/// https://man7.org/linux/man-pages/man3/mq_timedreceive.3.html
pub fn sys_mq_timedreceive(
    mqdes: usize,
    msg_ptr: *mut u8,
    msg_len: usize,
    msg_prio: *mut u32,
    _abs_timeout: *const u8,
) -> SyscallRet {
    if msg_len == 0 {
        return Err(SysErrNo::EINVAL);
    }
    if msg_ptr.is_null() {
        return Err(SysErrNo::EFAULT);
    }

    let mq = Mqueue::lookup(mqdes)?;

    let task = current_task().unwrap();
    let process = task.process.inner_lock();
    let memory_set = process.get_locked_memory_set_write();

    let mut buf = alloc::vec![0u8; msg_len];
    let received = mq.try_receive(&mut buf)?;

    let to_copy = core::cmp::min(received, msg_len);
    copy_to_user(&memory_set, msg_ptr as usize, &buf[..to_copy])?;

    // 写回优先级（Phase 1 固定为 0）
    if !msg_prio.is_null() {
        let prio: u32 = 0;
        copy_to_user(&memory_set, msg_prio as usize, &prio.to_ne_bytes())?;
    }

    Ok(received)
}

// ---------------------------------------------------------------------------
// mq_notify (Phase 3)
// ---------------------------------------------------------------------------

/// https://man7.org/linux/man-pages/man3/mq_notify.3.html
pub fn sys_mq_notify(_mqdes: usize, _notification: *const u8) -> SyscallRet {
    warn!("[sys_mq_notify] not implement!");
    Err(SysErrNo::ENOSYS)
}
