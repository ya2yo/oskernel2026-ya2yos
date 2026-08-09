//! rt_sigreturn 回归测例：稳定触发 restore_frame 的 frame checkout panic。
//!
//! tmp_01.ans（loongarch64 buildstorm 全量跑）末尾崩溃点：
//!
//! ```text
//! panic
//! [kernel] Panicked at src/signal/frame.rs:373 restore frame checkout error!
//! ```
//!
//! `restore_frame()` 只校验 `trap_cx.sp` 处的 magic word（0xdeadbeef）。
//! 只要用户态在 sp 处没有一份合法的 signal frame——例如不带 frame 直接调用
//! rt_sigreturn、handler 在返回前破坏或移动了 sp 指向的 frame——内核就会
//! assert panic。
//!
//! 每个 case 都在 fork 出来的子进程里触发：当前内核会在第一个 case 直接
//! panic（这就是复现）；修复后子进程应被 waitpid 回收且内核保持存活。

use user_lib::{
    exit, fork, getpid, kill, println, sigaction, sigaltstack, waitpid, RawSigAction, StackT,
    SA_ONSTACK, SA_SIGINFO, SS_DISABLE,
};

const SIGUSR1: usize = 10;
const ALT_STACK_SIZE: usize = 8192;

/// 在 [sp-16, sp) 写入非 0xdeadbeef 的已知值，然后直接执行 rt_sigreturn。
///
/// 修复前内核读到的 checkout != 0xdeadbeef，在 frame.rs:373 assert panic；
/// 修复后 syscall 应返回负值（EINVAL）而不是让内核崩溃。
#[cfg(target_arch = "riscv64")]
#[inline(never)]
unsafe fn bad_rt_sigreturn() -> isize {
    let mut ret: isize;
    core::arch::asm!(
        "addi sp, sp, -16",
        "li t0, 0x11112222",
        "sd t0, 0(sp)",
        "li a7, 139",
        "ecall",
        "addi sp, sp, 16",
        inlateout("x10") 0isize => ret,
        out("x5") _,
        out("x17") _,
    );
    ret
}

#[cfg(target_arch = "loongarch64")]
#[inline(never)]
unsafe fn bad_rt_sigreturn() -> isize {
    let mut ret: isize;
    core::arch::asm!(
        "addi.d $sp, $sp, -16",
        "li.d $t0, 0x11112222",
        "st.d $t0, $sp, 0",
        "li.d $a7, 139",
        "syscall 0",
        "addi.d $sp, $sp, 16",
        inlateout("$a0") 0isize as usize => ret,
        out("$t0") _,
        out("$a7") _,
    );
    ret
}

fn expect(condition: bool, message: &str) -> bool {
    if !condition {
        println!("sigreturn regression failed: {}", message);
    }
    condition
}

fn run_case(name: &str, child: fn() -> i32) -> bool {
    let pid = fork();
    if pid < 0 {
        return expect(false, "fork");
    }
    if pid == 0 {
        println!("sigreturn regression: {} triggering", name);
        let code = child();
        println!("sigreturn regression: {} child returned {}", name, code);
        exit(code);
    }
    let mut status = 0;
    let reaped = waitpid(pid as usize, &mut status) == pid;
    if !reaped {
        return expect(false, "waitpid");
    }
    println!("sigreturn regression: {} reaped status={}", name, status);
    true
}

/// case 1：没有任何 signal frame，直接调用 rt_sigreturn。
fn case_direct() -> i32 {
    let ret = unsafe { bad_rt_sigreturn() };
    if ret >= 0 {
        println!(
            "sigreturn regression: direct rt_sigreturn unexpectedly returned {}",
            ret
        );
        return 1;
    }
    0
}

extern "C" fn plain_handler(_signo: usize) {
    let _ = unsafe { bad_rt_sigreturn() };
}

extern "C" fn siginfo_handler(_signo: usize, _siginfo: usize, _ucontext: usize) {
    let _ = unsafe { bad_rt_sigreturn() };
}

fn install_and_raise(handler: usize, flags: usize) -> i32 {
    let action = RawSigAction::new(handler, flags, 0, 0);
    let mut previous = RawSigAction::new(0, 0, 0, 0);
    if sigaction(SIGUSR1, &action, &mut previous) != 0 {
        return 1;
    }
    if kill(getpid() as usize, SIGUSR1) != 0 {
        return 1;
    }
    0
}

/// case 2：普通 handler 返回前破坏 sp 处的 frame 内容再触发 rt_sigreturn。
fn case_plain_handler() -> i32 {
    install_and_raise(plain_handler as *const () as usize, 0)
}

/// case 3：SA_SIGINFO handler 同 case 2，覆盖 siginfo frame 路径。
fn case_siginfo_handler() -> i32 {
    install_and_raise(siginfo_handler as *const () as usize, SA_SIGINFO)
}

#[repr(align(16))]
struct AlternateStack([u8; ALT_STACK_SIZE]);

static mut ALT_STACK: AlternateStack = AlternateStack([0; ALT_STACK_SIZE]);

/// case 4：SA_ONSTACK handler 在备选信号栈上触发同样的错误。
fn case_alt_stack_handler() -> i32 {
    let stack_start = core::ptr::addr_of!(ALT_STACK) as usize;
    let requested = StackT::new(stack_start, 0, ALT_STACK_SIZE);
    if sigaltstack(Some(&requested), None) != 0 {
        return 1;
    }
    let result = install_and_raise(plain_handler as *const () as usize, SA_ONSTACK);
    let disabled = StackT::new(0, SS_DISABLE, 0);
    let _ = sigaltstack(Some(&disabled), None);
    result
}

pub fn run() -> bool {
    let cases: [(&str, fn() -> i32); 4] = [
        ("direct", case_direct),
        ("plain-handler", case_plain_handler),
        ("siginfo-handler", case_siginfo_handler),
        ("alt-stack-handler", case_alt_stack_handler),
    ];
    let mut ok = true;
    for (name, child) in cases {
        ok = run_case(name, child) && ok;
    }
    println!("sigreturn regression: {}", if ok { "PASS" } else { "FAIL" });
    ok
}
