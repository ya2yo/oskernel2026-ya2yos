#![no_std]
#![no_main]

extern crate user_lib;

#[path = "netdev_test/cases.rs"]
mod cases;

#[no_mangle]
fn main() -> i32 {
    cases::run_all()
}
