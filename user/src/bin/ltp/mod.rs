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

const LTP_TFAIL: i32 = 0x01;
const LTP_TBROK: i32 = 0x02;
const LTP_TWARN: i32 = 0x04;
const LTP_TCONF: i32 = 0x20;

#[derive(Default)]
struct LtpSummary {
    passed: usize,
    failed: usize,
    broken: usize,
    skipped: usize,
    warnings: usize,
}

impl LtpSummary {
    fn record_skipped(&mut self) {
        self.skipped += 1;
    }

    fn record_wait_status(&mut self, wait_status: i32) {
        let exit_code = ltp_exit_code(wait_status);
        if exit_code == 0 {
            self.passed += 1;
            return;
        }

        let mut recognized = false;
        if exit_code & LTP_TFAIL != 0 {
            self.failed += 1;
            recognized = true;
        }
        if exit_code & LTP_TBROK != 0 {
            self.broken += 1;
            recognized = true;
        }
        if exit_code & LTP_TCONF != 0 {
            self.skipped += 1;
            recognized = true;
        }
        if exit_code & LTP_TWARN != 0 {
            self.warnings += 1;
            recognized = true;
        }
        if !recognized {
            self.broken += 1;
        }
    }

    fn print(&self) {
        println!("Summary:");
        println!("passed   {}", self.passed);
        println!("failed   {}", self.failed);
        println!("broken   {}", self.broken);
        println!("skipped  {}", self.skipped);
        println!("warnings {}", self.warnings);
    }
}

fn ltp_exit_code(wait_status: i32) -> i32 {
    // Ya2yOS waitpid stores normal exits as Linux-style status << 8, while
    // signal exits in the 128..255 range are already unshifted.
    if wait_status >= 128 && wait_status <= 255 {
        wait_status
    } else {
        (wait_status >> 8) & 0xff
    }
}

// musl
#[allow(unused)]
pub fn run_ltp_tests_musl(tests: &[&str], blacklist: &[&str]) {
    println!("#### OS COMP TEST GROUP START ltp-musl ####");
    let mut summary = LtpSummary::default();
    for &test in tests {
        if blacklist.contains(&test) {
            println!("SKIP LTP CASE {}", trim_trailing_nul(test));
            summary.record_skipped();
            continue;
        }
        println!("RUN LTP CASE {}", trim_trailing_nul(test));

        let r = fork_and_run("/musl/ltp/testcases/bin\0", &[test]);
        summary.record_wait_status(r);
        println!("FAIL LTP CASE {} : {}", trim_trailing_nul(test), r);
    }
    summary.print();
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

        let mut summary = LtpSummary::default();
        let mut j = group_start;
        while j < group_end {
            let test = tests[j];
            if blacklist.contains(&test) {
                println!("SKIP LTP CASE {}", trim_trailing_nul(test));
                summary.record_skipped();
                j += 1;
                continue;
            }
            println!("RUN LTP CASE {}", trim_trailing_nul(test));
            let r = fork_and_run("/musl/ltp/testcases/bin\0", &[test]);
            summary.record_wait_status(r);
            println!("FAIL LTP CASE {} : {}", trim_trailing_nul(test), r);
            j += 1;
        }
        summary.print();
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
    println!("RUN MUSL LTP SINGLE CASE {}", trim_trailing_nul(test_name));
    let r = fork_and_run("/musl/ltp/testcases/bin\0", &[test_name]);
    println!(
        "RESULT MUSL LTP SINGLE CASE {} : {}",
        trim_trailing_nul(test_name),
        r
    );
    let mut summary = LtpSummary::default();
    summary.record_wait_status(r);
    summary.print();
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
    let mut summary = LtpSummary::default();
    for &test in tests {
        if blacklist.contains(&test) {
            println!("SKIP LTP CASE {}", trim_trailing_nul(test));
            summary.record_skipped();
            continue;
        }
        println!("RUN LTP CASE {}", trim_trailing_nul(test));
        let r = fork_and_run("/glibc/ltp/testcases/bin\0", &[test]);
        summary.record_wait_status(r);
        println!("FAIL LTP CASE {} : {}", trim_trailing_nul(test), r);
    }
    summary.print();
    println!("#### OS COMP TEST GROUP END ltp-glibc ####");
}

/// 单独测试 glibc 版本下某个特定的 LTP 测例。
/// **注意：test_name 必须以 `\0` 结尾**（C 字符串约定），否则 execve 会失败。
///
/// 用法：`ltp::test_glibc_single("brk01\0")`
#[allow(unused)]
pub fn test_glibc_single(test_name: &str) {
    println!("RUN GLIBC LTP SINGLE CASE {}", trim_trailing_nul(test_name));
    let r = fork_and_run("/glibc/ltp/testcases/bin\0", &[test_name]);
    println!(
        "RESULT GLIBC LTP SINGLE CASE {} : {}",
        trim_trailing_nul(test_name),
        r
    );
    let mut summary = LtpSummary::default();
    summary.record_wait_status(r);
    summary.print();
}


/// 单独运行 glibc 版本下指定的一组测例（自定义列表）
/// 用法：ltp::test_glibc_custom(&["brk01\0", "brk02\0", "mmap01\0"])
#[allow(unused)]
pub fn test_glibc_custom() {
    println!("===== GLIBC LTP CUSTOM TESTS START =====");
    run_ltp_tests_glibc(&ltp::FILELIST[LTP_TEST_START..LTP_TEST_START+LTP_TESTS_PER_GROUP], LTP_BLACKLIST);
    println!("===== GLIBC LTP CUSTOM TESTS END =====");
}
