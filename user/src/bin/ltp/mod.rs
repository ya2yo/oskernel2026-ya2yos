use crate::*;
mod blacklist;
mod filelist;
pub use blacklist::{LTP_BLACKLIST, LTP_CGROUP_PREFIX_LEN};
pub use filelist::FILELIST;

// ---------------------------------------------------------------------------
// 通用工具
// ---------------------------------------------------------------------------
#[allow(unused)]
fn trim_trailing_nul(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut end = bytes.len();
    if end > 0 && bytes[end - 1] == 0 {
        end -= 1;
    }
    unsafe { core::str::from_utf8_unchecked(&bytes[..end]) }
}

// ---------------------------------------------------------------------------
// musl
// ---------------------------------------------------------------------------

#[allow(unused)]
pub fn run_ltp_tests_musl(tests: &[&str], blacklist: &[&str]) {
    println!("#### OS COMP TEST GROUP START ltp-musl ####");
    for &test in tests {
        if blacklist.contains(&test) {
            continue;
        }
        println!("RUN LTP CASE {}", test);

        let r = fork_and_run("/musl/ltp/testcases/bin\0", &[test]);
        println!("FAIL LTP CASE {} : {}", test, r); // 这不是表示失败了，这只是告诉外界程序返回值是多少而已
    }
    println!("#### OS COMP TEST GROUP END ltp-musl ####");
}

#[allow(unused)]
pub fn check_ltp_tests_musl(tests: &[&str], blacklist: &[&str]) {
    println!("#### OS COMP TEST GROUP START ltp-musl ####");
    for &test in tests {
        if !blacklist.contains(&test) {
            continue;
        }
        println!("RUN LTP CASE {}", test);

        let r = fork_and_run("/musl/ltp/testcases/bin\0", &[test]);
        println!("FAIL LTP CASE {} : {}", test, r); // 这不是表示失败了，这只是告诉外界程序返回值是多少而已
    }
    println!("#### OS COMP TEST GROUP END ltp-musl ####");
}

const LTP_TEST_START: usize = 2100;
const LTP_TESTS_PER_GROUP: usize = 50;

#[allow(unused)]
pub fn run_ltp_tests_musl_separately(tests: &[&str], blacklist: &[&str]) {
    let mut group = 0;
    let mut i = 0;
    while i < tests.len() {
        let group_start = i;
        let mut group_end = i + LTP_TESTS_PER_GROUP;
        if group_end > tests.len() {
            group_end = tests.len();
        }

        println!(
            "#### OS COMP TEST GROUP START ltp-musl ####"
        );

        let mut j = group_start;
        while j < group_end {
            let test = tests[j];
            if blacklist.contains(&test) {
                println!("SKIP LTP CASE {}", trim_trailing_nul(test));
                j += 1;
                continue;
            }
            println!("RUN LTP CASE {}", test);
            let r = fork_and_run("/musl/ltp/testcases/bin\0", &[test]);
            println!("FAIL LTP CASE {} : {}", test, r);
            j += 1;
        }

        println!(
            "#### OS COMP TEST GROUP END ltp-musl ####"
        );
        group += 1;
        i = group_end;
    }
}

#[allow(unused)]
pub fn test_ltp() {
    let test = &FILELIST[LTP_TEST_START..LTP_TEST_START + LTP_TESTS_PER_GROUP];
    run_ltp_tests_musl_separately(test, LTP_BLACKLIST);
}

#[allow(unused)]
pub fn check_ltp() {
    let test = &FILELIST[..];
    check_ltp_tests_musl(test, &LTP_BLACKLIST[LTP_CGROUP_PREFIX_LEN..]);
}

// ---------------------------------------------------------------------------
// glibc
// ---------------------------------------------------------------------------

/// glibc 版本的黑名单（与 musl 共用同一份 FILELIST，但可能有些测试在 glibc 下行为不同）
#[allow(unused)]
pub const GLIBC_LTP_BLACKLIST: &[&str] = &[
    "cgroup_fj_common.sh\0",
    "cgroup_fj_function.sh\0",
    "cgroup_fj_proc\0",
    "cgroup_fj_stress.sh\0",
    "cgroup_lib.sh\0",
];

/// 内存相关 LTP 测试用例（brk, mmap, munmap, mprotect, madvise, mlock 等）
/// 用于单独验证内存管理子系统的正确性
#[allow(unused)]
pub const LTP_MEMORY_TESTS: &[&str] = &[
    "brk01\0",
    "brk02\0",
    "mmap01\0",
    "mmap02\0",
    "mmap03\0",
    "mmap04\0",
    "mmap05\0",
    "mmap06\0",
    "mmap08\0",
    "mmap09\0",
    "mmap10\0",
    "mmap11\0",
    "mmap001\0",
    "mmap1\0",
    "mmap2\0",
    "mmap3\0",
    "mmap13\0",
    "mmap14\0",
    "mmap16\0",
    "mmap19\0",
    "mmapstress02\0",
    "mprotect01\0",
    "madvise01\0",
    "mlock01\0",
    "mlock02\0",
    "mlock03\0",
    "mlock04\0",
    "munlock01\0",
];

#[allow(unused)]
pub fn run_ltp_tests_glibc(tests: &[&str], blacklist: &[&str]) {
    println!("#### OS COMP TEST GROUP START ltp-glibc ####");
    for &test in tests {
        if blacklist.contains(&test) {
            continue;
        }
        println!("RUN LTP CASE {}", test);
        let r = fork_and_run("/glibc/ltp/testcases/bin\0", &[test]);
        println!("FAIL LTP CASE {} : {}", test, r); // 这不是表示失败了，这只是告诉外界程序返回值是多少而已
    }
    println!("#### OS COMP TEST GROUP END ltp-glibc ####");
}

/// 单独测试 glibc 版本下某个特定的 LTP 测例。
/// **注意：test_name 必须以 `\0` 结尾**（C 字符串约定），否则 execve 会失败。
///
/// 用法：`ltp::test_glibc_single("brk01\0")`
#[allow(unused)]
pub fn test_glibc_single(test_name: &str) {
    println!("RUN GLIBC LTP SINGLE CASE {}", test_name);
    let r = fork_and_run("/glibc/ltp/testcases/bin\0", &[test_name]);
    println!("RESULT GLIBC LTP SINGLE CASE {} : {}", test_name, r);
}

/// 按顺序逐个运行 glibc 版本的内存相关 LTP 测试用例
/// 用于排查 brk/mmap/munmap 等内存管理 bug
#[allow(unused)]
pub fn test_glibc_memory() {
    println!("===== GLIBC LTP MEMORY TESTS START =====");
    run_ltp_tests_glibc(LTP_MEMORY_TESTS, GLIBC_LTP_BLACKLIST);
    println!("===== GLIBC LTP MEMORY TESTS END =====");
}

/// 单独运行 glibc 版本下指定的一组测例（自定义列表）
/// 用法：ltp::test_glibc_custom(&["brk01\0", "brk02\0", "mmap01\0"])
#[allow(unused)]
pub fn test_glibc_custom(tests: &[&str]) {
    println!("===== GLIBC LTP CUSTOM TESTS START =====");
    run_ltp_tests_glibc(tests, GLIBC_LTP_BLACKLIST);
    println!("===== GLIBC LTP CUSTOM TESTS END =====");
}
