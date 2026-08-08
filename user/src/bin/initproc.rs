#![no_std]
#![no_main]
#![allow(unused_imports)]
#![allow(unused_variables)]

extern crate alloc;

extern crate user_lib;

use libctest::runall::{run_specific_test, runall};
use user_lib::{
    chdir, execve, exit, fork, kill_processes, print, println, shutdown, socket, wait, waitpid,
    AF_INET, SOCK_DGRAM, SOCK_STREAM,
};

use crate::libctest::pthread_cancel_points::run_musl_static;

mod basic;
mod buildstorm;
mod busybox;
#[allow(dead_code)]
mod cagent;
#[path = "initproc/fstat_unlink_regression.rs"]
mod fstat_unlink_regression;
mod iozone;
mod libctest;
mod lmbench;
mod ltp;
mod lua;
#[path = "initproc/msg_regression.rs"]
#[allow(dead_code)]
mod msg_regression;
#[path = "netdev_test/cases.rs"]
mod netdev_test_cases;
mod netperf;
#[path = "initproc/rseq_regression.rs"]
mod rseq_regression;
#[path = "initproc/sigaltstack_regression.rs"]
mod sigaltstack_regression;
#[path = "initproc/uptime_regression.rs"]
mod uptime_regression;
mod vfork_bench;

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

#[allow(dead_code)]
/// fork 并在子进程中运行一个 testsuit
fn run_testsuit(root: &str, script: &str) -> i32 {
    let args = ["busybox\0", "sh\0", script];
    let status = fork_and_run(root, &args);
    cleanup_testsuit_children();
    status
}

/// Run a final-round script with Bash and preserve its wait status.
pub(crate) fn run_final_testsuit(root: &str, script: &str) -> i32 {
    let args = ["/bin/bash\0", script];
    let status = fork_and_run(root, &args);
    cleanup_testsuit_children();
    status
}

/// Boot the BuildStorm artifact after the timed build has completed.
///
/// The final RISC-V image ships its nested QEMU under `/opt/qemu-rv64`; this
/// invocation intentionally does not rebuild the artifact or add a timeout.
#[cfg(target_arch = "riscv64")]
fn boot_arceos_helloworld_in_qemu() -> i32 {
    const QEMU_COMMAND: &str = concat!(
        "export LD_LIBRARY_PATH=/opt/qemu-rv64/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}; ",
        "exec /opt/qemu-rv64/bin/qemu-system-riscv64 ",
        "-machine virt -cpu rv64 -m 512M -smp 1 -nographic ",
        "-bios /opt/qemu-rv64/share/opensbi-riscv64-generic-fw_dynamic.bin ",
        "-kernel /work/tgoskits/target/riscv64gc-unknown-linux-musl/release/arceos-helloworld\0"
    );

    println!("----- boot arceos-helloworld in qemu (untimed, arch=riscv64) -----");

    let pid = fork();
    if pid < 0 {
        println!("fork arceos qemu failed: {}", pid);
        return pid as i32;
    }
    if pid == 0 {
        let ret = chdir("/work/tgoskits\0");
        if ret != 0 {
            println!("chdir /work/tgoskits failed: {}", ret);
            exit(127);
        }
        let args = ["/bin/bash\0", "-c\0", QEMU_COMMAND];
        let ret = execve(&args);
        println!("exec arceos qemu failed: {}", ret);
        exit(127);
    }

    let mut exit_code: i32 = 0;
    let waited = waitpid(pid as usize, &mut exit_code);
    if waited != pid as isize {
        println!("waitpid arceos qemu failed: {}", waited);
        return -1;
    }
    if exit_code != 0 {
        println!("arceos qemu exited with status: {}", exit_code);
    }
    exit_code
}

#[cfg(target_arch = "loongarch64")]
fn boot_arceos_helloworld_in_qemu() -> i32 {
    const QEMU_COMMAND: &str = concat!(
        "QEMU_ROOT=/opt/qemu-la64; ",
        "ART=/work/tgoskits/target/loongarch64-unknown-linux-musl/release/arceos-helloworld; ",
        "rm -rf /work/buildstorm.esp; ",
        "mkdir -p /work/buildstorm.esp/EFI/BOOT; ",
        "cp \"${ART}.bin\" /work/buildstorm.esp/EFI/BOOT/BOOTLOONGARCH64.EFI; ",
        "cp \"${QEMU_ROOT}/share/edk2/loongarch64/vars.fd\" /work/buildstorm.vars.fd; ",
        "exec \"${QEMU_ROOT}/lib/ld-linux-loongarch-lp64d.so.1\" ",
        "--library-path \"${QEMU_ROOT}/lib\" ",
        "\"${QEMU_ROOT}/bin/qemu-system-loongarch64\" ",
        "-L \"${QEMU_ROOT}/share/qemu\" ",
        "-machine virt -cpu la464 -smp 1 -m 2G -nographic -serial mon:stdio ",
        "-drive if=pflash,format=raw,unit=0,readonly=on,",
        "file=\"${QEMU_ROOT}/share/edk2/loongarch64/code.fd\" ",
        "-drive if=pflash,format=raw,unit=1,file=/work/buildstorm.vars.fd ",
        "-drive format=raw,file=fat:rw:/work/buildstorm.esp\0"
    );

    println!("----- boot arceos-helloworld in qemu (untimed, arch=loongarch64) -----");

    let pid = fork();
    if pid < 0 {
        println!("fork arceos qemu failed: {}", pid);
        return pid as i32;
    }
    if pid == 0 {
        let ret = chdir("/work/tgoskits\0");
        if ret != 0 {
            println!("chdir /work/tgoskits failed: {}", ret);
            exit(127);
        }
        let args = ["/bin/bash\0", "-c\0", QEMU_COMMAND];
        let ret = execve(&args);
        println!("exec arceos qemu failed: {}", ret);
        exit(127);
    }

    let mut exit_code: i32 = 0;
    let waited = waitpid(pid as usize, &mut exit_code);
    if waited != pid as isize {
        println!("waitpid arceos qemu failed: {}", waited);
        return -1;
    }
    if exit_code != 0 {
        println!("arceos qemu exited with status: {}", exit_code);
    }
    exit_code
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
    if pid < 0 {
        println!("fork fail: {}", pid);
        return pid as i32;
    }
    if pid == 0 {
        chdir(dir);
        let ret = execve(&args);
        println!("execve fail: {}", ret);
        exit(127);
    } else {
        let mut exit_code: i32 = 0;
        let _ = waitpid(pid as usize, &mut exit_code);
        return exit_code;
    }
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
    // test_pre()
    test_final_2026()
}

// Score helpers (kept for ad-hoc testing)
#[allow(unused)]
fn test_pre() -> i32 {
    println!("test_pre start!");
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
    if !fstat_unlink_regression::run() {
        shutdown();
        return 1;
    }
    if !sigaltstack_regression::run() {
        shutdown();
        return 1;
    }
    if !rseq_regression::run() {
        shutdown();
        return 1;
    }
    if !uptime_regression::run() {
        shutdown();
        return 1;
    }
    run_final_testsuit("glibc\0", "cagent_testcode.sh\0");
    run_final_testsuit("glibc\0", "buildstorm_testcode.sh\0");
    #[cfg(any(target_arch = "riscv64", target_arch = "loongarch64"))]
    boot_arceos_helloworld_in_qemu();
    shutdown();
    0
}
