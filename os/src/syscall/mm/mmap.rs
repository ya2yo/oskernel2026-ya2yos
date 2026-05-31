//! memory related syscall

use alloc::format;
use log::debug;

use super::super::{MmapFlags, MmapProt};
use crate::{
    arch::memory_layout::PAGE_SIZE,
    fs::File,
    mm::{
        if_bad_address, insert_bad_address, remove_bad_address, shm_attach, shm_create, shm_drop,
        shm_find, MapArea, MapAreaType, MapPermission, MremapFlags, ShmFlags, VirtAddr,
        VirtPageNum,
    },
    syscall::process,
    task::{self, current_task},
    utils::{page_round_up, SysErrNo, SyscallRet},
};

/// 参考 https://man7.org/linux/man-pages/man2/mmap.2.html
pub fn sys_mmap(
    addr: usize,
    len: usize,
    prot: u32,
    flags: u32,
    fd: usize,
    off: usize,
) -> SyscallRet {
    debug!(
        "[sysmap] addr={:#x},len={},prot={:#x},flags={:#x},fd={},off={}",
        addr, len, prot, flags, fd, off
    );
    let map_perm: MapPermission = MmapProt::from_bits(prot).unwrap().into();
    let flags = MmapFlags::from_bits(flags).expect(&format!(
        "sys_mmap: Failed to convert flags to MmapFlags bitmap: value is {:#x}",
        flags
    ));
    // flags=0x4022导致问题
    // 不对啊，1<<14这一位没用啊？

    // 地址合法性
    if flags.contains(MmapFlags::MAP_FIXED) && addr == 0 {
        return Err(SysErrNo::EPERM);
    }
    let task = current_task().unwrap();
    let process = task.process.inner_lock();
    let memory_set = process.get_locked_memory_set_write();
    let len = page_round_up(len);
    if fd == usize::MAX {
        if !flags.contains(MmapFlags::MAP_ANONYMOUS) {
            return Err(SysErrNo::EBADF);
        }
        let rv = memory_set.mmap(addr, len, map_perm, flags, None, usize::MAX);
        return Ok(rv);
    }
    if flags.contains(MmapFlags::MAP_ANONYMOUS) {
        //映射1字节没有任何权限的地址
        let rv = memory_set.mmap(0, 1, MapPermission::empty(), flags, None, usize::MAX);
        insert_bad_address(rv);
        log::info!("bad address is 0x{:x}", rv);
        return Ok(rv);
    }
    // check fd and map_permission
    let file = process.fd_table.get(fd)?.file()?;
    // 读写权限
    if map_perm.contains(MapPermission::R) && !file.readable()
        || flags.contains(MmapFlags::MAP_SHARED)
            && map_perm.contains(MapPermission::W)
            && !file.writable()
    {
        return Err(SysErrNo::EPERM);
    }
    let rv = memory_set.mmap(addr, len, map_perm, flags, Some(file), off);
    debug!("[sys_mmap] alloc addr={:#x}", rv);
    Ok(rv)
}

/// 参考 https://man7.org/linux/man-pages/man2/munmap.2.html
pub fn sys_munmap(addr: usize, len: usize) -> SyscallRet {
    debug!("[sys_munmap] addr={:#x}, len={:#x}", addr, len);
    let task = current_task().unwrap();
    let process = task.process.inner_lock();
    let memory_set = process.get_locked_memory_set_write();
    let len = page_round_up(len);
    if if_bad_address(addr) {
        remove_bad_address(addr);
    }
    memory_set.munmap(addr, len)
}

/// Add by HXC
pub fn sys_mremap(
    old_addr: usize,
    old_size: usize,
    new_size: usize,
    flags: i32,
    _new_addr: usize,
) -> SyscallRet {
    let flags_bitmap = MremapFlags::from_bits(flags).expect("Invalid flags on mremap!");
    // debug!(
    //     "sys_mremap: old_addr={:#x}, old_size={}, new_size={}, new_addr={:#x}",
    //     old_addr, old_size, new_size, new_addr
    // );
    // debug!("flags={:?}", flags_bitmap);
    // 允许内核在原地址空间不足时，将内存区域移动到新的虚拟地址。此时返回值应为移动后的地址。此时new_addr参数应该被忽视
    // 如果为false，则表示必须原地扩展/收缩
    let may_move = flags_bitmap.contains(MremapFlags::MAYMOVE);

    // 强制将内存重新映射到指定的新地址new_addr​（需配合MREMAP_MAYMOVE使用）
    // 如果为true，则may_move必须为true
    // 如果为false，则new_addr则是建议地址，不强制
    let fixed = flags_bitmap.contains(MremapFlags::FIXED);

    // 如果存在，则并不调整原来的map，而是创建一个新的[new_addr, new_addr+new_size]虚拟地址空间，映射到原来的物理地址空间
    let dont_unmap = flags_bitmap.contains(MremapFlags::DONTUNMAP);
    if dont_unmap {
        println!("sys_munmap for DONTUNMAP unimplemented!");
        unimplemented!();
    }
    if fixed && !may_move {
        panic!("fixed && !may_mov");
    }
    let task = current_task().unwrap();
    let process = task.process.inner_lock();
    let memory_set = process.get_locked_memory_set_write();
    let old_area = memory_set
        .get_mut()
        .find_area_by_range(
            VirtAddr::from(old_addr).floor(),
            VirtAddr::from(old_addr + old_size - 1).ceil(),
        )
        .unwrap();
    if old_area.area_type != MapAreaType::Mmap {
        debug!("old_area.area_type != MapAreaType::Mmap");
        return Err(SysErrNo::EINVAL);
    }
    let old_flag = old_area.mmap_flags;
    let old_file = &old_area.mmap_file;
    let old_perm = old_area.map_perm;

    if fixed {
        unimplemented!();
    } else if may_move {
        // sys_munmap(old_addr, old_size);
        // debug!("[sys_munmap] addr={:#x}, len={:#x}", addr, len);
        if if_bad_address(old_addr) {
            remove_bad_address(old_addr);
        }
        memory_set.munmap(old_addr, page_round_up(old_size));
        // sys_mmap(old_addr, new_len, prot, flags, fd, off)
        let file = &old_file.file;

        if let Some(inode) = file {
            // 读写权限
            if old_perm.contains(MapPermission::R) && !inode.readable()
                || old_flag.contains(MmapFlags::MAP_SHARED)
                    && old_perm.contains(MapPermission::W)
                    && !inode.writable()
            {
                return Err(SysErrNo::EPERM);
            }
        }

        let rv = memory_set.mmap(
            old_addr,
            new_size,
            old_perm,
            old_flag,
            file.clone(),
            old_file.offset,
        );

        return Ok(rv);
    } else {
        // fixed == may_move == 0
        unimplemented!();
    }
}

/// 参考 https://man7.org/linux/man-pages/man2/mprotect.2.html
pub fn sys_mprotect(addr: usize, len: usize, prot: u32) -> SyscallRet {
    if (addr % PAGE_SIZE != 0) || (len % PAGE_SIZE != 0) {
        println!("sys_mprotect: not align");
        return Err(SysErrNo::EINVAL);
    }
    let map_perm: MapPermission = MmapProt::from_bits(prot).unwrap().into();

    // debug!(
    //     "[sys_mprotect] addr is {:x}, len is {:#x}, map_perm is {:?}",
    //     addr, len, map_perm
    // );

    let task = current_task().unwrap();
    let process = task.process.inner_lock();
    let memory_set = process.get_locked_memory_set_write();
    let start_vpn = VirtAddr::from(addr).floor();
    let end_vpn = VirtAddr::from(addr + len).ceil();
    //修改各段的mappermission
    memory_set.mprotect(start_vpn, end_vpn, map_perm);
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/madvise.2.html
pub fn sys_madvise(_addr: usize, _len: usize, _advice: usize) -> SyscallRet {
    //伪实现，该系统调用用于给内存提建议
    // debug!(
    //     "[sys_madvise] addr is {}, len is {}, advice is {}",
    //     addr, len, advice
    // );
    Ok(0)
}

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
            panic!("[sys_shmctl] unsupport cmd");
        }
    }
}
