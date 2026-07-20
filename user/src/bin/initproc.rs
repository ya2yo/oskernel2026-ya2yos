#![no_std]
#![no_main]
#![allow(unused_imports)]
#![allow(unused_variables)]

extern crate user_lib;

use libctest::runall::{run_specific_test, runall};
use user_lib::{
    chdir, execve, exit, fork, kill_processes, print, println, shutdown, socket, wait, waitpid,
    AF_INET, SOCK_DGRAM, SOCK_STREAM,
};

use crate::libctest::pthread_cancel_points::run_musl_static;

mod basic;
mod busybox;
mod iozone;
mod libctest;
mod lmbench;
mod ltp;
mod lua;
#[path = "netdev_test/cases.rs"]
mod netdev_test_cases;
mod netperf;

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

// Entry points
#[allow(unused)]
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
fn main() -> i32 {
    // run_interactive_shell()
    // get_score()
    test_final_2026()
}

// Score helpers (kept for ad-hoc testing)
#[allow(unused)]
fn get_score() -> i32 {
    println!("get_score start!");
    netdev_test_cases::run_all();
    // basic
    run_testsuit("musl\0", "basic_testcode.sh\0");
    run_testsuit("glibc\0", "basic_testcode.sh\0");
    // busybox
    run_testsuit("musl\0", "busybox_testcode.sh\0");
    run_testsuit("glibc\0", "busybox_testcode.sh\0");
    // lua
    run_testsuit("musl\0", "lua_testcode.sh\0");
    run_testsuit("glibc\0", "lua_testcode.sh\0");
    // iperf
    run_testsuit("musl\0", "iperf_testcode.sh\0");
    run_testsuit("glibc\0", "iperf_testcode.sh\0");
    // netperf
    run_testsuit("musl\0", "netperf_testcode.sh\0");
    run_testsuit("glibc\0", "netperf_testcode.sh\0");
    // cyclictest
    run_testsuit("musl\0", "cyclictest_testcode.sh\0");
    run_testsuit("glibc\0", "cyclictest_testcode.sh\0");
    // libc
    run_testsuit("musl\0", "libctest_testcode.sh\0");
    // run_testsuit("glibc\0", "libctest_testcode.sh\0");
    // iozone
    run_testsuit("musl\0", "iozone_testcode.sh\0");
    run_testsuit("glibc\0", "iozone_testcode.sh\0");
    // lmbench
    run_testsuit("musl\0", "lmbench_testcode.sh\0");
    run_testsuit("glibc\0", "lmbench_testcode.sh\0");
    // libcbench
    run_testsuit("musl\0", "libcbench_testcode.sh\0");
    run_testsuit("glibc\0", "libcbench_testcode.sh\0");
    // ltp
    ltp::test_musl_ltp();
    ltp::test_glibc_ltp();
    shutdown();
    0
}

// final-2026
#[allow(unused)]
fn test_final_2026() -> i32 {
    // run_testsuit("glibc\0", "cagent_testcode.sh\0");
    run_testsuit("glibc\0", "buildstorm_testcode.sh\0");
    shutdown();
    0
}
