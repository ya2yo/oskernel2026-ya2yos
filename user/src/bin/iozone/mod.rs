//! Iozone cases extracted from the image-provided `iozone_testcode.sh`.
//!
//! Run one case through `iozone::<case>::run_musl()` or
//! `iozone::<case>::run_glibc()`.  The `run_all_*` helpers keep the original
//! script order for full-suite regression runs.

mod common;

pub mod automatic;
pub mod backward_read;
pub mod fwrite_fread;
pub mod pwrite_pread;
pub mod pwritev_preadv;
pub mod random_read;
pub mod stride_read;
pub mod write_read;

use user_lib::println;

type CaseRunner = fn() -> i32;

#[allow(dead_code)]
pub fn run_all_musl() -> i32 {
    run_all(
        "musl",
        &[
            automatic::run_musl,
            write_read::run_musl,
            random_read::run_musl,
            backward_read::run_musl,
            stride_read::run_musl,
            fwrite_fread::run_musl,
            pwrite_pread::run_musl,
            pwritev_preadv::run_musl,
        ],
    )
}

#[allow(dead_code)]
pub fn run_all_glibc() -> i32 {
    run_all(
        "glibc",
        &[
            automatic::run_glibc,
            write_read::run_glibc,
            random_read::run_glibc,
            backward_read::run_glibc,
            stride_read::run_glibc,
            fwrite_fread::run_glibc,
            pwrite_pread::run_glibc,
            pwritev_preadv::run_glibc,
        ],
    )
}

fn run_all(libc: &str, cases: &[CaseRunner]) -> i32 {
    println!("#### OS COMP TEST GROUP START iozone-{} ####", libc);

    let mut result = 0;
    for run_case in cases {
        let status = run_case();
        if result == 0 && status != 0 {
            result = status;
        }
    }

    println!("#### OS COMP TEST GROUP END iozone-{} ####", libc);
    result
}
