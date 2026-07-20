use crate::fork_and_run;
use user_lib::println;

fn run_applet(root: &str, name: &str, args: &[&str]) -> i32 {
    let status = fork_and_run(root, args);
    println!("busybox-regression {} {} exit_code={}", root, name, status);
    status
}

fn run_failed_cases_for_root(root: &str) {
    println!("#### BUSYBOX FAILED CASES START {} ####", root);

    // Keep the directory case independent of files left by a prior QEMU run.
    run_applet(
        root,
        "rm -rf test test_dir",
        &["busybox\0", "rm\0", "-rf\0", "test\0", "test_dir\0"],
    );
    run_applet(root, "hwclock", &["busybox\0", "hwclock\0"]);
    run_applet(
        root,
        "mkdir test_dir",
        &["busybox\0", "mkdir\0", "test_dir\0"],
    );
    run_applet(
        root,
        "mv test_dir test",
        &["busybox\0", "mv\0", "test_dir\0", "test\0"],
    );
    run_applet(root, "rmdir test", &["busybox\0", "rmdir\0", "test\0"]);
    println!("#### BUSYBOX FAILED CASES END {} ####", root);
}

pub fn run_failed_cases() {
    run_failed_cases_for_root("musl\0");
    run_failed_cases_for_root("glibc\0");
}
