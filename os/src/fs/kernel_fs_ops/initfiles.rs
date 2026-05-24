use alloc::{string::String, vec::Vec};
use log::debug;

use super::*;
use crate::{mm::UserBuffer, utils::GeneralRet};

fn flush_preload() {
    extern "C" {
        fn initproc_start();
        fn initproc_end();
    }

    let initproc = open("/initproc", OpenFlags::O_CREATE, DEFAULT_FILE_MODE)
        .unwrap()
        .file()
        .unwrap();
    let mut v = Vec::new();
    v.push(unsafe {
        core::slice::from_raw_parts_mut(
            initproc_start as *mut u8,
            initproc_end as *const () as usize - initproc_start as *const () as usize,
        ) as &'static mut [u8]
    });
    initproc.write(UserBuffer::new(v));
}

const MOUNTS: &str = " ext4 / ext rw 0 0\n";
const PASSWD: &str = "root:x:0:0:root:/root:/bin/bash\nnobody:x:1:0:nobody:/nobody:/bin/bash\n";
const MEMINFO: &str = r"
MemTotal:         944564 kB
MemFree:          835248 kB
MemAvailable:     873464 kB
Buffers:            6848 kB
Cached:            36684 kB
SwapCached:            0 kB
Active:            19032 kB
Inactive:          32676 kB
Active(anon):        128 kB
Inactive(anon):     8260 kB
Active(file):      18904 kB
Inactive(file):    24416 kB
Unevictable:           0 kB
Mlocked:               0 kB
SwapTotal:             0 kB
SwapFree:              0 kB
Dirty:                 0 kB
Writeback:             0 kB
AnonPages:          8172 kB
Mapped:            16376 kB
Shmem:               216 kB
KReclaimable:       9960 kB
Slab:              17868 kB
SReclaimable:       9960 kB
SUnreclaim:         7908 kB
KernelStack:        1072 kB
PageTables:          600 kB
NFS_Unstable:          0 kB
Bounce:                0 kB
WritebackTmp:          0 kB
CommitLimit:      472280 kB
Committed_AS:      64684 kB
VmallocTotal:   67108863 kB
VmallocUsed:       15740 kB
VmallocChunk:          0 kB
Percpu:              496 kB
HugePages_Total:       0
HugePages_Free:        0
HugePages_Rsvd:        0
HugePages_Surp:        0
Hugepagesize:       2048 kB
Hugetlb:               0 kB
";
const ADJTIME: &str = "0.000000 0.000000 UTC\n";
const LOCALTIME: &str =
    "lrwxrwxrwx 1 root root 33 11月 18  2023 /etc/localtime -> /usr/share/zoneinfo/Asia/Shanghai\n";
const PRELOAD: &str = "";

pub fn create_init_files() -> GeneralRet {
    // 写入预先加载内容
    flush_preload();
    //创建/proc文件夹
    open(
        "/proc",
        OpenFlags::O_CREATE | OpenFlags::O_RDWR | OpenFlags::O_DIRECTORY,
        DEFAULT_DIR_MODE,
    )?;
    //创建/proc/mounts文件系统使用情况
    let mountsfile = open(
        "/proc/mounts",
        OpenFlags::O_CREATE | OpenFlags::O_RDWR,
        DEFAULT_FILE_MODE,
    )?
    .file()?;
    let mut mountsinfo = String::from(MOUNTS);
    let mut mountsvec = Vec::new();
    unsafe {
        let mounts = mountsinfo.as_bytes_mut();
        mountsvec.push(core::slice::from_raw_parts_mut(
            mounts.as_mut_ptr(),
            mounts.len(),
        ));
    }
    let mountbuf = UserBuffer::new(mountsvec);
    let mountssize = mountsfile.write(mountbuf)?;
    debug!("create /proc/mounts with {} sizes", mountssize);
    //创建/proc/meminfo系统内存使用情况
    let memfile = open(
        "/proc/meminfo",
        OpenFlags::O_CREATE | OpenFlags::O_RDWR,
        DEFAULT_FILE_MODE,
    )?
    .file()?;
    let mut meminfo = String::from(MEMINFO);
    let mut memvec = Vec::new();
    unsafe {
        let mem = meminfo.as_bytes_mut();
        memvec.push(core::slice::from_raw_parts_mut(mem.as_mut_ptr(), mem.len()));
    }
    let membuf = UserBuffer::new(memvec);
    let memsize = memfile.write(membuf)?;
    debug!("create /proc/meminfo with {} sizes", memsize);
    //创建/dev文件夹
    open(
        "/dev",
        OpenFlags::O_CREATE | OpenFlags::O_RDWR | OpenFlags::O_DIRECTORY,
        DEFAULT_DIR_MODE,
    )?;
    //注册设备/dev/rtc和/dev/rtc0
    register_device("/dev/rtc");
    register_device("/dev/rtc0");
    //注册设备/dev/tty
    register_device("/dev/tty");
    //注册设备/dev/zero
    register_device("/dev/zero");
    //注册设备/dev/numm
    register_device("/dev/null");
    //注册设备/dev/cpu_dma_latency
    register_device("/dev/cpu_dma_latency");
    //创建./dev/misc文件夹
    open(
        "/dev/misc",
        OpenFlags::O_CREATE | OpenFlags::O_RDWR | OpenFlags::O_DIRECTORY,
        DEFAULT_DIR_MODE,
    )?;
    //注册设备/dev/misc/rtc
    register_device("/dev/misc/rtc");
    //创建/etc文件夹
    open(
        "/etc",
        OpenFlags::O_CREATE | OpenFlags::O_RDWR | OpenFlags::O_DIRECTORY,
        DEFAULT_DIR_MODE,
    )?;
    //创建/etc/adjtime记录时间偏差
    let adjtimefile = open(
        "/etc/adjtime",
        OpenFlags::O_CREATE | OpenFlags::O_RDWR,
        DEFAULT_FILE_MODE,
    )?
    .file()?;
    let mut adjtime = String::from(ADJTIME);
    let mut adjtimevec = Vec::new();
    unsafe {
        let adj = adjtime.as_bytes_mut();
        adjtimevec.push(core::slice::from_raw_parts_mut(adj.as_mut_ptr(), adj.len()));
    }
    let adjtimebuf = UserBuffer::new(adjtimevec);
    let adjtimesize = adjtimefile.write(adjtimebuf)?;
    debug!("create /etc/adjtime with {} sizes", adjtimesize);

    //创建./etc/localtime记录时区
    let localtimefile = open(
        "/etc/localtime",
        OpenFlags::O_CREATE | OpenFlags::O_RDWR,
        DEFAULT_FILE_MODE,
    )?
    .file()?;
    let mut localtime = String::from(LOCALTIME);
    let mut localtimevec = Vec::new();
    unsafe {
        let local = localtime.as_bytes_mut();
        localtimevec.push(core::slice::from_raw_parts_mut(
            local.as_mut_ptr(),
            local.len(),
        ));
    }
    let localtimebuf = UserBuffer::new(localtimevec);
    let localtimesize = localtimefile.write(localtimebuf)?;
    debug!("create /etc/localtime with {} sizes", localtimesize);

    //创建/etc/passwd记录用户信息
    let passwdfile = open(
        "/etc/passwd",
        OpenFlags::O_CREATE | OpenFlags::O_RDWR,
        DEFAULT_FILE_MODE,
    )?
    .file()?;
    let mut passwd = String::from(PASSWD);
    let mut passwdvec = Vec::new();
    unsafe {
        let wd = passwd.as_bytes_mut();
        passwdvec.push(core::slice::from_raw_parts_mut(wd.as_mut_ptr(), wd.len()));
    }
    let passwdbuf = UserBuffer::new(passwdvec);
    let passwdsize = passwdfile.write(passwdbuf)?;
    debug!("create /etc/passwd with {} sizes", passwdsize);

    //创建/etc/ld.so.preload记录用户信息
    let preloadfile = open(
        "/etc/ld.so.preload",
        OpenFlags::O_CREATE | OpenFlags::O_RDWR,
        DEFAULT_FILE_MODE,
    )?
    .file()?;
    let mut preload = String::from(PRELOAD);
    let mut preloadvec = Vec::new();
    unsafe {
        let pre = preload.as_bytes_mut();
        preloadvec.push(core::slice::from_raw_parts_mut(pre.as_mut_ptr(), pre.len()));
    }
    let preloadbuf = UserBuffer::new(preloadvec);
    let preloadsize = preloadfile.write(preloadbuf)?;
    debug!("create /etc/ld.so.preload with {} sizes", preloadsize);

    // 创建/tmp文件夹
    open(
        "/tmp",
        OpenFlags::O_CREATE | OpenFlags::O_RDWR | OpenFlags::O_DIRECTORY,
        DEFAULT_DIR_MODE,
    )?;

    // 创建/bin文件夹，这是为了应付测试集中的which ls
    open(
        "/bin",
        OpenFlags::O_CREATE | OpenFlags::O_RDWR | OpenFlags::O_DIRECTORY,
        DEFAULT_DIR_MODE,
    )?;
    // 创建一系列符号链接指向busybox，这样就近似实现了bash
    for path in [
        "/bin/ls",       // which ls 需要它
        "/bin/basename", // 如果不加这个，ltp_testcode.sh会无法使用basename
        "/bin/mkdir",    // ltp的cgroup_regression_3_1.sh需要它
        "/bin/rmdir",    // ltp的cgroup_regression_3_1.sh需要它
        "/bin/cat",      // ltp的cgroup_regression_3_2.sh需要它
        "/bin/grep",     // ltp的cgroup_fj_proc需要
        "/bin/sed",
        "/bin/id",
        "/bin/killall",
        "/bin/mktemp",
        "/bin/chmod",
        "/bin/cut",
    ] {
        superblock_root_inode().sym_link("/musl/busybox", path);
    }

    println!("create_init_files success!");
    Ok(())
}
