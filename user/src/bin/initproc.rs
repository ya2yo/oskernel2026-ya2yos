#![no_std]
#![no_main]
#![allow(unused_imports)]
#![allow(unused_variables)]

extern crate user_lib;

use libctest::runall::{run_specific_test, runall};
use user_lib::{
    AF_INET, SOCK_DGRAM, SOCK_STREAM, chdir, execve, exit, fork, print, println, shutdown, socket, wait, waitpid
};

use crate::libctest::pthread_cancel_points::run_musl_static;

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

// ---------------------------------------------------------------------------
// LTP-musl test helpers
// ---------------------------------------------------------------------------

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
    // basic
    run_testsuit("musl\0", "basic_testcode.sh\0");//龙芯 riscv 不会死循环或panic
    run_testsuit("glibc\0", "basic_testcode.sh\0");// 龙芯 riscv 不会死循环或panic
    // busybox
    run_testsuit("musl\0", "busybox_testcode.sh\0");//龙芯 riscv 不会死循环或panic
    run_testsuit("glibc\0", "busybox_testcode.sh\0");// 龙芯 riscv 不会死循环或panic
    // lua
    run_testsuit("musl\0", "lua_testcode.sh\0");//龙芯 riscv 不会死循环或panic
    run_testsuit("glibc\0", "lua_testcode.sh\0");// 龙芯 riscv 不会死循环或panic
    // libc
    run_testsuit("musl\0", "libctest_testcode.sh\0");//龙芯 riscv 不会死循环或panic
    run_testsuit("glibc\0", "libctest_testcode.sh\0");// riscv loongarch 通过
    // iozone
    run_testsuit("musl\0", "iozone_testcode.sh\0");//龙芯 riscv 不会死循环或panic
    run_testsuit("glibc\0", "iozone_testcode.sh\0");// riscv 通过
    // lmbench
    run_testsuit("musl\0", "lmbench_testcode.sh\0");// 双架构通过
    run_testsuit("glibc\0", "lmbench_testcode.sh\0");// riscv loogarch 通过
    // libcbench
    run_testsuit("musl\0", "libcbench_testcode.sh\0");// 龙芯 riscv 通过
    run_testsuit("glibc\0", "libcbench_testcode.sh\0");// riscv loongarch 通过
    // ltp
    ltp::test_musl_ltp();
    // ltp::test_musl_single("waitpid13\0");
    ltp::test_glibc_ltp();

        // #[cfg(target_arch = "riscv64")]
        // test_cgroup_fj_function_cpuset_via_script();
    // run_testsuit("musl\0", "cyclictest_testcode.sh\0");
    // run_testsuit("musl\0", "iperf_testcode.sh\0");
    // run_testsuit("musl\0", "netperf_testcode.sh\0");// FAIL

    // libctest::pthread_robust_detach::run_glibc_dynamic();

    // run_testsuit("glibc\0", "cyclictest_testcode.sh\0");
    // run_testsuit("glibc\0", "iperf_testcode.sh\0");
    // run_testsuit("glibc\0", "netperf_testcode.sh\0");

}
