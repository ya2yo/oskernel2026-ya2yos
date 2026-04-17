use crate::{chdir, execve, exit, fork, fork_and_run, wait};

#[allow(dead_code)]
pub fn run_glibc_static() {
    let args = ["runtest.exe\0", "-w\0", "entry-static.exe\0", "fscanf\0"];
    fork_and_run("/glibc\0", &args);
}

#[allow(dead_code)]
pub fn run_musl_static() {
    let args = ["runtest.exe\0", "-w\0", "entry-static.exe\0", "fscanf\0"];
    fork_and_run("/musl\0", &args);
}
