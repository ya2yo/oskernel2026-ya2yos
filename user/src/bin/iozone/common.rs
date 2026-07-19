use crate::{cleanup_testsuit_children, fork_and_run};
use user_lib::println;

const MUSL_DIR: &str = "/musl\0";
const GLIBC_DIR: &str = "/glibc\0";

pub(crate) fn run_musl(title: &str, args: &[&str]) -> i32 {
    run_case(MUSL_DIR, title, args)
}

pub(crate) fn run_glibc(title: &str, args: &[&str]) -> i32 {
    run_case(GLIBC_DIR, title, args)
}

fn run_case(dir: &str, title: &str, args: &[&str]) -> i32 {
    // Keep the headings byte-for-byte compatible with iozone_testcode.sh.
    println!("{}", title);
    let status = fork_and_run(dir, args);
    cleanup_testsuit_children();
    status
}
