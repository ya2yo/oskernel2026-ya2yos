use super::common;

const TITLE: &str = "iozone throughput pwrite/pread measurements";
const ARGS: [&str; 11] = [
    "./iozone\0",
    "-t\0",
    "4\0",
    "-i\0",
    "9\0",
    "-i\0",
    "10\0",
    "-r\0",
    "1k\0",
    "-s\0",
    "1m\0",
];

#[allow(dead_code)]
pub fn run_musl() -> i32 {
    common::run_musl(TITLE, &ARGS)
}

#[allow(dead_code)]
pub fn run_glibc() -> i32 {
    common::run_glibc(TITLE, &ARGS)
}
