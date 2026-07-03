#![no_std]
#![no_main]
#![allow(unused_imports)]
#![allow(unused_variables)]

extern crate user_lib;

use libctest::runall::{run_specific_test, runall};
use user_lib::{
    AF_INET, SOCK_DGRAM, SOCK_STREAM, chdir, execve, exit, fork, kill_processes, print, println,
    shutdown, socket, wait, waitpid,
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
    cleanup_testsuit_children();
}

fn cleanup_testsuit_children() {
    const SIGKILL: usize = 9;
    let _ = kill_processes(-1, SIGKILL);
    loop {
        let mut exit_code: i32 = 0;
        if wait(&mut exit_code) <= 0 {
            break;
        }
    }
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

fn run_interactive_shell() -> i32 {
    println!("initproc launching interactive shell......");

    let args = ["/bin/sh\0", "-i\0"];
    let ret = execve(&args);
    println!("exec /bin/sh -i failed: {}", ret);

    let args = ["/musl/busybox\0", "sh\0", "-i\0"];
    let ret = execve(&args);
    println!("exec /musl/busybox sh -i failed: {}", ret);

    shutdown();
    ret as i32
}

#[no_mangle]
#[cfg(target_arch = "loongarch64")]
fn main() -> i32 {
    run_interactive_shell()
}

#[no_mangle]
#[cfg(target_arch = "riscv64")]
fn main() -> i32 {
    // run_interactive_shell()
    get_score()
}

// ---------------------------------------------------------------------------
// Unused helpers (kept for ad-hoc testing)
// ---------------------------------------------------------------------------

#[allow(unused)]
fn get_score() -> i32 {
    println!("get_score start!");
    // basic
    // run_testsuit("musl\0", "basic_testcode.sh\0");//龙芯 riscv 不会死循环或panic
    // run_testsuit("glibc\0", "basic_testcode.sh\0");// 龙芯 riscv 不会死循环或panic
    // // busybox
    // run_testsuit("musl\0", "busybox_testcode.sh\0");//龙芯 riscv 不会死循环或panic
    // run_testsuit("glibc\0", "busybox_testcode.sh\0");// 龙芯 riscv 不会死循环或panic
    // // lua
    // run_testsuit("musl\0", "lua_testcode.sh\0");//龙芯 riscv 不会死循环或panic
    // run_testsuit("glibc\0", "lua_testcode.sh\0");// 龙芯 riscv 不会死循环或panic
    // // iperf
    // run_testsuit("musl\0", "iperf_testcode.sh\0");
    // run_testsuit("glibc\0", "iperf_testcode.sh\0");
    //  // netperf
    // run_testsuit("musl\0", "netperf_testcode.sh\0");
    // run_testsuit("glibc\0", "netperf_testcode.sh\0");
    // // cyclictest
    // run_testsuit("musl\0", "cyclictest_testcode.sh\0");
    // run_testsuit("glibc\0", "cyclictest_testcode.sh\0");
    // // libc
    // run_testsuit("musl\0", "libctest_testcode.sh\0");//龙芯 riscv 不会死循环或panic
    // run_testsuit("glibc\0", "libctest_testcode.sh\0");// riscv loongarch 通过
    // // iozone
    // run_testsuit("musl\0", "iozone_testcode.sh\0");//龙芯 riscv 不会死循环或panic
    // run_testsuit("glibc\0", "iozone_testcode.sh\0");// riscv 通过
    // // lmbench
    // run_testsuit("musl\0", "lmbench_testcode.sh\0");// 双架构通过
    // run_testsuit("glibc\0", "lmbench_testcode.sh\0");// riscv loogarch 通过
    // // libcbench
    // run_testsuit("musl\0", "libcbench_testcode.sh\0");// 龙芯 riscv 通过
    // run_testsuit("glibc\0", "libcbench_testcode.sh\0");// riscv loongarch 通过
    // // ltp
    // ltp::test_musl_ltp();
    ltp::test_musl_single("alarm05\0");
    // ltp::test_glibc_ltp();

    shutdown();
    0
}
