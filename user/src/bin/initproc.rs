#![no_std]
#![no_main]
#![allow(unused_imports)]
#![allow(unused_variables)]

extern crate alloc;

extern crate user_lib;

use libctest::runall::{run_specific_test, runall};
use user_lib::{
    chdir, close, execve, exit, fork, kill_processes, openat, print, println, shutdown, socket,
    wait, waitpid, OpenFlags, AF_INET, SOCK_DGRAM, SOCK_STREAM,
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
#[path = "initproc/mprotect_split_regression.rs"]
mod mprotect_split_regression;
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
#[path = "initproc/sigreturn_regression.rs"]
#[allow(dead_code)]
mod sigreturn_regression;
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

/// Run the preliminary basic suite from the supplied image.
///
/// The official preliminary images ship `basic/run-all.sh` as mode `0644`,
/// although `basic_testcode.sh` invokes it as `./run-all.sh`. Keep execve's
/// permission checks intact and repair that test-fixture mode before running
/// the original wrapper.
fn run_preliminary_basic_testsuit(root: &str, script: &str) -> i32 {
    let args = [
        "busybox\0",
        "sh\0",
        "-c\0",
        "busybox chmod 0755 basic/run-all.sh && exec busybox sh \"$1\"\0",
        "initproc-basic\0",
        script,
    ];
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
#[allow(dead_code)]
fn boot_arceos_helloworld_in_qemu() -> i32 {
    const QEMU_COMMAND: &str = concat!(
        "export QEMU_LD=/opt/qemu-rv64/lib/ld-linux-riscv64-lp64d.so.1; ",
        "export QEMU_BIN=/opt/qemu-rv64/bin/qemu-system-riscv64; ",
        "export QEMU_BIOS=/opt/qemu-rv64/share/opensbi-riscv64-generic-fw_dynamic.bin; ",
        "export ART=/work/tgoskits/target/riscv64gc-unknown-linux-musl/release/arceos-helloworld; ",
        "echo '----- nested qemu diagnostics -----'; ",
        "printf 'QEMU_LD=%s\\nQEMU_BIN=%s\\nQEMU_BIOS=%s\\nART=%s\\n' ",
        "  \"$QEMU_LD\" \"$QEMU_BIN\" \"$QEMU_BIOS\" \"$ART\"; ",
        "ls -l \"$QEMU_LD\" \"$QEMU_BIN\" \"$QEMU_BIOS\" \"$ART\" 2>&1 || true; ",
        "printf '%s\\n' '----- nested qemu loader probe -----'; ",
        "\"$QEMU_LD\" --version 2>&1 || true; ",
        "\"$QEMU_LD\" --library-path \"$(dirname \"$QEMU_LD\")\" \"$QEMU_BIN\" --version 2>&1 || true; ",
        "printf '%s\\n' '----- nested qemu launch (evaluation command) -----'; ",
        "exec \"$QEMU_LD\" --library-path \"$(dirname \"$QEMU_LD\")\" \"$QEMU_BIN\" ",
        "-machine virt -smp 1 -m 256M -nographic ",
        "-bios \"$QEMU_BIOS\" -kernel \"$ART\"\0"
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
#[allow(dead_code)]
fn boot_arceos_helloworld_in_qemu() -> i32 {
    const QEMU_COMMAND: &str = concat!(
        "export QEMU_ROOT=/opt/qemu-la64; ",
        "export PATH=/root/.cargo/bin:/usr/local/bin:/usr/bin:/bin:/sbin:/usr/sbin; ",
        "export QEMU_LD=\"${QEMU_ROOT}/lib/ld-linux-loongarch-lp64d.so.1\"; ",
        "export QEMU_BIN=\"${QEMU_ROOT}/bin/qemu-system-loongarch64\"; ",
        "export QEMU_CODE=\"${QEMU_ROOT}/share/edk2/loongarch64/code.fd\"; ",
        "export QEMU_VARS=\"${QEMU_ROOT}/share/edk2/loongarch64/vars.fd\"; ",
        "export ART=/work/tgoskits/target/loongarch64-unknown-linux-musl/release/arceos-helloworld; ",
        "echo '----- nested qemu diagnostics -----'; ",
        "printf 'QEMU_LD=%s\\nQEMU_BIN=%s\\nQEMU_CODE=%s\\nQEMU_VARS=%s\\nART=%s.bin\\n' ",
        "  \"$QEMU_LD\" \"$QEMU_BIN\" \"$QEMU_CODE\" \"$QEMU_VARS\" \"$ART\"; ",
        "ls -l \"$QEMU_LD\" \"$QEMU_BIN\" \"$QEMU_CODE\" \"$QEMU_VARS\" \"${ART}.bin\" 2>&1; ",
        "if [ ! -s \"${ART}.bin\" ]; then ",
        "echo 'nested qemu: regenerate missing EFI payload from existing ELF'; ",
        "/usr/bin/llvm-objcopy -O binary \"$ART\" \"${ART}.bin\" || exit 127; ",
        "fi; ",
        "[ -x \"$QEMU_LD\" ] && [ -x \"$QEMU_BIN\" ] && [ -f \"$QEMU_CODE\" ] && ",
        "[ -f \"$QEMU_VARS\" ] && [ -s \"${ART}.bin\" ] || exit 127; ",
        "printf '%s\\n' '----- nested qemu loader probe -----'; ",
        "\"$QEMU_LD\" --version; ",
        "\"$QEMU_LD\" --library-path \"${QEMU_ROOT}/lib\" \"$QEMU_BIN\" --version; ",
        "rm -rf /work/buildstorm.esp; ",
        "mkdir -p /work/buildstorm.esp/EFI/BOOT || exit 127; ",
        "cp \"${ART}.bin\" /work/buildstorm.esp/EFI/BOOT/BOOTLOONGARCH64.EFI || exit 127; ",
        "cp \"$QEMU_VARS\" /work/buildstorm.vars.fd || exit 127; ",
        "printf '%s\\n' '----- nested qemu launch (evaluation command) -----'; ",
        "exec \"$QEMU_LD\" --library-path \"${QEMU_ROOT}/lib\" \"$QEMU_BIN\" ",
        "-L \"${QEMU_ROOT}/share/qemu\" ",
        "-machine virt -cpu la464 -smp 1 -m 2G -nographic -serial mon:stdio ",
        "-drive if=pflash,format=raw,unit=0,readonly=on,file=\"$QEMU_CODE\" ",
        "-drive if=pflash,format=raw,unit=1,file=/work/buildstorm.vars.fd ",
        "-drive if=none,format=raw,id=esp,file=fat:rw:/work/buildstorm.esp ",
        "-device virtio-blk-pci,drive=esp\0"
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

const AT_FDCWD: isize = -100;

#[derive(Clone, Copy)]
enum TestImageKind {
    Preliminary,
    Final,
}

/// Return whether a path from the mounted test image can be opened.
///
/// This probes the guest-visible filesystem rather than the host-side image
/// filename, so it also works when the evaluator supplies a differently named
/// image.  Opening and closing the file keeps the probe independent of a
/// separate `stat` ABI and does not modify the image.
fn image_contains(path: &str) -> bool {
    let fd = openat(AT_FDCWD, path, OpenFlags::O_RDONLY, 0);
    if fd < 0 {
        return false;
    }
    let _ = close(fd as usize);
    true
}

fn detect_test_image() -> Option<TestImageKind> {
    // The final image intentionally keeps the old /musl compatibility tree,
    // so check its Debian/BuildStorm marker before checking preliminary files.
    if image_contains("/work/tgoskits\0") || image_contains("/glibc/cagent_testcode.sh\0") {
        return Some(TestImageKind::Final);
    }

    if image_contains("/musl/basic_testcode.sh\0")
        && image_contains("/glibc/basic_testcode.sh\0")
    {
        return Some(TestImageKind::Preliminary);
    }

    None
}

fn run_selected_tests() -> i32 {
    match detect_test_image() {
        Some(TestImageKind::Preliminary) => {
            println!("detected preliminary test image; running preliminary suites");
            test_pre()
        }
        Some(TestImageKind::Final) => {
            println!("detected final test image; running final suites");
            test_final_2026()
        }
        None => {
            println!(
                "unsupported test image: expected /musl/basic_testcode.sh + \
/glibc/basic_testcode.sh or final-image markers"
            );
            shutdown();
            1
        }
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
    run_selected_tests()
}

// Score helpers (kept for ad-hoc testing)
#[allow(unused)]
fn test_pre() -> i32 {
    println!("test_pre start!");
    // basic
    let status = run_preliminary_basic_testsuit("musl\0", "basic_testcode.sh\0");
    if status != 0 {
        println!("basic-musl testsuite exited with status: {}", status);
        shutdown();
        return status;
    }
    let status = run_preliminary_basic_testsuit("glibc\0", "basic_testcode.sh\0");
    if status != 0 {
        println!("basic-glibc testsuite exited with status: {}", status);
        shutdown();
        return status;
    }
    // // busybox
    // run_testsuit("musl\0", "busybox_testcode.sh\0");
    // run_testsuit("glibc\0", "busybox_testcode.sh\0");
    // // lua
    // run_testsuit("musl\0", "lua_testcode.sh\0");
    // run_testsuit("glibc\0", "lua_testcode.sh\0");
    // // iperf
    // run_testsuit("musl\0", "iperf_testcode.sh\0");
    // run_testsuit("glibc\0", "iperf_testcode.sh\0");
    // // netperf
    // run_testsuit("musl\0", "netperf_testcode.sh\0");
    // run_testsuit("glibc\0", "netperf_testcode.sh\0");
    // // cyclictest
    // run_testsuit("musl\0", "cyclictest_testcode.sh\0");
    // run_testsuit("glibc\0", "cyclictest_testcode.sh\0");
    // // libc
    // run_testsuit("musl\0", "libctest_testcode.sh\0");
    // // run_testsuit("glibc\0", "libctest_testcode.sh\0");
    // // iozone
    // run_testsuit("musl\0", "iozone_testcode.sh\0");
    // run_testsuit("glibc\0", "iozone_testcode.sh\0");
    // // lmbench
    // run_testsuit("musl\0", "lmbench_testcode.sh\0");
    // run_testsuit("glibc\0", "lmbench_testcode.sh\0");
    // // libcbench
    // run_testsuit("musl\0", "libcbench_testcode.sh\0");
    // run_testsuit("glibc\0", "libcbench_testcode.sh\0");
    // // ltp
    // ltp::test_musl_ltp();
    // ltp::test_glibc_ltp();
    shutdown();
    0
}

// final-2026
#[allow(unused)]
fn test_final_2026() -> i32 {
    run_final_testsuit("glibc\0", "cagent_testcode.sh\0");
    run_final_testsuit("glibc\0", "buildstorm_testcode.sh\0");
    boot_arceos_helloworld_in_qemu();
    shutdown();
    0
}

#[allow(unused)]
fn test() -> i32 {
    if netdev_test_cases::run_all()==1 {
        shutdown();
        return 1;
    }
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
    if !mprotect_split_regression::run() {
        shutdown();
        return 1;
    }
    0
}
