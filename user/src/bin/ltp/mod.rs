use crate::*;
mod blacklist;
mod filelist;
pub use blacklist::LTP_BLACKLIST;
pub use filelist::FILELIST;

// 通用工具
#[allow(unused)]
fn trim_trailing_nul(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut end = bytes.len();
    if end > 0 && bytes[end - 1] == 0 {
        end -= 1;
    }
    unsafe { core::str::from_utf8_unchecked(&bytes[..end]) }
}

// musl
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

const LTP_TEST_START: usize = 0;
const LTP_TESTS_PER_GROUP: usize = 2820;

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
pub fn test_musl_ltp() {
    let test = &FILELIST;
    run_ltp_tests_musl_separately(test, LTP_BLACKLIST);
}
#[allow(unused)]
pub fn test_musl_single(test_name: &str) {
    println!("RUN MUSL LTP SINGLE CASE {}", test_name);
    let r = fork_and_run("/glibc/ltp/testcases/bin\0", &[test_name]);
    println!("RESULT MUSL LTP SINGLE CASE {} : {}", test_name, r);
}

// glibc
#[allow(unused)]
pub fn test_glibc_ltp() {
    let test = &FILELIST;
    run_ltp_tests_glibc(test, LTP_BLACKLIST);
}

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


/// 单独运行 glibc 版本下指定的一组测例（自定义列表）
/// 用法：ltp::test_glibc_custom(&["brk01\0", "brk02\0", "mmap01\0"])
#[allow(unused)]
pub fn test_glibc_custom() {
    println!("===== GLIBC LTP CUSTOM TESTS START =====");
    run_ltp_tests_glibc(&ltp::FILELIST[LTP_TEST_START..LTP_TEST_START+LTP_TESTS_PER_GROUP], LTP_BLACKLIST);
    println!("===== GLIBC LTP CUSTOM TESTS END =====");
}
