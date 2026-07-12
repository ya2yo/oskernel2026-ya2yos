use crate::{
    arch::{memory_layout::PAGE_SIZE, time::get_ticks},
    mm::{MapPermission, MemorySet, UserBuffer},
    syscall::MmapFlags,
    utils::SysErrNo,
};

// 该文件创建形如/proc/xxx的文件
use super::*;
use alloc::{format, string::String, vec::Vec};

const PAGEMAP_ENTRY_SIZE: usize = core::mem::size_of::<u64>();
const PAGEMAP_PFN_MASK: u64 = (1u64 << 55) - 1;
const PAGEMAP_PRESENT: u64 = 1u64 << 63;

fn format_map_perm(perm: MapPermission, flags: MmapFlags) -> String {
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
    s.push(if flags.contains(MmapFlags::MAP_SHARED) {
        's'
    } else {
        'p'
    });
    s
}

fn write_kernel_file(file: &dyn File, data: &mut String) -> Result<usize, SysErrNo> {
    let mut vec = Vec::new();
    unsafe {
        let bytes = data.as_bytes_mut();
        vec.push(core::slice::from_raw_parts_mut(
            bytes.as_mut_ptr(),
            bytes.len(),
        ));
    }
    file.write(UserBuffer::new(vec))
}

fn format_status(pid: usize, ppid: usize, comm: &str, memory_set: &MemorySet) -> String {
    let vm_size = memory_set.virtual_size_kb();
    let vm_rss = memory_set.resident_size_kb();
    format!(
        "VmSwap:\t       0 kB\n\
VmHWM:\t{:8} kB\n\
VmRSS:\t{:8} kB\n\
Name:\t{}\n\
State:\tS (sleeping)\n\
Tgid:\t{}\n\
Pid:\t{}\n\
PPid:\t{}\n\
VmPeak:\t{:8} kB\n\
VmSize:\t{:8} kB\n\
VmHWM:\t{:8} kB\n\
VmRSS:\t{:8} kB\n",
        vm_rss, vm_rss, comm, pid, pid, ppid, vm_size, vm_size, vm_rss, vm_rss
    )
}

fn format_stat(pid: usize, ppid: usize, state: char, comm: &str, memory_set: &MemorySet) -> String {
    let vsize = memory_set.virtual_size_kb() * 1024;
    let rss_pages = memory_set.resident_size_kb() * 1024 / PAGE_SIZE;
    format!(
        "{} ({}) {} {} 0 0 0 0 0 0 0 0 0 0 0 0 0 0 1 0 0 {} {} {} 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n",
        pid,
        comm,
        state,
        ppid,
        get_ticks(),
        vsize,
        rss_pages
    )
}

pub fn create_proc_dir_and_file(
    pid: usize,
    ppid: usize,
    comm: &str,
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
        OpenFlags::O_CREATE | OpenFlags::O_RDWR | OpenFlags::O_TRUNC,
        DEFAULT_FILE_MODE,
    )
    .unwrap()
    .file()?;
    let mut statinfo = format_stat(pid, ppid, 'S', comm, memory_set);
    write_kernel_file(statfile.as_ref(), &mut statinfo)?;
    statfile.inode.sync();

    //创建进程状态文件/proc/<pid>/status
    let statusfile = open(
        format!("/proc/{}/status", pid).as_str(),
        OpenFlags::O_CREATE | OpenFlags::O_RDWR | OpenFlags::O_TRUNC,
        DEFAULT_FILE_MODE,
    )?
    .file()?;
    let mut statusinfo = format_status(pid, ppid, comm, memory_set);
    write_kernel_file(statusfile.as_ref(), &mut statusinfo)?;
    statusfile.inode.sync();

    refresh_proc_maps(pid, memory_set)?;
    refresh_proc_pagemap(pid, memory_set)?;

    Ok(())
}

/// Rebuild `/proc/<pid>/maps` from the process's current VMA metadata.
///
/// Proc files are regular VFS files in Ya2yOS, so their contents must be
/// refreshed before every open instead of being treated as a creation-time
/// snapshot. Copy VMA metadata before entering the filesystem to avoid
/// holding the address-space lock across VFS operations.
pub fn refresh_proc_maps(pid: usize, memory_set: &MemorySet) -> Result<(), SysErrNo> {
    let areas = {
        let memory_set = memory_set.get_ref();
        memory_set
            .areas
            .iter()
            .map(|area| {
                (
                    area.vpn_range.start().0 * PAGE_SIZE,
                    area.vpn_range.end().0 * PAGE_SIZE,
                    area.map_perm,
                    area.mmap_flags,
                    area.mmap_file.offset,
                    area.mmap_file.file.clone(),
                )
            })
            .collect::<Vec<_>>()
    };

    let mapsfile = open(
        format!("/proc/{}/maps", pid).as_str(),
        OpenFlags::O_CREATE | OpenFlags::O_RDWR | OpenFlags::O_TRUNC,
        DEFAULT_FILE_MODE,
    )?
    .file()?;
    let mut mapsinfo = String::new();
    for (start, end, perm, flags, offset, file) in areas {
        let pathname = file.map(|file| file.inode.path()).unwrap_or_default();
        mapsinfo.push_str(&format!(
            "{:x}-{:x} {} {:08x} 00:00 0 {}\n",
            start,
            end,
            format_map_perm(perm, flags),
            offset,
            pathname
        ));
    }
    write_kernel_file(mapsfile.as_ref(), &mut mapsinfo)?;
    mapsfile.inode.sync();
    Ok(())
}

/// Rebuild `/proc/<pid>/pagemap` from the current hardware page table.
///
/// Each entry is a native-endian Linux pagemap u64. Ya2yOS does not implement
/// swap, soft-dirty, userfaultfd write-protect, or page exclusivity tracking,
/// so only the present bit (63) and PFN bits (0-54) are populated. The file is
/// sparse: holes represent unmapped pages and read back as zero.
pub fn refresh_proc_pagemap(pid: usize, memory_set: &MemorySet) -> Result<(), SysErrNo> {
    // Snapshot page-table state before entering VFS. In particular, do not hold
    // the address-space lock while open/truncate/write_at can touch the FS.
    let (file_size, present_runs) = {
        let memory_set = memory_set.get_ref();
        let mut highest_vpn = 0usize;
        let mut present_runs = Vec::new();

        for area in memory_set.areas.iter() {
            // pagemap offset is vpn * sizeof(u64); the logical file must cover
            // every VMA even when a lazy page has not faulted in yet.
            highest_vpn = highest_vpn.max(area.vpn_range.end().0);

            let mut run_start = None;
            let mut run = Vec::new();
            for vpn in area.vpn_range {
                match memory_set.page_table.translate(vpn) {
                    Some(ppn) => {
                        if run_start.is_none() {
                            run_start = Some(vpn.0);
                        }
                        // Present pages expose their PFN. Unsupported Linux
                        // pagemap flags deliberately remain zero.
                        let entry = PAGEMAP_PRESENT | (ppn.0 as u64 & PAGEMAP_PFN_MASK);
                        run.extend_from_slice(&entry.to_ne_bytes());
                    }
                    None if let Some(start) = run_start.take() => {
                        // Emit adjacent present entries in one VFS write. A
                        // following hole is left unwritten and reads as zero.
                        present_runs.push((start, run));
                        run = Vec::new();
                    }
                    None => {}
                }
            }
            if let Some(start) = run_start {
                present_runs.push((start, run));
            }
        }

        let file_size = highest_vpn
            .checked_mul(PAGEMAP_ENTRY_SIZE)
            .ok_or(SysErrNo::EFBIG)?;
        (file_size, present_runs)
    };

    let pagemapfile = open(
        format!("/proc/{}/pagemap", pid).as_str(),
        OpenFlags::O_CREATE | OpenFlags::O_RDWR | OpenFlags::O_TRUNC,
        DEFAULT_FILE_MODE,
    )?
    .file()?;
    // truncate establishes the Linux-visible pagemap length without filling
    // high virtual-address ranges with explicit zero bytes.
    pagemapfile.inode.truncate(file_size)?;

    for (start_vpn, entries) in present_runs {
        // A pagemap entry is addressed by virtual page number, not by its PFN.
        let offset = start_vpn
            .checked_mul(PAGEMAP_ENTRY_SIZE)
            .ok_or(SysErrNo::EFBIG)?;
        pagemapfile.inode.write_at(offset, &entries)?;
    }
    pagemapfile.inode.sync();
    Ok(())
}

pub fn refresh_proc_stat(
    pid: usize,
    ppid: usize,
    state: char,
    comm: &str,
    memory_set: &MemorySet,
) -> Result<(), SysErrNo> {
    let statfile = open(
        format!("/proc/{}/stat", pid).as_str(),
        OpenFlags::O_CREATE | OpenFlags::O_RDWR | OpenFlags::O_TRUNC,
        DEFAULT_FILE_MODE,
    )?
    .file()?;
    let mut statinfo = format_stat(pid, ppid, state, comm, memory_set);
    write_kernel_file(statfile.as_ref(), &mut statinfo)?;
    statfile.inode.sync();
    Ok(())
}

pub fn refresh_proc_status(
    pid: usize,
    ppid: usize,
    comm: &str,
    memory_set: &MemorySet,
) -> Result<(), SysErrNo> {
    let statusfile = open(
        format!("/proc/{}/status", pid).as_str(),
        OpenFlags::O_CREATE | OpenFlags::O_RDWR | OpenFlags::O_TRUNC,
        DEFAULT_FILE_MODE,
    )?
    .file()?;
    let mut statusinfo = format_status(pid, ppid, comm, memory_set);
    write_kernel_file(statusfile.as_ref(), &mut statusinfo)?;
    statusfile.inode.sync();
    Ok(())
}

pub fn remove_proc_dir_and_file(pid: usize) {
    superblock_root_inode().unlink(format!("/proc/{}/pagemap", pid).as_str());
    FsIndex::remove_inode_idx(format!("/proc/{}/pagemap", pid).as_str());
    superblock_root_inode().unlink(format!("/proc/{}/maps", pid).as_str());
    FsIndex::remove_inode_idx(format!("/proc/{}/maps", pid).as_str());
    superblock_root_inode().unlink(format!("/proc/{}/status", pid).as_str());
    FsIndex::remove_inode_idx(format!("/proc/{}/status", pid).as_str());
    superblock_root_inode().unlink(format!("/proc/{}/stat", pid).as_str());
    FsIndex::remove_inode_idx(format!("/proc/{}/stat", pid).as_str());
    superblock_root_inode().unlink(format!("/proc/{}", pid).as_str());
    FsIndex::remove_inode_idx(format!("/proc/{}", pid).as_str());
}
