//! Regression for sparse resident pages across an `mprotect` VMA split.

use core::arch::asm;
use user_lib::println;

const PAGE_SIZE: usize = 4096;
const SYS_MUNMAP: usize = 215;
const SYS_MMAP: usize = 222;
const SYS_MPROTECT: usize = 226;

const PROT_READ: usize = 1;
const PROT_WRITE: usize = 2;
const MAP_PRIVATE: usize = 2;
const MAP_ANONYMOUS: usize = 0x20;

#[cfg(target_arch = "riscv64")]
unsafe fn syscall6(number: usize, args: [usize; 6]) -> isize {
    let result: isize;
    asm!(
        "ecall",
        inlateout("a0") args[0] => result,
        in("a1") args[1],
        in("a2") args[2],
        in("a3") args[3],
        in("a4") args[4],
        in("a5") args[5],
        in("a7") number,
    );
    result
}

#[cfg(target_arch = "loongarch64")]
unsafe fn syscall6(number: usize, args: [usize; 6]) -> isize {
    let result: isize;
    asm!(
        "syscall 0",
        inlateout("$a0") args[0] => result,
        in("$a1") args[1],
        in("$a2") args[2],
        in("$a3") args[3],
        in("$a4") args[4],
        in("$a5") args[5],
        in("$a7") number,
    );
    result
}

unsafe fn mmap_anonymous(pages: usize) -> isize {
    syscall6(
        SYS_MMAP,
        [
            0,
            pages * PAGE_SIZE,
            PROT_READ | PROT_WRITE,
            MAP_PRIVATE | MAP_ANONYMOUS,
            usize::MAX,
            0,
        ],
    )
}

unsafe fn munmap(addr: usize, pages: usize) -> isize {
    syscall6(SYS_MUNMAP, [addr, pages * PAGE_SIZE, 0, 0, 0, 0])
}

pub fn run() -> bool {
    let mapping = unsafe { mmap_anonymous(4) };
    if mapping < 0 {
        println!("mprotect split regression: mmap failed {}", mapping);
        return false;
    }
    let base = mapping as usize;
    let left = base as *mut usize;
    let right = (base + 3 * PAGE_SIZE) as *mut usize;

    unsafe {
        // Leave the split boundary at page two non-resident.
        left.write_volatile(0x1122_3344_5566_7788);
        right.write_volatile(0x8877_6655_4433_2211);
    }

    let protected = unsafe {
        syscall6(
            SYS_MPROTECT,
            [base + 2 * PAGE_SIZE, 2 * PAGE_SIZE, PROT_READ, 0, 0, 0],
        )
    };
    if protected != 0 {
        unsafe {
            munmap(base, 4);
        }
        println!("mprotect split regression: mprotect failed {}", protected);
        return false;
    }

    if unsafe { munmap(base + 2 * PAGE_SIZE, 2) } != 0 {
        unsafe {
            munmap(base, 2);
        }
        println!("mprotect split regression: right munmap failed");
        return false;
    }

    // The old split loop attached the left frame to the removed right VMA.
    // The next anonymous fault then reused and overwrote that still-mapped
    // physical page.
    let replacement = unsafe { mmap_anonymous(1) };
    if replacement < 0 {
        unsafe {
            munmap(base, 2);
        }
        println!(
            "mprotect split regression: replacement mmap failed {}",
            replacement
        );
        return false;
    }
    unsafe {
        (replacement as usize as *mut usize).write_volatile(0xa5a5_a5a5_a5a5_a5a5);
    }

    let preserved = unsafe { left.read_volatile() == 0x1122_3344_5566_7788 };
    unsafe {
        munmap(replacement as usize, 1);
        munmap(base, 2);
    }
    println!(
        "mprotect split regression: {}",
        if preserved { "PASS" } else { "FAIL" }
    );
    preserved
}
