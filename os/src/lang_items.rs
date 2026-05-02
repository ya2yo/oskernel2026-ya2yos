//! The panic handler
use crate::arch::cpu::shutdown;
use core::panic::PanicInfo;
use log::*;

// #[panic_handler]
// fn panic(info: &PanicInfo) -> ! {
//     if let Some(location) = info.location() {
//         error!(
//             "[kernel] Panicked at {}:{} {}",
//             location.file(),
//             location.line(),
//             info.message().unwrap()
//         );
//     } else {
//         error!("[kernel] Panicked: {}", info.message().unwrap());
//     }
//     shutdown(true)
// }

/// 这是一个更加安全的panic处理器
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!("panic");
    if let Some(location) = info.location() {
        println!(
            "[kernel] Panicked at {}:{} {}",
            location.file(),
            location.line(),
            info.message()
        );
    } else {
        println!("[kernel] Panicked: {}", info.message());
    }
    shutdown(true)
}
