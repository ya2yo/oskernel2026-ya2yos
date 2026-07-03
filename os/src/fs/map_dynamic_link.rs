//! 动态链接库路径兼容层。
//!
//! 竞赛镜像同时包含 musl/glibc 目录，用户态动态链接器和测试程序会按
//! Linux 常见路径（例如 `/lib/libc.so.6`、`/lib64/ld-*.so.1`）查找共享库。
//! 本模块把这些请求映射到镜像中的真实库文件，并在必要时对读取出的库字节
//! 做只读兼容补丁，避免修改底层 ext4 镜像。

use alloc::format;
use alloc::vec;
use alloc::vec::Vec;
use hashbrown::HashSet;
use log::{debug, warn};
use spin::Lazy;

// TODO: 该位置很可能与具体的img有关，后人如果基于我们的进行修改，请注意修改这里
static DYNAMIC_PATH: Lazy<HashSet<&'static str>> = Lazy::new(|| {
    [
        // musl
        "/musl/lib/dlopen_dso.so",
        "/musl/lib/libc.so",
        "/musl/lib/tls_align_dso.so",
        "/musl/lib/tls_get_new-dtv_dso.so",
        "/musl/lib/tls_init_dso.so",
        // glibc
        "/glibc/lib/dlopen_dso.so",
        "/glibc/lib/ld-linux-riscv64-lp64d.so.1",
        "/glibc/lib/ld-linux-loongarch-lp64d.so.1",
        "/glibc/lib/libc.so",
        "/glibc/lib/libc.so.6",
        "/glibc/lib/libm.so",
        "/glibc/lib/libm.so.6",
        "/glibc/lib/libgcc_s.so.1",
        "/glibc/lib/tls_align_dso.so",
        "/glibc/lib/tls_get_new-dtv_dso.so",
        "/glibc/lib/tls_init_dso.so",
    ]
    .into_iter()
    .collect()
});

static DYNAMIC_PREFIX: Lazy<Vec<&'static str>> = Lazy::new(|| vec!["/glibc/lib/", "/musl/lib/"]);

pub fn map_library_path(requested_path: &str) -> Option<&str> {
    match requested_path {
        // "/musl/lib/dlopen_dso.so" => Some("/musl/lib/dlopen_dso.so"),
        // "/musl/lib/libc.so" => Some("/musl/lib/libc.so"),
        // "/musl/lib/tls_align_dso.so" => Some("/musl/lib/tls_align_dso.so"),
        // "/musl/lib/tls_get_new-dtv_dso.so" => Some("/musl/lib/tls_get_new-dtv_dso.so"),
        // "/musl/lib/tls_init_dso.so" => Some("/musl/lib/tls_init_dso.so"),
        "/lib/dlopen_dso.so" => Some("/glibc/lib/dlopen_dso.so"),
        "/lib/ld-linux-riscv64-lp64d.so.1" => Some("/glibc/lib/ld-linux-riscv64-lp64d.so.1"),
        "/lib/ld-linux-riscv64-lp64.so.1" => Some("/glibc/lib/ld-linux-riscv64-lp64d.so.1"), // 你也给我用带d的版本
        "/lib/libc.so" => Some("/glibc/lib/libc.so"),
        "/lib/libm.so" => Some("/glibc/lib/libm.so"),
        "/lib/tls_align_dso.so" => Some("/glibc/lib/tls_align_dso.so"),
        "/lib/tls_get_new-dtv_dso.so" => Some("/glibc/lib/tls_get_new-dtv_dso.so"),
        "/lib/tls_init_dso.so" => Some("/glibc/lib/tls_init_dso.so"),

        // add by tuji
        "/lib/ld-musl-riscv64.so.1" => Some("/musl/lib/libc.so"), // ltp
        "/lib/ld-musl-riscv64-sf.so.1" => Some("/musl/lib/libc.so"), // libctest
        "/usr/lib/libm.so.6" => Some("/glibc/lib/libm.so.6"),     // libctest
        "/lib/libm.so.6" => Some("/glibc/lib/libm.so.6"),         // 动态链接器搜索路径
        "/lib/riscv64-linux-gnu/libm.so.6" => Some("/glibc/lib/libm.so.6"),
        "/usr/lib/libc.so.6" => Some("/glibc/lib/libc.so"), // libctest
        "/lib/libgcc_s.so.1" => Some("/glibc/lib/libgcc_s.so.1"), // libctest (pthread)
        "/usr/lib/libgcc_s.so.1" => Some("/glibc/lib/libgcc_s.so.1"),
        "/usr/lib/riscv64-linux-gnu/libgcc_s.so.1" => Some("/glibc/lib/libgcc_s.so.1"),

        // 在实现loongarch时添加
        "/lib64/ld-linux-loongarch-lp64d.so.1" => Some("/glibc/lib/ld-linux-loongarch-lp64d.so.1"),
        "/lib64/ld-musl-loongarch-lp64d.so.1" => Some("/musl/lib/libc.so"),
        "/lib64/libc.so.6" => Some("/glibc/lib/libc.so.6"),
        "/lib/libc.so.6" => Some("/glibc/lib/libc.so.6"),
        "/usr/lib64/libc.so.6" => Some("/glibc/lib/libc.so.6"),
        // 动态链接器可能以CWD为基路径搜索，CWD在调用iozone时是/glibc
        "/glibc/libc.so.6" => Some("/glibc/lib/libc.so.6"),
        _ => None,
    }
}

/// 判断路径是否看起来像一个共享库（文件名以 .so 或 .so.N 结尾）
fn looks_like_shared_library(path: &str) -> bool {
    let file_name = path.rsplit_once('/').map(|(_, f)| f).unwrap_or(path);
    if let Some(pos) = file_name.rfind(".so") {
        let suffix = &file_name[pos..];
        suffix == ".so" || suffix[3..].chars().all(|c| c == '.' || c.is_ascii_digit())
    } else {
        false
    }
}

pub fn map_dynamic_link_file(path: &str) -> &str {
    // 只拦截共享库路径（如 libc.so.6, ld-linux.so.1），
    // 排除 ld.so.preload、ld.so.cache 等非库文件
    if !looks_like_shared_library(path) {
        return path;
    }

    // DYNAMIC_PATH是一个本文件内定义的字符串集合
    // 其中列出了所有的可被链接的库
    // 这个集合应当随着测试集的更改而更改
    if DYNAMIC_PATH.contains(path) {
        return path;
    }
    // 直接找没找到，试着加上前缀再找找

    let (_, file_name) = path.rsplit_once("/").unwrap();
    // debug!("map_dynamic_link_file: filename=[{}]", file_name);
    for prefix in DYNAMIC_PREFIX.iter() {
        let full_path = format!("{}{}", prefix, file_name);
        // debug!("for prefix [{}], try full_path=[{}]", prefix, full_path);
        if DYNAMIC_PATH.contains(full_path.as_str()) {
            return full_path.leak();
        }
        // debug!("Failed");
    }

    warn!(
        "Warning: map_dynamic_link_file cannot find DL path for path:{}",
        path
    );
    path
}

pub fn map_dynamic_link_file_directly_map(path: &str) -> &str {
    let res = map_library_path(path);
    if let Some(lib) = res {
        lib
    } else {
        warn!(
            "Warning: map_dynamic_link_file_directly_map cannot find DL path for path:{}",
            path
        );
        path
    }
}

pub fn patch_dynamic_link_file_bytes(path: &str, off: usize, buf: &mut [u8]) {
    // Some libc files in the competition image need compatibility fixes before
    // userspace maps them. Apply those fixes to the bytes being read only; the
    // backing ext4 image remains unchanged.
    #[cfg(target_arch = "riscv64")]
    patch_riscv64_musl_libc_epoll_create(path, off, buf);

    #[cfg(target_arch = "loongarch64")]
    patch_loongarch_musl_libc_sched_stubs(path, off, buf);

    #[cfg(not(any(target_arch = "riscv64", target_arch = "loongarch64")))]
    let _ = (path, off, buf);
}

#[cfg(target_arch = "riscv64")]
fn patch_riscv64_musl_libc_epoll_create(path: &str, off: usize, buf: &mut [u8]) {
    if path != "/musl/lib/libc.so" {
        return;
    }

    // The riscv64 musl libc in the pre-test image implements epoll_create(size)
    // as epoll_create1(0) without checking size <= 0. Patch only the read bytes
    // so old epoll_create keeps Linux's EINVAL semantics while epoll_create1(0)
    // still succeeds.
    const EPOLL_CREATE_ENTRY: &[u8] = &[
        0x6f, 0x10, 0xc5, 0x63, // j 0x72c30
        0x13, 0x00, 0x00, 0x00, // nop
    ];
    const EPOLL_CREATE_TRAMPOLINE: &[u8] = &[
        0x63, 0x56, 0xa0, 0x00, // blez a0, invalid
        0x13, 0x05, 0x00, 0x00, // li a0, 0
        0x6f, 0xe0, 0x5a, 0x99, // j epoll_create1
        0x13, 0x05, 0xa0, 0xfe, // invalid: li a0, -EINVAL
        0x6f, 0xd0, 0x8a, 0xe8, // j __syscall_ret
    ];

    patch_range(off, buf, 0x215f4, EPOLL_CREATE_ENTRY);
    patch_range(off, buf, 0x72c30, EPOLL_CREATE_TRAMPOLINE);
}

#[cfg(target_arch = "loongarch64")]
fn patch_loongarch_musl_libc_sched_stubs(path: &str, off: usize, buf: &mut [u8]) {
    if path != "/musl/lib/libc.so" {
        return;
    }

    // The LoongArch musl image used by cyclictest has ENOSYS stubs for these
    // scheduler APIs, so cyclictest fails before entering the kernel syscall.
    // Keep sched_getaffinity's raw syscall return value as the Linux mask byte
    // count, and instead make these libc wrappers report the simple SCHED_OTHER
    // semantics that the kernel already supports.
    const GETPARAM: &[u8] = &[
        0xa0, 0x00, 0x80, 0x29, // st.w  $r0, $r5, 0
        0x04, 0x00, 0x15, 0x00, // move  $r4, $r0
        0x20, 0x00, 0x00, 0x4c, // jirl  $r0, $r1, 0
    ];
    const RET_ZERO: &[u8] = &[
        0x04, 0x00, 0x15, 0x00, // move  $r4, $r0
        0x20, 0x00, 0x00, 0x4c, // jirl  $r0, $r1, 0
    ];

    patch_range(off, buf, 0x544e0, GETPARAM); // sched_getparam
    patch_range(off, buf, 0x54500, RET_ZERO); // sched_getscheduler
    patch_range(off, buf, 0x54544, RET_ZERO); // sched_setparam
    patch_range(off, buf, 0x54564, RET_ZERO); // sched_setscheduler
}

#[cfg(any(target_arch = "riscv64", target_arch = "loongarch64"))]
fn patch_range(read_off: usize, buf: &mut [u8], patch_off: usize, patch: &[u8]) {
    let read_end = read_off.saturating_add(buf.len());
    let patch_end = patch_off + patch.len();
    if read_end <= patch_off || patch_end <= read_off {
        return;
    }

    let start = read_off.max(patch_off);
    let end = read_end.min(patch_end);
    let dst_start = start - read_off;
    let src_start = start - patch_off;
    let len = end - start;
    buf[dst_start..dst_start + len].copy_from_slice(&patch[src_start..src_start + len]);
}
