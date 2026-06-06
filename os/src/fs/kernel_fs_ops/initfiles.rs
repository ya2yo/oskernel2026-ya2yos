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

/// 将内嵌的 libgcc_s.so.1 写入 glibc 库目录，
/// 解决竞赛固定镜像中缺少该库的问题（glibc pthread 测试依赖此库）
fn flush_libgcc_s() {
    extern "C" {
        fn libgcc_s_start();
        fn libgcc_s_end();
    }

    let size = libgcc_s_end as *const () as usize - libgcc_s_start as *const () as usize;
    // 如果内嵌的库大小为 0（编译时未找到文件），则跳过
    if size == 0 {
        return;
    }

    // 确保 /glibc/lib 目录存在
    open(
        "/glibc/lib",
        OpenFlags::O_CREATE | OpenFlags::O_RDWR | OpenFlags::O_DIRECTORY,
        DEFAULT_DIR_MODE,
    ).ok();

    let file = open(
        "/glibc/lib/libgcc_s.so.1",
        OpenFlags::O_CREATE,
        DEFAULT_FILE_MODE,
    )
    .unwrap()
    .file()
    .unwrap();
    let mut v = Vec::new();
    v.push(unsafe {
        core::slice::from_raw_parts_mut(libgcc_s_start as *mut u8, size)
    });
    file.write(UserBuffer::new(v));
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
    // 写入内嵌的 libgcc_s.so.1（解决 glibc pthread 测试依赖问题）
    flush_libgcc_s();
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
    //创建/proc/sys/kernel/tainted 内核污染标记
    open(
        "/proc/sys",
        OpenFlags::O_CREATE | OpenFlags::O_RDWR | OpenFlags::O_DIRECTORY,
        DEFAULT_DIR_MODE,
    )?;
    open(
        "/proc/sys/kernel",
        OpenFlags::O_CREATE | OpenFlags::O_RDWR | OpenFlags::O_DIRECTORY,
        DEFAULT_DIR_MODE,
    )?;
    let taintedfile = open(
        "/proc/sys/kernel/tainted",
        OpenFlags::O_CREATE | OpenFlags::O_RDWR,
        DEFAULT_FILE_MODE,
    )?
    .file()?;
    let mut tainted = String::from("0\n");
    let mut taintedvec = Vec::new();
    unsafe {
        let t = tainted.as_bytes_mut();
        taintedvec.push(core::slice::from_raw_parts_mut(t.as_mut_ptr(), t.len()));
    }
    let taintedbuf = UserBuffer::new(taintedvec);
    taintedfile.write(taintedbuf)?;
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
    // loop 设备路径由 devfs 动态解析，此处仅创建常用目录
    open(
        "/dev/block",
        OpenFlags::O_CREATE | OpenFlags::O_RDWR | OpenFlags::O_DIRECTORY,
        DEFAULT_DIR_MODE,
    )?;
    open(
        "/dev/loop",
        OpenFlags::O_CREATE | OpenFlags::O_RDWR | OpenFlags::O_DIRECTORY,
        DEFAULT_DIR_MODE,
    )?;
    open(
        "/dev/shm",
        OpenFlags::O_CREATE | OpenFlags::O_RDWR | OpenFlags::O_DIRECTORY,
        DEFAULT_DIR_MODE,
    )?;
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
    // 注意：ext4_fsymlink 遇到已存在文件会静默失败（返回Ok但不覆盖）
    for path in [
        "/bin/awk",
        "/bin/basename", // 如果不加这个，ltp_testcode.sh会无法使用basename
        "/bin/bc",
        "/bin/bzip2",
        "/bin/cat",      // ltp的cgroup_regression_3_2.sh需要它
        "/bin/chmod",
        "/bin/cp",       // 通用文件操作
        "/bin/cut",
        "/bin/dd",
        "/bin/gdb",
        "/bin/grep",     // ltp的cgroup_fj_proc需要
        "/bin/gunzip",
        "/bin/gzip",
        "/bin/killall",
        "/bin/id",
        "/bin/ip",
        "/bin/ln",
        "/bin/ls",       // which ls 需要它
        "/bin/locale",
        "/bin/mkdir",    // ltp的cgroup_regression_3_1.sh需要它
        "/bin/mktemp",
        "/bin/rmdir",    // ltp的cgroup_regression_3_1.sh需要它
        "/bin/rsh",
        "/bin/sed",
        "/bin/sleep",
        "/bin/sh",
        "/bin/tc",
        "/bin/touch",
        "/bin/mount",
        "/bin/umount",
        "/bin/rm",       // fs_bind 清理需要
        "/bin/mv",
    ] {
        let _ = superblock_root_inode().unlink(path);
        if let Err(e) = superblock_root_inode().sym_link("/musl/busybox", path) {
            println!("WARN: sym_link {} -> /musl/busybox failed: {:?}", path, e);
        }
    }

    // tst_sleep 和 tst_timeout_kill 不是 busybox applet，用 shell 脚本实现
    // tst_sleep: 将 LTP 的 "100ms" 格式转为 busybox sleep 支持的 "0.100" 秒格式
    {
        let mut content = String::from("#!/bin/sh\n# LTP tst_sleep wrapper: converts 100ms -> 0.100\narg=\"$1\"\ncase \"$arg\" in\n    *ms) /bin/sleep \"0.${arg%ms}\" ;;\n    *) /bin/sleep \"$arg\" ;;\nesac\n");
        let file = open("/bin/tst_sleep", OpenFlags::O_CREATE | OpenFlags::O_RDWR, DEFAULT_FILE_MODE)?.file()?;
        let mut v = Vec::new();
        unsafe { v.push(core::slice::from_raw_parts_mut(content.as_bytes_mut().as_mut_ptr(), content.len())); }
        file.write(UserBuffer::new(v))?;
        file.inode.sync();
    }
    // tst_timeout_kill: 发送 SIGTERM + SIGKILL 终止超时 watchdog
    {
        let mut content = String::from("#!/bin/sh\n# LTP tst_timeout_kill wrapper\npid=\"$1\"\nif [ -n \"$pid\" ] && [ \"$pid\" -gt 0 ] 2>/dev/null; then\n    kill -TERM \"$pid\" 2>/dev/null\n    /bin/sleep 0.1\n    kill -KILL \"$pid\" 2>/dev/null\nfi\n");
        let file = open("/bin/tst_timeout_kill", OpenFlags::O_CREATE | OpenFlags::O_RDWR, DEFAULT_FILE_MODE)?.file()?;
        let mut v = Vec::new();
        unsafe { v.push(core::slice::from_raw_parts_mut(content.as_bytes_mut().as_mut_ptr(), content.len())); }
        file.write(UserBuffer::new(v))?;
        file.inode.sync();
    }
    // tst_rod: LTP "remove old directory" → rm -rf
    {
        let mut content = String::from("#!/bin/sh\n# LTP tst_rod wrapper\n/bin/rm -rf \"$@\"\n");
        let file = open("/bin/tst_rod", OpenFlags::O_CREATE | OpenFlags::O_RDWR, DEFAULT_FILE_MODE)?.file()?;
        let mut v = Vec::new();
        unsafe { v.push(core::slice::from_raw_parts_mut(content.as_bytes_mut().as_mut_ptr(), content.len())); }
        file.write(UserBuffer::new(v))?;
        file.inode.sync();
    }

    // 磁盘镜像中 glibc/lib 下已同时存在 libm.so 和 libm.so.6（两个独立文件），
    // 此处不再创建重复的符号链接，避免覆盖已存在的普通文件。

    println!("create_init_files success!");
    Ok(())
}
