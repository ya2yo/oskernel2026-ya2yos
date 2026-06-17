use crate::*;
use user_lib::{close, dup2, pipe, read, write as fd_write};
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
const STDOUT: usize = 1;
const STDERR: usize = 2;

#[derive(Default)]
struct LtpSummary {
    passed: usize,
    failed: usize,
    broken: usize,
    skipped: usize,
    warnings: usize,
}

#[derive(Clone, Copy, Default)]
struct LtpOutputCounts {
    passed: usize,
    failed: usize,
    broken: usize,
    skipped: usize,
    warnings: usize,
}

impl LtpOutputCounts {
    fn total(&self) -> usize {
        self.passed + self.failed + self.broken + self.skipped + self.warnings
    }
}

struct LtpRunResult {
    wait_status: i32,
    counts: LtpOutputCounts,
}

impl LtpSummary {
    fn record_skipped(&mut self) {
        self.skipped += 1;
    }

    fn record_run_result(&mut self, result: &LtpRunResult) {
        if result.counts.total() > 0 {
            self.passed += result.counts.passed;
            self.failed += result.counts.failed;
            self.broken += result.counts.broken;
            self.skipped += result.counts.skipped;
            self.warnings += result.counts.warnings;
        } else {
            self.record_wait_status(result.wait_status);
        }
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
        println!("\nSummary:");
        println!("passed   {}", self.passed);
        println!("failed   {}", self.failed);
        println!("broken   {}", self.broken);
        println!("skipped  {}", self.skipped);
        println!("warnings {}", self.warnings);
    }
}

struct LtpOutputScanner {
    tail: [u8; 4],
    tail_len: usize,
    counts: LtpOutputCounts,
}

impl LtpOutputScanner {
    fn new() -> Self {
        Self {
            tail: [0; 4],
            tail_len: 0,
            counts: LtpOutputCounts::default(),
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        let mut data = [0u8; 260];
        let mut len = self.tail_len;
        let mut i = 0;
        while i < self.tail_len {
            data[i] = self.tail[i];
            i += 1;
        }

        i = 0;
        while i < chunk.len() {
            data[len + i] = chunk[i];
            i += 1;
        }
        len += chunk.len();

        let mut pos = self.tail_len.saturating_sub(4);
        while pos + 5 <= len {
            match &data[pos..pos + 5] {
                b"TPASS" => self.counts.passed += 1,
                b"TFAIL" => self.counts.failed += 1,
                b"TBROK" => self.counts.broken += 1,
                b"TCONF" => self.counts.skipped += 1,
                b"TWARN" => self.counts.warnings += 1,
                _ => {}
            }
            pos += 1;
        }

        self.tail_len = if len < self.tail.len() {
            len
        } else {
            self.tail.len()
        };
        i = 0;
        while i < self.tail_len {
            self.tail[i] = data[len - self.tail_len + i];
            i += 1;
        }
    }
}

fn fork_run_ltp_and_collect(dir: &str, args: &[&str]) -> LtpRunResult {
    println!("{:?}", args);
    let mut fds = [0u32; 2];
    if pipe(&mut fds, 0) < 0 {
        let wait_status = fork_and_run(dir, args);
        return LtpRunResult {
            wait_status,
            counts: LtpOutputCounts::default(),
        };
    }

    let read_fd = fds[0] as usize;
    let write_fd = fds[1] as usize;
    let pid = fork();
    if pid == 0 {
        close(read_fd);
        dup2(write_fd, STDOUT, 0);
        dup2(write_fd, STDERR, 0);
        close(write_fd);
        chdir(dir);
        let _ret = execve(args);
        println!("execve fail!");
        exit(1);
    }

    close(write_fd);
    let mut scanner = LtpOutputScanner::new();
    let mut buf = [0u8; 256];
    loop {
        let buf_len = buf.len();
        let n = read(read_fd, &mut buf, buf_len);
        if n <= 0 {
            break;
        }
        let n = n as usize;
        fd_write(STDOUT, &buf[..n], n);
        scanner.push(&buf[..n]);
    }
    close(read_fd);

    let mut wait_status: i32 = 0;
    let _ = waitpid(pid as usize, &mut wait_status);
    LtpRunResult {
        wait_status,
        counts: scanner.counts,
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

        let result = fork_run_ltp_and_collect("/musl/ltp/testcases/bin\0", &[test]);
        summary.record_run_result(&result);
        println!(
            "FAIL LTP CASE {} : {}",
            trim_trailing_nul(test),
            result.wait_status
        );
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

        println!("#### OS COMP TEST GROUP START ltp-musl ####");

        let mut j = group_start;
        while j < group_end {
            let mut summary = LtpSummary::default();
            let test = tests[j];
            if blacklist.contains(&test) {
                println!("SKIP LTP CASE {}", trim_trailing_nul(test));
                summary.record_skipped();
                j += 1;
                continue;
            }
            println!("RUN LTP CASE {}", trim_trailing_nul(test));
            let result = fork_run_ltp_and_collect("/musl/ltp/testcases/bin\0", &[test]);
            summary.record_run_result(&result);
            summary.print();
            println!(
                "FAIL LTP CASE {} : {}",
                trim_trailing_nul(test),
                result.wait_status
            );
            j += 1;
        }
        println!("#### OS COMP TEST GROUP END ltp-musl ####");
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
    println!("#### OS COMP TEST GROUP START ltp-musl ####");
    println!("RUN LTP CASE {}", trim_trailing_nul(test_name));
    let result = fork_run_ltp_and_collect("/musl/ltp/testcases/bin\0", &[test_name]);
    println!(
        "FAIL LTP CASE {} : {}",
        trim_trailing_nul(test_name),
        result.wait_status
    );
    let mut summary = LtpSummary::default();
    summary.record_run_result(&result);
    summary.print();
    println!("#### OS COMP TEST GROUP END ltp-musl ####");
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
        let mut summary = LtpSummary::default();
        if blacklist.contains(&test) {
            println!("SKIP LTP CASE {}", trim_trailing_nul(test));
            summary.record_skipped();
            continue;
        }
        println!("RUN LTP CASE {}", trim_trailing_nul(test));
        let result = fork_run_ltp_and_collect("/glibc/ltp/testcases/bin\0", &[test]);
        summary.record_run_result(&result);
        summary.print();
        println!(
            "FAIL LTP CASE {} : {}",
            trim_trailing_nul(test),
            result.wait_status
        );
    }
    println!("#### OS COMP TEST GROUP END ltp-glibc ####");
}

/// 单独测试 glibc 版本下某个特定的 LTP 测例。
/// **注意：test_name 必须以 `\0` 结尾**（C 字符串约定），否则 execve 会失败。
///
/// 用法：`ltp::test_glibc_single("brk01\0")`
#[allow(unused)]
pub fn test_glibc_single(test_name: &str) {
    println!("RUN GLIBC LTP SINGLE CASE {}", trim_trailing_nul(test_name));
    let result = fork_run_ltp_and_collect("/glibc/ltp/testcases/bin\0", &[test_name]);
    println!(
        "RESULT GLIBC LTP SINGLE CASE {} : {}",
        trim_trailing_nul(test_name),
        result.wait_status
    );
    let mut summary = LtpSummary::default();
    summary.record_run_result(&result);
    summary.print();
}

/// 单独运行 glibc 版本下指定的一组测例（自定义列表）
/// 用法：ltp::test_glibc_custom(&["brk01\0", "brk02\0", "mmap01\0"])
#[allow(unused)]
pub fn test_glibc_custom() {
    println!("===== GLIBC LTP CUSTOM TESTS START =====");
    run_ltp_tests_glibc(
        &ltp::FILELIST[LTP_TEST_START..LTP_TEST_START + LTP_TESTS_PER_GROUP],
        LTP_BLACKLIST,
    );
    println!("===== GLIBC LTP CUSTOM TESTS END =====");
}
