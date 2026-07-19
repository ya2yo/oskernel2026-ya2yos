use super::common;

const TITLE: &str = "iozone automatic measurements";
const ARGS: [&str; 6] = ["./iozone\0", "-a\0", "-r\0", "1k\0", "-s\0", "4m\0"];

#[allow(dead_code)]
pub fn run_musl() -> i32 {
    common::run_musl(TITLE, &ARGS)
}

#[allow(dead_code)]
pub fn run_glibc() -> i32 {
    common::run_glibc(TITLE, &ARGS)
}
