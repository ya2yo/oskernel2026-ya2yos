//! Regression for the minimum anonymous 2 MiB MAP_HUGETLB mapping path.

use core::arch::asm;
use user_lib::println;

const PAGE_SIZE: usize = 4096;
const HUGE_PAGE_SIZE: usize = 2 * 1024 * 1024;

const SYS_MUNMAP: usize = 215;
const SYS_MMAP: usize = 222;

const PROT_READ: usize = 1;
const PROT_WRITE: usize = 2;
const MAP_PRIVATE: usize = 2;
const MAP_FIXED: usize = 0x10;
const MAP_ANONYMOUS: usize = 0x20;
const MAP_HUGETLB: usize = 0x40000;

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

unsafe fn mmap(addr: usize, len: usize, flags: usize) -> isize {
    syscall6(
        SYS_MMAP,
        [addr, len, PROT_READ | PROT_WRITE, flags, usize::MAX, 0],
    )
}

unsafe fn munmap(addr: usize, len: usize) -> isize {
    syscall6(SYS_MUNMAP, [addr, len, 0, 0, 0, 0])
}

pub fn run() -> bool {
    let flags = MAP_PRIVATE | MAP_ANONYMOUS | MAP_HUGETLB;

    let invalid_len = unsafe { mmap(0, HUGE_PAGE_SIZE - PAGE_SIZE, flags) };
    if invalid_len >= 0 {
        if invalid_len != 0 {
            unsafe {
                munmap(invalid_len as usize, HUGE_PAGE_SIZE);
            }
        }
        println!("hugepage regression: FAIL (misaligned length accepted)");
        return false;
    }

    let mapping = unsafe { mmap(0, 2 * HUGE_PAGE_SIZE, flags) };
    if mapping <= 0 {
        println!("hugepage regression: FAIL (mmap {})", mapping);
        return false;
    }
    let base = mapping as usize;
    if base % HUGE_PAGE_SIZE != 0 {
        unsafe {
            munmap(base, 2 * HUGE_PAGE_SIZE);
        }
        println!("hugepage regression: FAIL (address alignment)");
        return false;
    }

    let first = base as *mut usize;
    let second = (base + HUGE_PAGE_SIZE) as *mut usize;
    unsafe {
        first.write_volatile(0x1122_3344_5566_7788);
        second.write_volatile(0x8877_6655_4433_2211);
        if first.read_volatile() != 0x1122_3344_5566_7788
            || second.read_volatile() != 0x8877_6655_4433_2211
        {
            munmap(base, 2 * HUGE_PAGE_SIZE);
            println!("hugepage regression: FAIL (initial access)");
            return false;
        }
    }

    let replacement = unsafe { mmap(base, HUGE_PAGE_SIZE, flags | MAP_FIXED) };
    if replacement != mapping {
        unsafe {
            munmap(base, 2 * HUGE_PAGE_SIZE);
        }
        println!("hugepage regression: FAIL (MAP_FIXED {})", replacement);
        return false;
    }

    unsafe {
        first.write_volatile(0xa5a5_a5a5_a5a5_a5a5);
        if first.read_volatile() != 0xa5a5_a5a5_a5a5_a5a5
            || second.read_volatile() != 0x8877_6655_4433_2211
        {
            munmap(base, 2 * HUGE_PAGE_SIZE);
            println!("hugepage regression: FAIL (MAP_FIXED data)");
            return false;
        }
    }

    if unsafe { munmap(base, HUGE_PAGE_SIZE) } != 0
        || unsafe { munmap(base + HUGE_PAGE_SIZE, HUGE_PAGE_SIZE) } != 0
    {
        println!("hugepage regression: FAIL (munmap)");
        return false;
    }

    println!("hugepage regression: PASS");
    true
}
