use user_lib::{
    getpid, kill, println, sigaction, sigaltstack, RawSigAction, StackT, SA_ONSTACK, SA_SIGINFO,
    SS_DISABLE,
};

const SIGUSR1: usize = 10;
const ALT_STACK_SIZE: usize = 8192;
#[cfg(target_arch = "riscv64")]
const MIN_SIGALTSTACK_SIZE: usize = 2048;
#[cfg(target_arch = "loongarch64")]
const MIN_SIGALTSTACK_SIZE: usize = 4096;

#[repr(align(16))]
#[allow(dead_code)]
struct AlternateStack([u8; ALT_STACK_SIZE]);

static mut ALT_STACK: AlternateStack = AlternateStack([0; ALT_STACK_SIZE]);
static mut HANDLER_SP: usize = 0;
static mut HANDLER_UC_STACK: StackT = StackT::empty();

extern "C" fn handler(_signo: usize, _siginfo: usize, ucontext: usize) {
    let marker = 0usize;
    unsafe {
        core::ptr::write(
            core::ptr::addr_of_mut!(HANDLER_SP),
            &marker as *const usize as usize,
        );
        let stack =
            core::ptr::read((ucontext + 2 * core::mem::size_of::<usize>()) as *const StackT);
        core::ptr::write(core::ptr::addr_of_mut!(HANDLER_UC_STACK), stack);
    }
}

fn stack_equals(stack: StackT, sp: usize, flags: u32, size: usize) -> bool {
    stack.sp == sp && stack.flags == flags && stack.size == size
}

pub fn run() -> bool {
    let mut previous_action = RawSigAction::new(0, 0, 0, 0);
    let mut unused_action = RawSigAction::new(0, 0, 0, 0);
    let result = (|| -> Result<(), &'static str> {
        let mut initial = StackT::empty();
        if sigaltstack(None, Some(&mut initial)) != 0 || !stack_equals(initial, 0, SS_DISABLE, 0) {
            return Err("initial query");
        }

        let stack_start = core::ptr::addr_of!(ALT_STACK) as usize;
        let requested = StackT::new(stack_start, 0, ALT_STACK_SIZE);
        if sigaltstack(Some(&requested), None) != 0 {
            return Err("set alternate stack");
        }

        let action =
            RawSigAction::new(handler as *const () as usize, SA_SIGINFO | SA_ONSTACK, 0, 0);
        if sigaction(SIGUSR1, &action, &mut previous_action) != 0 {
            return Err("install signal handler");
        }

        unsafe {
            core::ptr::write(core::ptr::addr_of_mut!(HANDLER_SP), 0);
            core::ptr::write(core::ptr::addr_of_mut!(HANDLER_UC_STACK), StackT::empty());
        }
        if kill(getpid() as usize, SIGUSR1) != 0 {
            return Err("deliver signal");
        }

        let (handler_sp, handler_uc_stack) = unsafe {
            (
                core::ptr::read(core::ptr::addr_of!(HANDLER_SP)),
                core::ptr::read(core::ptr::addr_of!(HANDLER_UC_STACK)),
            )
        };
        if handler_sp <= stack_start || handler_sp > stack_start + ALT_STACK_SIZE {
            return Err("handler did not run on alternate stack");
        }
        if !stack_equals(handler_uc_stack, stack_start, 0, ALT_STACK_SIZE) {
            return Err("ucontext alternate stack");
        }

        let mut current = StackT::empty();
        if sigaltstack(None, Some(&mut current)) != 0
            || !stack_equals(current, stack_start, 0, ALT_STACK_SIZE)
        {
            return Err("post-signal query");
        }

        let disabled = StackT::new(0, SS_DISABLE, 0);
        if sigaltstack(Some(&disabled), None) != 0 {
            return Err("disable alternate stack");
        }
        if sigaltstack(None, Some(&mut current)) != 0 || !stack_equals(current, 0, SS_DISABLE, 0) {
            return Err("disabled query");
        }

        let too_small = StackT::new(stack_start, 0, MIN_SIGALTSTACK_SIZE - 1);
        if sigaltstack(Some(&too_small), None) != -12 {
            return Err("minimum stack size errno");
        }
        let invalid_flags = StackT::new(stack_start, 4, ALT_STACK_SIZE);
        if sigaltstack(Some(&invalid_flags), None) != -22 {
            return Err("invalid flags errno");
        }

        Ok(())
    })();

    let _ = sigaction(SIGUSR1, &previous_action, &mut unused_action);
    match result {
        Ok(()) => {
            println!("sigaltstack regression: PASS");
            true
        }
        Err(step) => {
            println!("sigaltstack regression: FAIL ({})", step);
            false
        }
    }
}
