use log::warn;

use crate::{mm::{MapPermission, ShmFlags, shm_attach, shm_create, shm_drop, shm_find}, utils::{SysErrNo, SyscallRet}};


/// 参考 https://man7.org/linux/man-pages/man2/shmget.2.html
pub fn sys_shmget(key: i32, size: usize, shmflag: i32) -> SyscallRet {
    const IPC_PRIVATE: i32 = 0;
    // 忽略权限位
    let flags = ShmFlags::from_bits(shmflag & !0x1ff).unwrap();
    match key {
        IPC_PRIVATE => Ok(shm_create(size)),
        key if key > 0 => {
            if shm_find(key as usize) {
                if flags.contains(ShmFlags::IPC_CREAT | ShmFlags::IPC_EXCL) {
                    Err(SysErrNo::EEXIST)
                } else {
                    Ok(key as usize)
                }
            } else {
                if flags.contains(ShmFlags::IPC_CREAT) {
                    Ok(shm_create(size))
                } else {
                    Err(SysErrNo::ENOENT)
                }
            }
        }
        _ => Err(SysErrNo::ENOENT),
    }
}

/// 参考 https://man7.org/linux/man-pages/man2/shmat.2.html
pub fn sys_shmat(shmid: i32, shmaddr: usize, shmflag: i32) -> SyscallRet {
    let mut permission = MapPermission::U | MapPermission::R;
    if shmflag == 0 {
        permission |= MapPermission::W | MapPermission::X
    } else {
        let shmflg = ShmFlags::from_bits(shmflag).unwrap();
        if shmflg.contains(ShmFlags::SHM_EXEC) {
            permission |= MapPermission::X;
        }
        if !shmflg.contains(ShmFlags::SHM_RDONLY) {
            permission |= MapPermission::W;
        }
    }

    match shmid {
        key if key < 0 => Err(SysErrNo::EINVAL),
        _ => shm_attach(shmid as usize, shmaddr, permission),
    }
}

/// 参考 https://man7.org/linux/man-pages/man2/shmctl.2.html
pub fn sys_shmctl(shmid: i32, cmd: i32, _buf: usize) -> SyscallRet {
    const IPC_RMID: i32 = 0;
    match cmd {
        IPC_RMID => {
            shm_drop(shmid as usize);
            Ok(0)
        }
        _ => {
            warn!("[sys_shmctl] unsupport cmd");
            Err(SysErrNo::ENOSYS)
        }
    }
}