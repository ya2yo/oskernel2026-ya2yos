use crate::{
    arch::{memory_layout::PAGE_SIZE, time::get_ticks},
    mm::{MapPermission, MemorySet, UserBuffer},
};

// 该文件创建形如/proc/xxx的文件
use super::*;
use alloc::{format, string::String, vec::Vec};

fn format_map_perm(perm: MapPermission) -> String {
    let mut s = String::with_capacity(4);
    s.push(if perm.contains(MapPermission::R) {
        'r'
    } else {
        '-'
    });
    s.push(if perm.contains(MapPermission::W) {
        'w'
    } else {
        '-'
    });
    s.push(if perm.contains(MapPermission::X) {
        'x'
    } else {
        '-'
    });
    s.push('p');
    s
}

pub fn create_proc_dir_and_file(
    pid: usize,
    ppid: usize,
    memory_set: &MemorySet,
) -> Result<(), SysErrNo> {
    let procdir = open(
        format!("/proc/{}", pid).as_str(),
        OpenFlags::O_DIRECTORY | OpenFlags::O_CREATE | OpenFlags::O_RDWR,
        DEFAULT_DIR_MODE,
    )?
    .file()?;
    // 强制刷盘确保目录创建持久化，否则后续在目录内创建的文件可能不可见
    procdir.inode.sync();

    //创建进程状态文件/proc/<pid>/stat
    let statfile = open(
        format!("/proc/{}/stat", pid).as_str(),
        OpenFlags::O_CREATE | OpenFlags::O_RDWR,
        DEFAULT_FILE_MODE,
    )
    .unwrap()
    .file()?;
    let mut statinfo = format!(
        "{} (busybox) S {} 0 0 0 0 0 0 0 0 0 {} 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0",
        pid,
        ppid,
        get_ticks()
    );
    let mut statvec = Vec::new();
    unsafe {
        let stat = statinfo.as_bytes_mut();
        statvec.push(core::slice::from_raw_parts_mut(
            stat.as_mut_ptr(),
            stat.len(),
        ));
    }
    let statbuf = UserBuffer::new(statvec);
    statfile.write(statbuf)?;
    statfile.inode.sync();

    //创建进程内存映射文件/proc/<pid>/maps
    let mapsfile = open(
        format!("/proc/{}/maps", pid).as_str(),
        OpenFlags::O_CREATE | OpenFlags::O_RDWR,
        DEFAULT_FILE_MODE,
    )?
    .file()?;
    let mut mapsinfo = String::new();
    for area in &memory_set.get_ref().areas {
        let start = area.vpn_range.start().0 * PAGE_SIZE;
        let end = area.vpn_range.end().0 * PAGE_SIZE;
        let perm = format_map_perm(area.map_perm);
        let offset = area.mmap_file.offset;
        let pathname = if let Some(ref file) = area.mmap_file.file {
            file.inode.path()
        } else {
            String::new()
        };
        mapsinfo.push_str(&format!(
            "{:016x}-{:016x} {} {:08x} 00:00 0 {}\n",
            start, end, perm, offset, pathname
        ));
    }
    let mut mapsvec = Vec::new();
    unsafe {
        let maps = mapsinfo.as_bytes_mut();
        mapsvec.push(core::slice::from_raw_parts_mut(
            maps.as_mut_ptr(),
            maps.len(),
        ));
    }
    let mapsbuf = UserBuffer::new(mapsvec);
    mapsfile.write(mapsbuf)?;
    mapsfile.inode.sync();

    Ok(())
}

pub fn remove_proc_dir_and_file(pid: usize) {
    superblock_root_inode().unlink(format!("/proc/{}/maps", pid).as_str());
    FsIndex::remove_inode_idx(format!("/proc/{}/maps", pid).as_str());
    superblock_root_inode().unlink(format!("/proc/{}/stat", pid).as_str());
    FsIndex::remove_inode_idx(format!("/proc/{}/stat", pid).as_str());
    superblock_root_inode().unlink(format!("/proc/{}", pid).as_str());
    FsIndex::remove_inode_idx(format!("/proc/{}", pid).as_str());
}
