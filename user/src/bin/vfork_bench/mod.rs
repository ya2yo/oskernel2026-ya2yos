//! Small Rust translation of the stress-ng vfork/exec hot paths.
//!
//! The vfork child deliberately avoids Rust allocation, formatting, locking,
//! and the normal `execve(&[&str])` wrapper. It only issues raw execve and, on
//! failure, exits. This preserves the CLONE_VM | CLONE_VFORK contract.

use core::ptr;

use user_lib::{execve_raw, exit, fork, get_time, println, vfork, waitpid};

const VFORK_EXIT_OPS: usize = 256;
const EXEC_OPS: usize = 64;

// Project test images contain this static program. Running `true` makes the
// child payload stable while still exercising a full execve loader path. The
// array that points to these strings is initialized before vfork.
const EXEC_PATH: &[u8] = b"/musl/busybox\0";
const EXEC_ARG0: &[u8] = b"true\0";

#[derive(Clone, Copy)]
struct CaseResult {
    attempted: usize,
    failed: usize,
    elapsed_ms: usize,
}

impl CaseResult {
    const fn passed(self) -> bool {
        self.failed == 0 && self.attempted != 0
    }
}

fn wait_for_success(pid: isize) -> bool {
    if pid <= 0 {
        return false;
    }

    let mut status = 0;
    let waited = waitpid(pid as usize, &mut status);
    waited == pid && status == 0
}

fn print_case_start(name: &str) {
    println!("#### OS COMP TEST GROUP START rust-vfork-{} ####", name);
}

fn print_case_end(name: &str, result: CaseResult) {
    println!(
        "#### OS COMP TEST GROUP END rust-vfork-{} attempted={} failed={} elapsed_ms={} ####",
        name, result.attempted, result.failed, result.elapsed_ms
    );
}

fn run_vfork_exit() -> CaseResult {
    print_case_start("exit");
    let started = get_time();
    let mut failed = 0;

    for _ in 0..VFORK_EXIT_OPS {
        let pid = unsafe { vfork() };
        if pid == 0 {
            exit(0);
        }
        if !wait_for_success(pid) {
            failed += 1;
        }
    }

    let result = CaseResult {
        attempted: VFORK_EXIT_OPS,
        failed,
        elapsed_ms: get_time().saturating_sub(started),
    };
    print_case_end("exit", result);
    result
}

fn run_vfork_exec() -> CaseResult {
    print_case_start("exec");
    let argv = [EXEC_PATH.as_ptr(), EXEC_ARG0.as_ptr(), ptr::null()];
    let started = get_time();
    let mut failed = 0;

    for _ in 0..EXEC_OPS {
        let pid = unsafe { vfork() };
        if pid == 0 {
            // SAFETY: all pointers refer to immutable NUL-terminated data;
            // `argv` was initialized before vfork and is only read here.
            unsafe {
                execve_raw(EXEC_PATH.as_ptr(), argv.as_ptr(), ptr::null());
            }
            exit(127);
        }
        if !wait_for_success(pid) {
            failed += 1;
        }
    }

    let result = CaseResult {
        attempted: EXEC_OPS,
        failed,
        elapsed_ms: get_time().saturating_sub(started),
    };
    print_case_end("exec", result);
    result
}

fn run_fork_exec_control() -> CaseResult {
    print_case_start("fork-exec-control");
    let argv = [EXEC_PATH.as_ptr(), EXEC_ARG0.as_ptr(), ptr::null()];
    let started = get_time();
    let mut failed = 0;

    for _ in 0..EXEC_OPS {
        let pid = fork();
        if pid == 0 {
            // SAFETY: the immutable C strings and argv remain valid until the
            // execve syscall replaces this process image.
            unsafe {
                execve_raw(EXEC_PATH.as_ptr(), argv.as_ptr(), ptr::null());
            }
            exit(127);
        }
        if !wait_for_success(pid) {
            failed += 1;
        }
    }

    let result = CaseResult {
        attempted: EXEC_OPS,
        failed,
        elapsed_ms: get_time().saturating_sub(started),
    };
    print_case_end("fork-exec-control", result);
    result
}

/// Run the serial, deterministic vfork/exec regression and performance probe.
pub fn run() -> i32 {
    println!("#### OS COMP TEST GROUP START rust-vfork-profile ####");

    let vfork_exit = run_vfork_exit();
    let vfork_exec = run_vfork_exec();
    let fork_exec = run_fork_exec_control();
    let status = if vfork_exit.passed() && vfork_exec.passed() && fork_exec.passed() {
        0
    } else {
        1
    };

    println!(
        "#### OS COMP TEST GROUP END rust-vfork-profile status={} ####",
        status
    );
    status
}
