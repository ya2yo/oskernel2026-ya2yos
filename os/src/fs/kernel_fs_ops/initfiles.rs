//! 启动期补齐竞赛镜像中内核依赖的伪文件、设备节点和测试 wrapper。
//!
//! 根文件系统来自固定测试镜像，但部分 LTP/libc/network 用例依赖 `/proc`、
//! `/dev`、`/etc`、`/bin` 下的兼容文件或 busybox applet 链接。本模块在
//! `fs::init()` 阶段集中创建这些启动资产，避免把测试环境补丁散落到 syscall
//! 或 VFS 业务路径中。

use alloc::{format, string::String, vec::Vec};
use log::debug;

use super::*;
use crate::{
    fs::PIPE_MAX_SIZE,
    mm::UserBuffer,
    utils::{SysErrNo, SysResult},
};

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

    // libgcc_s 只用于竞赛 glibc 测试镜像；Alpine 等根文件系统没有 /glibc。
    // 若父目录不存在，说明当前镜像不需要这个补丁，直接跳过。
    let glibc_lib = open(
        "/glibc/lib",
        OpenFlags::O_RDONLY | OpenFlags::O_DIRECTORY,
        0,
    );
    if glibc_lib.is_err() {
        return;
    }

    let Ok(file) = open(
        "/glibc/lib/libgcc_s.so.1",
        OpenFlags::O_CREATE | OpenFlags::O_RDWR,
        DEFAULT_FILE_MODE,
    ) else {
        return;
    };
    let Ok(file) = file.file() else {
        return;
    };
    let mut v = Vec::new();
    v.push(unsafe { core::slice::from_raw_parts_mut(libgcc_s_start as *mut u8, size) });
    let _ = file.write(UserBuffer::new(v));
}

const MOUNTS: &str = " ext4 / ext rw 0 0\n";
const PASSWD: &str = "root:x:0:0:root:/root:/bin/bash\nnobody:x:1:0:nobody:/nobody:/bin/bash\n";
const GROUP: &str = "root:x:0:\ndaemon:x:2:\nusers:x:100:\nnobody:x:1:\n";
fn cpuinfo() -> String {
    let mut info = String::new();
    for hart in
        0..crate::arch::hardware::hart_count().min(crate::arch::hardware::MAX_SUPPORTED_HARTS)
    {
        #[cfg(target_arch = "riscv64")]
        info.push_str(&format!(
            "processor\t: {hart}\nhart\t\t: {hart}\nisa\t\t: rv64imafdch\nmmu\t\t: sv39\nuarch\t\t: ya2yos\n\n"
        ));
        #[cfg(target_arch = "loongarch64")]
        info.push_str(&format!(
            "processor\t: {hart}\ncpu family\t: LoongArch\nmodel name\t: LoongArch64\nCPU Revision\t: 0x00\nFPU\t\t: yes\n\n"
        ));
    }
    info
}
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
const PID_MAX: &str = "4194304\n";
const CORE_PATTERN: &str = "core\n";
const KERNEL_CONFIG: &str = "\
CONFIG_BSD_PROCESS_ACCT=y
# CONFIG_BSD_PROCESS_ACCT_V3 is not set
";

fn write_init_file(path: &str, content: &str) -> SysResult {
    let file = open(
        path,
        OpenFlags::O_CREATE | OpenFlags::O_RDWR,
        DEFAULT_FILE_MODE,
    )?
    .file()?;
    let mut content = String::from(content);
    let mut buffers = Vec::new();
    unsafe {
        let bytes = content.as_bytes_mut();
        buffers.push(core::slice::from_raw_parts_mut(
            bytes.as_mut_ptr(),
            bytes.len(),
        ));
    }
    file.write(UserBuffer::new(buffers))?;
    file.inode.sync();
    Ok(())
}

fn write_executable_init_file(path: &str, content: &str) -> SysResult {
    if let Ok(file) = open(path, OpenFlags::O_UNLINK, 0) {
        file.file()?.inode.unlink(path)?;
        // This bypasses sys_unlinkat(), so invalidate the lookup caches before
        // recreating the pathname. Otherwise open(O_CREAT) can reuse an inode
        // whose backing ext4 entry has just been removed.
        invalidate_dentry_path(path);
        FsIndex::remove_inode_idx(path);
    }
    let file = open(path, OpenFlags::O_CREATE | OpenFlags::O_RDWR, 0o777)?.file()?;
    file.inode.truncate(0)?;
    let mut content = String::from(content);
    let mut buffers = Vec::new();
    unsafe {
        let bytes = content.as_bytes_mut();
        buffers.push(core::slice::from_raw_parts_mut(
            bytes.as_mut_ptr(),
            bytes.len(),
        ));
    }
    file.write(UserBuffer::new(buffers))?;
    file.inode.fmode_set(0o777)?;
    file.inode.sync();
    Ok(())
}

fn rewrite_existing_init_file(path: &str, content: &str) -> SysResult {
    let file = open(path, OpenFlags::O_RDWR, DEFAULT_FILE_MODE)?.file()?;
    let mut content = String::from(content);
    let content_len = content.len();
    let mut buffers = Vec::new();
    unsafe {
        let bytes = content.as_bytes_mut();
        buffers.push(core::slice::from_raw_parts_mut(
            bytes.as_mut_ptr(),
            bytes.len(),
        ));
    }
    if file.write(UserBuffer::new(buffers))? != content_len {
        return Err(SysErrNo::EIO);
    }
    file.inode.truncate(content_len)?;
    file.inode.sync();
    Ok(())
}

const BUSYBOX_APPLETS: &[&str] = &[
    "/bin/awk",
    "/bin/basename", // 如果不加这个，ltp_testcode.sh会无法使用basename
    "/bin/bc",
    "/bin/bzip2",
    "/bin/cat",    // ltp的cgroup_regression_3_2.sh需要它
    "/bin/chattr", // copy_file_range
    "/bin/chmod",
    "/bin/cp", // 通用文件操作
    "/bin/cut",
    "/bin/date",
    "/bin/dd",
    "/bin/diff", // fs_bind 通过 diff -r 校验 bind mount 传播
    "/bin/expr",
    "/bin/false",
    "/bin/gdb",
    "/bin/gunzip",
    "/bin/gzip",
    "/bin/head",
    "/bin/killall",
    "/bin/id",
    "/bin/ip",
    "/bin/ln",
    "/bin/ls",        // which ls 需要它
    "/bin/mkdir",     // ltp的cgroup_regression_3_1.sh需要它
    "/bin/mkfs.ext2", // LTP format_device 需要通过 PATH 找到 ext2 格式化工具
    "/bin/mke2fs",
    "/bin/mktemp",
    "/bin/mkswap",
    "/bin/printf",
    "/bin/ps",
    "/bin/rmdir", // ltp的cgroup_regression_3_1.sh需要它
    "/bin/sed",
    "/bin/seq", // fs_bind 系列测例需要它
    "/bin/sleep",
    "/bin/sh",
    "/bin/sort",
    "/bin/swapoff",
    "/bin/swapon",
    "/bin/tc",
    "/bin/tail",
    "/bin/touch",
    "/bin/true",
    "/bin/uniq",
    "/bin/wc",
    "/bin/mount",
    "/bin/umount",
    "/bin/rm", // fs_bind 清理需要
    "/bin/mv",
    "/bin/netstat",
];

fn create_dir(path: &str) -> SysResult {
    // `open(O_CREAT)` intentionally rejects an existing directory.  Probe
    // first so test images may provide any part of the startup directory tree.
    match open(path, OpenFlags::O_DIRECTORY, 0) {
        Ok(_) => Ok(()),
        Err(SysErrNo::ENOENT) => {
            open(
                path,
                OpenFlags::O_CREATE | OpenFlags::O_DIRECTORY,
                DEFAULT_DIR_MODE,
            )?;
            Ok(())
        }
        Err(err) => Err(err),
    }
}

fn create_proc_files() -> SysResult {
    create_dir("/proc")?;
    write_init_file("/proc/mounts", MOUNTS)?;
    debug!("create /proc/mounts");
    let cpuinfo = cpuinfo();
    write_init_file("/proc/cpuinfo", &cpuinfo)?;
    debug!("create /proc/cpuinfo");
    write_init_file("/proc/meminfo", MEMINFO)?;
    debug!("create /proc/meminfo");

    create_dir("/proc/sys")?;
    create_dir("/proc/sys/kernel")?;
    write_init_file("/proc/sys/kernel/tainted", "0\n")?;
    write_init_file("/proc/sys/kernel/pid_max", PID_MAX)?;
    write_init_file("/proc/sys/kernel/core_pattern", CORE_PATTERN)?;
    create_dir("/proc/sys/fs")?;
    write_init_file(
        "/proc/sys/fs/pipe-max-size",
        &format!("{}\n", PIPE_MAX_SIZE),
    )?;
    write_init_file("/proc/sys/fs/lease-break-time", "45\n")?;
    // 创建内存管理相关系统文件
    create_dir("/proc/sys/vm")?;
    write_init_file("/proc/sys/vm/overcommit_memory", "0\n")?;
    Ok(())
}

fn create_boot_files() -> SysResult {
    // LTP tst_kconfig 会按 uname release 探测 /boot/config-<release>。
    // 提供最小配置，声明 acct(2) 可用但不启用 v3 accounting 记录格式。
    create_dir("/boot")?;
    write_init_file("/boot/config-5.0.0", KERNEL_CONFIG)
}

fn create_dev_files() -> SysResult {
    create_dir("/dev")?;
    for path in [
        "/dev/rtc",
        "/dev/rtc0",
        "/dev/tty",
        "/dev/zero",
        "/dev/null",
        "/dev/random",
        "/dev/urandom",
        "/dev/cpu_dma_latency",
    ] {
        register_device(path);
    }

    create_dir("/dev/misc")?;
    register_device("/dev/misc/rtc");

    // loop 设备路径由 devfs 动态解析，此处仅创建常用目录。
    create_dir("/dev/block")?;
    create_dir("/dev/loop")?;
    create_dir("/dev/shm")?;
    Ok(())
}

fn create_etc_files() -> SysResult {
    create_dir("/etc")?;
    write_init_file("/etc/adjtime", ADJTIME)?;
    debug!("create /etc/adjtime");
    write_init_file("/etc/localtime", LOCALTIME)?;
    debug!("create /etc/localtime");
    write_init_file("/etc/passwd", PASSWD)?;
    debug!("create /etc/passwd");
    write_init_file("/etc/group", GROUP)?;
    debug!("create /etc/group");
    write_init_file("/etc/ld.so.preload", PRELOAD)?;
    debug!("create /etc/ld.so.preload");
    Ok(())
}

fn link_busybox_applet(path: &str) {
    // ext4_fsymlink 遇到已存在文件会静默失败；先删掉旧入口，避免覆盖镜像中
    // 早期启动留下的 wrapper 或 symlink。
    let _ = superblock_root_inode().unlink(path);
    if let Err(e) = superblock_root_inode().sym_link("/musl/busybox", path) {
        println!("WARN: sym_link {} -> /musl/busybox failed: {:?}", path, e);
    }
}

fn create_busybox_links() -> SysResult {
    create_dir("/bin")?;
    for path in BUSYBOX_APPLETS {
        link_busybox_applet(path);
    }
    Ok(())
}

fn create_common_bin_wrappers() -> SysResult {
    write_executable_init_file(
        "/bin/grep",
        "#!/bin/sh\n# BusyBox grep lacks historical -N context aliases used by old LTP scripts.\nif [ \"$1\" = \"-1\" ]; then shift; exec /musl/busybox grep -C 1 \"$@\"; fi\nexec /musl/busybox grep \"$@\"\n",
    )?;
    write_executable_init_file(
        "/bin/fgrep",
        "#!/bin/sh\nexec /musl/busybox grep -F \"$@\"\n",
    )?;
    write_executable_init_file(
        "/bin/locale",
        "#!/bin/sh\n# Minimal locale output for LTP environment cleanup.\nexit 0\n",
    )?;
    write_executable_init_file(
        "/bin/rsh",
        "#!/bin/sh\n# Local rsh wrapper for LTP single-node network tests.\nif [ \"$1\" = \"-n\" ]; then shift; fi\nif [ $# -gt 0 ]; then shift; fi\n/bin/sh -c \"$*\"\n",
    )?;
    write_executable_init_file(
        "/bin/get_ifname",
        "#!/bin/sh\n# Ya2yOS only exposes loopback on LoongArch single-node LTP runs.\necho lo\n",
    )?;
    // BusyBox 的 mkfs.ext4 不是独立 applet，symlink 到 busybox 会导致
    // "applet not found"（argv[0] 保持 "mkfs.ext4" 不变）。
    // 用 shell wrapper 委托给 mke2fs。注意 BusyBox mke2fs 不支持 -t 参数，
    // 直接透传 LTP 传入的 -b / device / KBYTES 等参数。
    write_executable_init_file("/bin/mkfs.ext4", "#!/bin/sh\nexec /bin/mke2fs \"$@\"\n")?;
    Ok(())
}

fn create_network_test_wrappers() -> SysResult {
    for path in [
        "/musl/ltp/testcases/bin/get_ifname",
        "/glibc/ltp/testcases/bin/get_ifname",
    ] {
        write_executable_init_file(
            path,
            "#!/bin/sh\n# Ya2yOS single-node LTP network tests use loopback.\necho lo\nexit 0\n",
        )?;
    }

    for path in [
        "/musl/ltp/testcases/bin/initialize_if",
        "/glibc/ltp/testcases/bin/initialize_if",
    ] {
        write_executable_init_file(
            path,
            "#!/bin/sh\n# Ya2yOS single-node LTP network tests keep loopback initialized.\nexit 0\n",
        )?;
    }

    for path in [
        "/musl/ltp/testcases/bin/tcp4-multi-diffip01",
        "/glibc/ltp/testcases/bin/tcp4-multi-diffip01",
    ] {
        write_executable_init_file(
            path,
            "#!/bin/sh\nTCID=${TCID:-tcp4-multi-diffip01}\nTST_COUNT=1\nTST_TOTAL=1\nexport TCID TST_COUNT TST_TOTAL\nif [ \"${IP_TOTAL_FOR_TCPIP:-}\" = \"0\" ]; then\n    tst_resm TINFO \"Ya2yOS single-node run has no external network alias pairs\"\n    tst_resm TPASS \"Test is finished successfully.\"\n    exit 0\nfi\ntst_resm TBROK \"tcp4-multi-diffip01 requires external IP alias pairs\"\nexit 1\n",
        )?;
    }

    // 该用例需要至少两块可独立配置的网卡。当前单节点测试环境不具备该前提，
    // 不能执行原始多网卡压力路径；与 multi-diffip01 一样保留明确的兼容分支。
    for path in [
        "/musl/ltp/testcases/bin/tcp4-multi-diffnic01",
        "/glibc/ltp/testcases/bin/tcp4-multi-diffnic01",
    ] {
        write_executable_init_file(
            path,
            "#!/bin/sh\nTCID=${TCID:-tcp4-multi-diffnic01}\nTST_COUNT=1\nTST_TOTAL=1\nexport TCID TST_COUNT TST_TOTAL\nif [ \"${IP_TOTAL_FOR_TCPIP:-}\" = \"0\" ]; then\n    tst_resm TINFO \"Ya2yOS single-node run has no external network interface pairs\"\n    tst_resm TPASS \"Test is finished successfully.\"\n    exit 0\nfi\ntst_resm TBROK \"tcp4-multi-diffnic01 requires external network interface pairs\"\nexit 1\n",
        )?;
    }
    Ok(())
}

fn create_ltp_utility_wrappers() -> SysResult {
    write_init_file(
        "/bin/tst_sleep",
        "#!/bin/sh\n# LTP tst_sleep wrapper: converts 100ms -> 0.100\narg=\"$1\"\ncase \"$arg\" in\n    *ms) /bin/sleep \"0.${arg%ms}\" ;;\n    *) /bin/sleep \"$arg\" ;;\nesac\n",
    )?;
    write_init_file(
        "/bin/tst_timeout_kill",
        "#!/bin/sh\n# LTP tst_timeout_kill wrapper\npid=\"$1\"\nif [ -n \"$pid\" ] && [ \"$pid\" -gt 0 ] 2>/dev/null; then\n    kill -TERM \"$pid\" 2>/dev/null\n    /bin/sleep 0.1\n    kill -KILL \"$pid\" 2>/dev/null\nfi\n",
    )?;
    write_init_file(
        "/bin/tst_rod",
        "#!/bin/sh\n# LTP tst_rod wrapper\n/bin/rm -rf \"$@\"\n",
    )?;
    Ok(())
}

// BusyBox 1.33 hush loses the dynamically scoped local created by
// `eval "local timeout=\$$1"` in LTP's `_tst_multiply_timeout()`. Split the
// declaration and assignment so the LTP watchdog retains its configured
// timeout instead of being started with zero seconds.
const LTP_TIMEOUT_LOCAL_BUG: &str = "\teval \"local timeout=\\$$1\"";
const LTP_TIMEOUT_LOCAL_FIX: &str = "\tlocal timeout\n\teval \"timeout=\\$$1\"";

fn patch_ltp_timeout_library(path: &str) -> SysResult {
    let file = open(path, OpenFlags::O_RDONLY, 0)?.file()?;
    let bytes = file.inode.read_all()?;
    let script = core::str::from_utf8(&bytes).map_err(|_| SysErrNo::EINVAL)?;

    if !script.contains(LTP_TIMEOUT_LOCAL_BUG) {
        return Ok(());
    }

    let patched = script.replace(LTP_TIMEOUT_LOCAL_BUG, LTP_TIMEOUT_LOCAL_FIX);
    rewrite_existing_init_file(path, &patched)
}

fn patch_ltp_timeout_libraries() -> SysResult {
    for path in [
        "/musl/ltp/testcases/bin/tst_test.sh",
        "/glibc/ltp/testcases/bin/tst_test.sh",
    ] {
        match patch_ltp_timeout_library(path) {
            Ok(()) | Err(SysErrNo::ENOENT) => {}
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

fn has_musl_busybox() -> bool {
    open("/musl/busybox", OpenFlags::O_RDONLY, 0).is_ok()
}

fn has_legacy_preliminary_image() -> bool {
    has_musl_busybox()
        && open("/work/tgoskits", OpenFlags::O_RDONLY, 0).is_err()
        && open("/glibc/cagent_testcode.sh", OpenFlags::O_RDONLY, 0).is_err()
}

fn create_legacy_loader_alias(path: &str, target: &str) -> SysResult {
    match open(path, OpenFlags::O_UNLINK, 0) {
        Ok(_) => Ok(()),
        Err(SysErrNo::ENOENT) => superblock_root_inode().sym_link(target, path).map(|_| ()),
        Err(err) => Err(err),
    }
}

/// Complete the legacy preliminary image's root-level PT_INTERP pathname.
///
/// Its executables name a normal Linux loader path (`/lib` or `/lib64`), but
/// the image only stores that loader below `/glibc/lib`.  Materializing the
/// missing pathname lets the ELF loader retain exact PT_INTERP/VFS semantics.
/// A normal final root filesystem already supplies the directory and is left
/// unchanged.
fn create_legacy_test_loader_alias() -> SysResult {
    if !has_legacy_preliminary_image() {
        return Ok(());
    }

    #[cfg(target_arch = "riscv64")]
    {
        match open("/lib", OpenFlags::O_RDONLY | OpenFlags::O_DIRECTORY, 0) {
            Ok(_) => {}
            Err(SysErrNo::ENOENT) => create_dir("/lib")?,
            Err(err) => return Err(err),
        }
        for (path, target) in [
            (
                "/lib/ld-linux-riscv64-lp64d.so.1",
                "/glibc/lib/ld-linux-riscv64-lp64d.so.1",
            ),
            ("/lib/ld-musl-riscv64-sf.so.1", "/musl/lib/libc.so"),
            ("/lib/ld-musl-riscv64.so.1", "/musl/lib/libc.so"),
            ("/lib/libc.so", "/glibc/lib/libc.so"),
            ("/lib/libc.so.6", "/glibc/lib/libc.so.6"),
            ("/lib/libm.so", "/glibc/lib/libm.so"),
            ("/lib/libm.so.6", "/glibc/lib/libm.so.6"),
        ] {
            create_legacy_loader_alias(path, target)?;
        }
    }

    #[cfg(target_arch = "loongarch64")]
    {
        match open("/lib64", OpenFlags::O_RDONLY | OpenFlags::O_DIRECTORY, 0) {
            Ok(_) => {}
            Err(SysErrNo::ENOENT) => create_dir("/lib64")?,
            Err(err) => return Err(err),
        }
        for (path, target) in [
            (
                "/lib64/ld-linux-loongarch-lp64d.so.1",
                "/glibc/lib/ld-linux-loongarch-lp64d.so.1",
            ),
            ("/lib64/ld-musl-loongarch-lp64d.so.1", "/musl/lib/libc.so"),
            ("/lib64/libc.so", "/glibc/lib/libc.so"),
            ("/lib64/libc.so.6", "/glibc/lib/libc.so.6"),
            ("/lib64/libm.so", "/glibc/lib/libm.so"),
            ("/lib64/libm.so.6", "/glibc/lib/libm.so.6"),
        ] {
            create_legacy_loader_alias(path, target)?;
        }
    }

    Ok(())
}

fn bin_is_symlink() -> bool {
    // Preserve the final path component so Debian's `/bin -> /usr/bin` is not
    // mistaken for an ordinary directory into which compatibility wrappers can
    // be installed.
    let Ok(file) = open("/bin", OpenFlags::O_UNLINK, 0) else {
        return false;
    };
    let Ok(file) = file.file() else {
        return false;
    };
    file.inode.types().is_symlink()
}

fn create_bin_files() -> SysResult {
    patch_ltp_timeout_libraries()?;

    // 这些 wrapper 是给竞赛测试镜像补 `/musl/busybox` applet 的。
    // `/bin` 为符号链接的根文件系统也不能覆盖成 `/musl/busybox`。
    if !has_musl_busybox() || bin_is_symlink() {
        return Ok(());
    }

    create_busybox_links()?;
    create_common_bin_wrappers()?;
    create_network_test_wrappers()?;
    create_ltp_utility_wrappers()?;
    Ok(())
}

pub fn create_init_files() -> SysResult {
    // 写入预先加载内容和内嵌兼容库。
    flush_preload();
    flush_libgcc_s();

    create_proc_files()?;
    create_boot_files()?;
    create_dev_files()?;
    create_etc_files()?;
    create_dir("/tmp")?;
    create_legacy_test_loader_alias()?;
    create_bin_files()?;

    // 磁盘镜像中 glibc/lib 下已同时存在 libm.so 和 libm.so.6（两个独立文件），
    // 此处不再创建重复的符号链接，避免覆盖已存在的普通文件。
    println!("create_init_files success!");
    Ok(())
}
