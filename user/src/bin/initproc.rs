#![no_std]
#![no_main]
#![allow(unused_imports)]
#![allow(unused_variables)]

extern crate user_lib;

use libctest::runall::{run_specific_test, runall};
use user_lib::{
    chdir, execve, exit, fork, println, shutdown, waitpid,
    AF_INET, SOCK_DGRAM, SOCK_STREAM, socket, wait
};

mod basic;
mod libctest;
mod lmbench;
mod ltp;
mod lua;

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

#[allow(dead_code)]
/// fork 并在子进程中运行一个 testsuit
fn run_testsuit(root: &str, script: &str) {
    let args = ["busybox\0", "sh\0", script];
    fork_and_run(root, &args);
}

pub fn fork_and_run(dir: &str, args: &[&str]) -> i32 {
    println!("{:?}", args);
    let pid = fork();
    if pid == 0 {
        chdir(dir);
        let _ret = execve(&args);
        println!("execve fail!");
        exit(0);
    } else {
        let mut exit_code: i32 = 0;
        let _ = waitpid(pid as usize, &mut exit_code);
        return exit_code;
    }
}

fn trim_trailing_nul(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut end = bytes.len();
    if end > 0 && bytes[end - 1] == 0 {
        end -= 1;
    }
    unsafe { core::str::from_utf8_unchecked(&bytes[..end]) }
}

// ---------------------------------------------------------------------------
// LTP test helpers
// ---------------------------------------------------------------------------

const LTP_TEST_START: usize = 9;
const LTP_TESTS_PER_GROUP: usize = 1;

/// LTP 测试黑名单。
/// 前 5 项 (cgroup_fj_*) 仅 `test_ltp` 需要跳过，
/// `check_ltp` 通过 `&LTP_BLACKLIST[LTP_CGROUP_PREFIX_LEN..]` 跳过它们。
const LTP_BLACKLIST: &[&str] = &[
    // [100,200)区间
    // cgroup_fj 系列需要带参数的脚本入口，直接跑 helper 会卡死。
    // 需要验证时使用 test_cgroup_fj_function_cpuset_via_script。
    "cgroup_fj_common.sh\0",
    "cgroup_fj_function.sh\0",
    "cgroup_fj_proc\0",
    "cgroup_fj_stress.sh\0",
    "cgroup_lib.sh\0",
    "cgroup_regression_3_1.sh\0",
    "cgroup_regression_3_2.sh\0",
    "cgroup_regression_5_1.sh\0",
    "cgroup_regression_5_2.sh\0",
    "cgroup_regression_6_1.sh\0",
    "cgroup_regression_6_2.sh\0",
    "cgroup_regression_fork_processes\0",
    "cgroup_regression_getdelays\0",
    "clock_nanosleep01\0",
    "clock_nanosleep04\0",
    "clone02\0",
    "clone03\0",
    "clone08\0",
    "connect01\0",
    "cpuctl_fj_cpu-hog\0",
    // [200,300)区间
    "cpufreq_boost\0",
    "crash02\0",
    "creat06\0",
    "creat07\0",
    "cve-2017-17052\0",
    "dio_append\0",
    "dio_read\0",
    "dio_sparse\0",
    "dio_truncate\0",
    "diotest4\0",
    "diotest6\0",
    "dirty\0",
    "dirtyc0w\0",
    "dirtyc0w_shmem\0",
    "dirtypipe\0",
    "doio\0",
    // [300,400)区间
    "epoll_wait05\0",
    "execve02\0",
    "execve04\0",
    "execve05\0",
    "execveat01\0",
    "execveat02\0",
    "exit_group01\0",
    "fanotify12\0",
    // [400,500)区间
    "fcntl13\0",
    "fcntl13_64\0",
    "fcntl14\0",
    "fcntl14_64\0",
    "fcntl34\0",
    "fcntl34_64\0",
    "fcntl35\0",
    "fcntl36\0",
    "fcntl36_64\0",
    "fcntl37\0",
    "fcntl37_64\0",
    // [500,600)区间
    "flock03\0",
    "force_erase.sh\0",
    "fork04\0",
    "fork07\0",
    "fork14\0",
    "fork_exec_loop\0",
    // [600,700)区间
    "fs_racer_dir_test.sh\0",
    "fs_racer_file_list.sh\0",
    "fstat02\0",
    "fstat02_64\0",
    "fstat03_64\0",
    "fstatat01\0",
    // [700,800)区间
    "futex_cmp_requeue01\0",
    "futex_cmp_requeue02\0",
    "futex_wait02\0",
    "futex_wait04\0",
    "futex_wake03\0",
    "genfrexp\0",
    "genhypot\0",
    "genmodf\0",
    // [800,900)区间
    "getpid02\0",
    "getrusage03\0",
    "getrusage04\0",
    "getsockopt02\0",
    "growfiles\0",
    "hackbench\0",
    // [900,1000)区间
    "in6_02\0",
    "inode01\0",
    "inode02\0",
    // [1000,---)区间
    "ioctl_ns05\0",
    "ioctl_ns06\0",
    "kill02\0",
    "kill05\0",
    "kill06\0",
    "kill08\0",
    "kill09\0",
    "kill10\0",
    "leapsec01\0",
    // [1100,---)区间
    "link02\0",
    "link04\0",
    "link05\0",
    "link08\0",
    "madvise05\0",
    "mallocstress\0",
    // [1200,---)区间
    "memcg_test_2\0",
    "memcg_test_4\0",
    "memcg_test_4.sh\0",
    "mlockall03\0",
    "mmap-corruption01\0",
    "mmap001\0",
    "mmap01\0",
    "mmap12\0",
    "mmap15\0",
    "mmap17\0",
    "mmap18\0",
    "mmap20\0",
    "mmapstress01\0",
    // [1300,---)区间
    "mprotect02\0",
    "mprotect03\0",
    "mprotect04\0",
    "mremap01\0",
    "mremap02\0",
    "mremap03\0",
    "mremap04\0",
    "mremap05\0",
    "mremap06\0",
    "msync02\0",
    "msync03\0",
    "mtest01\0",
    "munlock02\0",
    "munmap02\0",
    "munmap03\0",
    "nanosleep04\0",
    // [1400,---)区间
    "netstress\0",
    "nice05\0",
    "nptl01\0",
    "open11\0",
    "openat01\0",
    "openfile\0",
    "page01\0",
    "pause01\0",
    "pause02\0",
    // [1500,---)区间
    "pidns32\0",
    "pipe11\0",
    "pipe12\0",
    "pipe15\0",
    "pipe2_02\0",
    "poll01\0",
    "ppoll01\0",
    // [1600,---)区间
    "prot_hsymlinks\0",
    "pselect02\0",
    "pselect02_64\0",
    "pthcli\0",
    "pthserv\0",
    "readlinkat02\0",
    "readv02\0",
    "recv01\0",
    "recvfrom01\0",
    "recvmsg01\0",
    "recvmsg03\0",
    // [1700,---)区间
    "rmdir02\0",
    "rt_sigaction02\0",
    "rt_sigprocmask02\0",
    "rt_sigqueueinfo01\0",
    "rt_sigsuspend01\0",
    "run_sched_cliserv.sh\0",
    "sched_driver\0",
    "sched_getaffinity01\0",
    "sched_getattr01\0",
    // [1800,---)区间
    "select03\0",
    "select04\0",
    "semtest_2ns\0",
    "send01\0",
    "sendfile04\0",
    "sendfile04_64\0",
    "sendmsg01\0",
    "sendto01\0",
    "setfsgid03\0",
    "setfsgid03_16\0",
    "setitimer01\0",
    "setitimer02\0",
    // [1900,---)区间
    "setpgid03\0",
    "setpriority01\0",
    "setrlimit05\0",
    "setrlimit06\0",
    "shm_test\0",
    "shmat03\0",
    "shmat04\0",
    "shmctl01\0",
    "shmctl03\0",
    "shmctl04\0",
    "shmctl06\0",
    "shmctl07\0",
    "shmctl08\0",
    // [2000,---)区间
    "shmt04\0",
    "shmt05\0",
    "shmt10\0",
    "sighold02\0",
    "sigrelse01\0",
    "sigsuspend01\0",
    "splice02\0",
    "starvation\0",
    "stat03\0",
    "stat03_64\0",
    // [2100,---)区间
    "symlink03\0",
    "sysctl03\0",
    "sysinfo01\0",
    "sysinfo02\0",
    // [2200,---) 无，网络部分
    // [2300,---) 无，网络部分
    // [2400,---) 无，网络部分
    // [2500,---)区间
    "tgkill01\0",
    "tgkill02\0",
    "tgkill03\0",
    "thp01\0",
    "timed_forkbomb\0",
    "times03\0",
    // [2600,---)区间
    "tst_hexdump\0",
    "tst_supported_fs\0",
    "umask01\0",
    "uname02\0",
    // [2700,---)区间
    "unlink07\0",
    "unlinkat01\0",
    "utsname01\0",
    "utsname02\0",
    "utsname03\0",
    "vma01\0",
    "vmsplice04\0",
    "waitid01\0",
    "waitid04\0",
    "waitid05\0",
    "waitid06\0",
    "waitid07\0",
    "waitid08\0",
    "waitid09\0",
    "waitid11\0",
    "waitpid04\0",
    "waitpid06\0",
    "waitpid07\0",
    "waitpid08\0",
    "waitpid09\0",
    "waitpid10\0",
    "waitpid11\0",
    "waitpid12\0",
    "waitpid13\0",
    // [2800,---)区间
    "writev01\0",
    "writev02\0",
    "writev03\0",
    "writev05\0",
    "writev06\0",
    "writev07\0",
];

/// `test_ltp` 需要额外跳过的 cgroup_fj 前缀条目数，
/// 位于 `LTP_BLACKLIST` 前 5 项。
const LTP_CGROUP_PREFIX_LEN: usize = 5;

#[allow(dead_code)]
fn test_cgroup_fj_function_cpuset_via_script() {
    let args = [
        "/musl/busybox\0",
        "sh\0",
        "-c\0",
        "PATH=/musl/ltp/testcases/bin:/bin:$PATH; export PATH; ./cgroup_fj_function.sh cpuset\0",
    ];
    println!("#### OS COMP TEST GROUP START ltp-musl-cgroup-fj-cpuset ####");
    fork_and_run("/musl/ltp/testcases/bin\0", &args);
    println!("#### OS COMP TEST GROUP END ltp-musl-cgroup-fj-cpuset ####");
}

#[allow(unused)]
fn run_ltp_tests_musl_separately(tests: &[&str], blacklist: &[&str]) {
    let mut group = 0;
    let mut i = 0;
    while i < tests.len() {
        let group_start = i;
        let mut group_end = i + LTP_TESTS_PER_GROUP;
        if group_end > tests.len() {
            group_end = tests.len();
        }

        println!(
            "#### OS COMP TEST GROUP START ltp-musl-{}-{} ####",
            group,
            LTP_TEST_START + group_start
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
            "#### OS COMP TEST GROUP END ltp-musl-{}-{} ####",
            group,
            LTP_TEST_START + group_start
        );
        group += 1;
        i = group_end;
    }
}

#[allow(unused)]
fn test_ltp() {
    let test = &ltp::FILELIST[LTP_TEST_START..LTP_TEST_START+LTP_TESTS_PER_GROUP];
    run_ltp_tests_musl_separately(test, LTP_BLACKLIST);
}

#[allow(unused)]
fn check_ltp() {
    let test = &ltp::FILELIST[..];
    ltp::check_ltp_tests_musl(test, &LTP_BLACKLIST[LTP_CGROUP_PREFIX_LEN..]);
}

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

#[no_mangle]
#[cfg(target_arch = "loongarch64")]
fn main() -> i32 {
    println!("initproc running......");
    // run_testsuit("musl\0", "busybox_testcode.sh\0");
    get_score();
    shutdown();
    0
}

#[no_mangle]
#[cfg(target_arch = "riscv64")]
fn main() -> i32 {
    println!("initproc running......");
    get_score();
    shutdown();
    0
}

// ---------------------------------------------------------------------------
// Unused helpers (kept for ad-hoc testing)
// ---------------------------------------------------------------------------

#[allow(unused)]
fn get_score() {
    // musl
    // run_testsuit("musl\0", "basic_testcode.sh\0");//龙芯 riscv 不会死循环或panic
    // run_testsuit("musl\0", "busybox_testcode.sh\0");//龙芯 riscv 不会死循环或panic
    // run_testsuit("musl\0", "libctest_testcode.sh\0");//龙芯 riscv 不会死循环或panic
    // run_testsuit("musl\0", "lua_testcode.sh\0");//龙芯 riscv 不会死循环或panic
    // run_testsuit("musl\0", "iozone_testcode.sh\0");//龙芯 riscv 不会死循环或panic
    // run_testsuit("musl\0", "cyclictest_testcode.sh\0");
    // run_testsuit("musl\0", "iperf_testcode.sh\0");
    // run_testsuit("musl\0", "libcbench_testcode.sh\0");// 龙芯 riscv 通过
    // run_testsuit("musl\0", "lmbench_testcode.sh\0");// 双架构通过
    // run_testsuit("musl\0", "ltp_testcode.sh\0");
        test_ltp();
        // test_cgroup_fj_function_cpuset_via_script();
    // run_testsuit("musl\0", "netperf_testcode.sh\0");// FAIL

    // glibc
    // run_testsuit("glibc\0", "basic_testcode.sh\0");// 龙芯 riscv 不会死循环或panic
    // run_testsuit("glibc\0", "busybox_testcode.sh\0");// 龙芯 riscv 不会死循环或panic
    // run_testsuit("glibc\0", "lua_testcode.sh\0");// 龙芯 riscv 不会死循环或panic
    // run_testsuit("glibc\0", "cyclictest_testcode.sh\0");
    // run_testsuit("glibc\0", "iozone_testcode.sh\0");// riscv 龙芯 通过
    // run_testsuit("glibc\0", "iperf_testcode.sh\0");
    // run_testsuit("glibc\0", "libcbench_testcode.sh\0");// riscv loonarch 通过
    // run_testsuit("glibc\0", "libctest_testcode.sh\0");// riscv loongarch 通过
    // run_testsuit("glibc\0", "lmbench_testcode.sh\0");// riscv loongarch 通过
    // run_testsuit("glibc\0", "ltp_testcode.sh\0");
    // run_testsuit("glibc\0", "netperf_testcode.sh\0");
}

#[allow(unused)]
fn test_socket() -> i32 {
    println!("---- Test Socket syscall ----");

    let fd_tcp = socket(AF_INET, SOCK_STREAM, 0);
    if fd_tcp >= 0 {
        println!("SUCCESS: TCP socket created, fd: {}.", fd_tcp);
    } else {
        println!("FAILED: TCP socket creation returned error: {}", fd_tcp);
    }

    let fd_udp = socket(AF_INET, SOCK_DGRAM, 0);
    if fd_udp >= 0 {
        println!("SUCCESS: UDP socket created, fd: {}.", fd_udp);
    } else {
        println!("FAILED: UDP socket creation returned error: {}.", fd_udp);
    }

    let fd_err = socket(1, SOCK_STREAM, 0);
    if fd_err < 0 {
        println!("SUCCESS: Correctly rejected unsupported domain, error: {}.", fd_err);
    } else {
        println!("FAILED: Should not have created socket for AF_UNIX, but got fd: {}.", fd_err);
    }

    let fd_invalid = socket(999, SOCK_STREAM, 0);
    if fd_invalid < 0 {
        println!("SUCCESS: Correctly rejected invalid domain, error: {}.", fd_invalid);
    }
    0
}
