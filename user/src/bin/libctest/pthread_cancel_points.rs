// ./runtest.exe -w entry-static.exe clocale_mbfuncs

use crate::fork_and_run;

#[allow(dead_code)]
pub fn run_glibc_static() {
    let args = [
        "runtest.exe\0",
        "-w\0",
        "entry-static.exe\0",
        "pthread_cancel_points\0",
    ];
    fork_and_run("/glibc\0", &args);
}

#[allow(dead_code)]
pub fn run_musl_static() {
    let args = [
        "runtest.exe\0",
        "-w\0",
        "entry-static.exe\0",
        "pthread_cancel_points\0",
    ];
    fork_and_run("/musl\0", &args);
}

#[allow(dead_code)]
pub fn run_glibc_dynamic() {
    let args = [
        "runtest.exe\0",
        "-w\0",
        "entry-dynamic.exe\0",
        "pthread_cancel_points\0",
    ];
    fork_and_run("/glibc\0", &args);
}

#[allow(dead_code)]
pub fn run_musl_dynamic() {
    let args = [
        "runtest.exe\0",
        "-w\0",
        "entry-dynamic.exe\0",
        "pthread_cancel_points\0",
    ];
    fork_and_run("/musl\0", &args);
}
