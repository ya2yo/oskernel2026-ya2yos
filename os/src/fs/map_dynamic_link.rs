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
        "/usr/lib/libm.so.6" => Some("/glibc/lib/libm.so"),       // libctest
        "/usr/lib/libc.so.6" => Some("/glibc/lib/libc.so"),       // libctest

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

pub fn map_dynamic_link_file(path: &str) -> &str {
    // DYNAMIC_PATH是一个本文件内定义的字符串集合
    // 其中列出了所有的可被链接的库
    // 这个集合应当随着测试集的更改而更改
    if DYNAMIC_PATH.contains(path) {
        return path;
    }
    // 直接找没找到，试着加上前缀再找找

    let (_, file_name) = path.rsplit_once("/").unwrap();
    debug!("map_dynamic_link_file: filename=[{}]", file_name);
    for prefix in DYNAMIC_PREFIX.iter() {
        let full_path = format!("{}{}", prefix, file_name);
        debug!("for prefix [{}], try full_path=[{}]", prefix, full_path);
        if DYNAMIC_PATH.contains(full_path.as_str()) {
            return full_path.leak();
        }
        debug!("Failed");
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
