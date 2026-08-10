//! Linux signal ABI 基础类型和常量。
//!
//! 本模块只定义用户态可见或跨模块共享的 signal 编号、`SigSet`、
//! `sigaction`、`siginfo_t` 和 alt-stack 结构；实际投递、默认动作处理
//! 和 signal frame 构造分别放在 `delivery`、`pending`、`frame` 模块。

use log::warn;

use crate::{
    arch::context::{MachineContext, UserContext},
    signal::{SIG_DFL, SIG_IGN, SIG_MAX_NUM},
    utils::{SysErrNo, SysResult},
};

/// 仿照Linux signal实现
pub const SIGHUP: usize = 1; /* Hangup.  */
pub const SIGINT: usize = 2; /* Interactive attention signal.  */
pub const SIGQUIT: usize = 3; /* Quit.  */
pub const SIGILL: usize = 4; /* Illegal instruction.  */
pub const SIGTRAP: usize = 5; /* Trace/breakpoint trap.  */
// aka SIGIOT
pub const SIGABRT: usize = 6; /* Abnormal termination.  */
pub const SIGBUS: usize = 7; /* Bus error.  */
pub const SIGFPE: usize = 8; /* Erroneous arithmetic operation.  */
pub const SIGKILL: usize = 9; /* Killed.  */
pub const SIGUSR1: usize = 10;
pub const SIGSEGV: usize = 11; /* Invalid access to storage.  */
pub const SIGUSR2: usize = 12;
pub const SIGPIPE: usize = 13; /* Broken pipe.  */
pub const SIGALRM: usize = 14; /* Alarm clock.  */
pub const SIGTERM: usize = 15; /* Termination request.  */
pub const SIGSTKFLT: usize = 16; /* Stack fault (obsolete).  */
// aka SIGCLD
pub const SIGCHLD: usize = 17; /* Child terminated or stopped.  */
pub const SIGCONT: usize = 18; /* Continue.  */
pub const SIGSTOP: usize = 19; /* Stop, unblockable.  */
pub const SIGTSTP: usize = 20; /* Keyboard stop.  */
pub const SIGTTIN: usize = 21; /* Background read from control terminal.  */
pub const SIGTTOU: usize = 22; /* Background write to control terminal.  */
pub const SIGURG: usize = 23; /* Urgent data is available at a socket.  */
pub const SIGXCPU: usize = 24; /* CPU time limit exceeded.  */
pub const SIGXFSZ: usize = 25; /* File size limit exceeded.  */
pub const SIGVTALRM: usize = 26; /* Virtual timer expired.  */
pub const SIGPROF: usize = 27; /* Profiling timer expired.  */
pub const SIGWINCH: usize = 28; /* Window size change (4.3 BSD, Sun).  */
// aka SIGPOLL
pub const SIGIO: usize = 29; /* Pollable event occurred (System V).  */
pub const SIGPWR: usize = 30; /* Power failure imminent.  */
pub const SIGSYS: usize = 31; /* Bad system call.  */
pub const SIGRTMIN: usize = 32;
// User Custom
pub const SIGRT_1: usize = SIGRTMIN + 1;

bitflags! {
    #[derive(Default)]
    pub struct SigSet: usize {
        const SIGHUP    = 1 << (SIGHUP -1);
        const SIGINT    = 1 << (SIGINT - 1);
        const SIGQUIT   = 1 << (SIGQUIT - 1);
        const SIGILL    = 1 << (SIGILL - 1);
        const SIGTRAP   = 1 << (SIGTRAP - 1);
        const SIGABRT   = 1 << (SIGABRT - 1);
        const SIGBUS    = 1 << (SIGBUS - 1);
        const SIGFPE    = 1 << (SIGFPE - 1);
        const SIGKILL   = 1 << (SIGKILL - 1);
        const SIGUSR1   = 1 << (SIGUSR1 - 1);
        const SIGSEGV   = 1 << (SIGSEGV - 1);
        const SIGUSR2   = 1 << (SIGUSR2 - 1);
        const SIGPIPE   = 1 << (SIGPIPE - 1);
        const SIGALRM   = 1 << (SIGALRM - 1);
        const SIGTERM   = 1 << (SIGTERM - 1);
        const SIGSTKFLT = 1 << (SIGSTKFLT- 1);
        const SIGCHLD   = 1 << (SIGCHLD - 1);
        const SIGCONT   = 1 << (SIGCONT - 1);
        const SIGSTOP   = 1 << (SIGSTOP - 1);
        const SIGTSTP   = 1 << (SIGTSTP - 1);
        const SIGTTIN   = 1 << (SIGTTIN - 1);
        const SIGTTOU   = 1 << (SIGTTOU - 1);
        const SIGURG    = 1 << (SIGURG - 1);
        const SIGXCPU   = 1 << (SIGXCPU - 1);
        const SIGXFSZ   = 1 << (SIGXFSZ - 1);
        const SIGVTALRM = 1 << (SIGVTALRM - 1);
        const SIGPROF   = 1 << (SIGPROF - 1);
        const SIGWINCH  = 1 << (SIGWINCH - 1);
        const SIGIO     = 1 << (SIGIO - 1);
        const SIGPWR    = 1 << (SIGPWR - 1);
        const SIGSYS    = 1 << (SIGSYS - 1);
        const SIGRTMIN  = 1 << (SIGRTMIN- 1);
        const SIGRT_1   = 1 << (SIGRT_1 - 1);
    }
}

/// 内核构造普通 signal handler 栈帧时使用的连续 ABI 镜像。
///
/// 该类型仅供 signal frame 代码使用；用户态布局从低地址到高地址依次为
/// magic、SA_SIGINFO 标记、`stack_t`、signal mask 和 machine context。
#[repr(C)]
pub(crate) struct NormalSignalFrame {
    pub(crate) magic: usize,
    pub(crate) siginfo_flag: usize,
    pub(crate) stack: SignalStack,
    pub(crate) sigmask: SigSet,
    pub(crate) mcontext: MachineContext,
}

/// 内核构造 `SA_SIGINFO` signal handler 栈帧时使用的连续 ABI 镜像。
///
/// 用户态布局从低地址到高地址依次为 magic、SA_SIGINFO 标记、`siginfo_t`
/// 与 `ucontext_t`。该类型不属于用户可直接访问的 Rust API。
#[repr(C)]
pub(crate) struct SigInfoSignalFrame {
    pub(crate) magic: usize,
    pub(crate) siginfo_flag: usize,
    pub(crate) siginfo: SigInfo,
    pub(crate) ucontext: UserContext,
}

impl SigSet {
    pub fn default_op(&self) -> SigOp {
        let terminate_signals = SigSet::SIGHUP
            | SigSet::SIGINT
            | SigSet::SIGKILL
            | SigSet::SIGUSR1
            | SigSet::SIGUSR2
            | SigSet::SIGPIPE
            | SigSet::SIGALRM
            | SigSet::SIGTERM
            | SigSet::SIGSTKFLT
            | SigSet::SIGVTALRM
            | SigSet::SIGPROF
            | SigSet::SIGIO
            | SigSet::SIGPWR;
        let dump_signals = SigSet::SIGQUIT
            | SigSet::SIGILL
            | SigSet::SIGTRAP
            | SigSet::SIGABRT
            | SigSet::SIGBUS
            | SigSet::SIGFPE
            | SigSet::SIGSEGV
            | SigSet::SIGXCPU
            | SigSet::SIGXFSZ
            | SigSet::SIGSYS;
        let ignore_signals = SigSet::SIGCHLD | SigSet::SIGURG | SigSet::SIGWINCH;
        let stop_signals = SigSet::SIGSTOP | SigSet::SIGTSTP | SigSet::SIGTTIN | SigSet::SIGTTOU;
        let continue_signals = SigSet::SIGCONT;
        if terminate_signals.contains(*self) {
            SigOp::Terminate
        } else if dump_signals.contains(*self) {
            SigOp::CoreDump
        } else if ignore_signals.contains(*self) || self.bits == 0 {
            SigOp::Ignore
        } else if stop_signals.contains(*self) {
            SigOp::Stop
        } else if continue_signals.contains(*self) {
            SigOp::Continue
        } else {
            // println!("[kernel] signal {:?}: undefined default operation", self);
            SigOp::Terminate
        }
    }

    pub fn from_sig(signo: usize) -> Self {
        if signo == 0 || signo > SIG_MAX_NUM {
            panic!("invalid signal number: {}", signo);
        }
        SigSet::from_bits_truncate(1 << (signo - 1))
    }
    pub fn peek_front(&self) -> Option<usize> {
        if self.is_empty() {
            None
        } else {
            // SigSet::from_bits(1 << (self.bits().trailing_zeros() as usize))
            Some(self.bits().trailing_zeros() as usize + 1)
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct SigAction {
    pub sa_handler: usize,
    pub sa_flags: SigActionFlags,
    pub sa_restore: usize,
    pub sa_mask: SigSet,
}

impl SigAction {
    /// Linux `SIG_DFL` action exposed by `rt_sigaction()`.
    ///
    /// The default disposition is determined from the signal number when it
    /// is delivered. It must not be encoded as `SIG_IGN` or a kernel function
    /// pointer, since both values are user-visible parts of the sigaction ABI.
    pub const fn default_action() -> Self {
        Self {
            sa_handler: SIG_DFL,
            sa_flags: SigActionFlags::empty(),
            sa_restore: 0,
            sa_mask: SigSet::empty(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SigDisposition {
    Default,
    Ignore,
    Handler,
}

#[derive(Clone, Copy)]
pub struct KSigAction {
    pub act: SigAction,
    disposition: SigDisposition,
}

impl KSigAction {
    pub const fn default_action() -> Self {
        Self {
            act: SigAction::default_action(),
            disposition: SigDisposition::Default,
        }
    }

    pub const fn ignore() -> Self {
        Self {
            act: SigAction {
                sa_handler: SIG_IGN,
                sa_flags: SigActionFlags::empty(),
                sa_restore: 0,
                sa_mask: SigSet::empty(),
            },
            disposition: SigDisposition::Ignore,
        }
    }

    pub const fn handler(act: SigAction) -> Self {
        Self {
            act,
            disposition: SigDisposition::Handler,
        }
    }

    pub const fn is_ignored(self) -> bool {
        matches!(self.disposition, SigDisposition::Ignore)
    }

    pub const fn is_handler(self) -> bool {
        matches!(self.disposition, SigDisposition::Handler)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SigOp {
    Terminate,
    CoreDump,
    Ignore,
    Stop,
    Continue,
}

bitflags! {
    /// Bits in `sa_flags' used to denote the default signal action.
    pub struct SigActionFlags: u32{
    /// Don't send SIGCHLD when children stop.
        const SA_NOCLDSTOP = 1		   ;
    /// Don't create zombie on child death.
        const SA_NOCLDWAIT = 2		   ;
    /// Invoke signal-catching function with three arguments instead of one.
        const SA_SIGINFO   = 4		   ;
    /// Use signal stack by using `sa_restorer'.
        const SA_ONSTACK   = 0x08000000;
    /// Restart syscall on signal return.
        const SA_RESTART   = 0x10000000;
    /// Don't automatically block the signal when its handler is being executed.
        const SA_NODEFER   = 0x40000000;
    /// Reset to SIG_DFL on entry to handler.
        const SA_RESETHAND = 0x80000000;
    /// Historical no-op.
        const SA_INTERRUPT = 0x20000000;
    /// Use signal trampoline provided by C library's wrapper function.
        const SA_RESTORER  = 0x04000000;
    }
}

bitflags! {
    pub struct SignalStackFlags : u32 {
        const ONSTACK = 1;
        const DISABLE = 2;
        const AUTODISARM = 0x80000000;
    }
}

#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct SignalStack {
    pub sp: usize,
    pub flags: u32,
    // Explicitly model the 64-bit Linux stack_t ABI padding so copies to
    // userspace never expose an uninitialized byte range.
    pub _pad: u32,
    pub size: usize,
}

impl SignalStack {
    pub const fn disabled() -> Self {
        Self {
            sp: 0,
            flags: SignalStackFlags::DISABLE.bits,
            _pad: 0,
            size: 0,
        }
    }

    pub const fn new(sp: usize, size: usize) -> Self {
        Self {
            sp,
            flags: 0,
            _pad: 0,
            size,
        }
    }

    /// Linux treats a zero-sized alternate stack as disabled, regardless of
    /// the stale address left in `ss_sp`.
    pub const fn is_disabled(&self) -> bool {
        self.size == 0
    }

    pub const fn is_autodisarm(&self) -> bool {
        self.flags & SignalStackFlags::AUTODISARM.bits != 0
    }

    /// Match Linux's downward-growing-stack interval: `(ss_sp, ss_sp + ss_size]`.
    /// `SS_AUTODISARM` deliberately reports false here, as Linux does.
    pub fn is_on_stack(&self, sp: usize) -> bool {
        !self.is_disabled() && !self.is_autodisarm() && sp > self.sp && sp - self.sp <= self.size
    }

    pub fn should_switch_for_signal(&self, sp: usize) -> bool {
        !self.is_disabled() && !self.is_on_stack(sp)
    }

    pub fn stack_top(&self) -> Option<usize> {
        self.sp.checked_add(self.size)
    }

    /// Build the dynamic `stack_t` view returned by sigaltstack(2).
    pub fn user_view(&self, sp: usize) -> Self {
        let mut flags = self.flags & SignalStackFlags::AUTODISARM.bits;
        if self.is_disabled() {
            flags |= SignalStackFlags::DISABLE.bits;
        } else if self.is_on_stack(sp) {
            flags |= SignalStackFlags::ONSTACK.bits;
        }
        Self {
            sp: self.sp,
            flags,
            _pad: 0,
            size: self.size,
        }
    }

    /// Validate and canonicalize a userspace sigaltstack request.  Callers
    /// hold the current task lock while using this so the stack pointer and
    /// saved configuration are observed consistently.
    pub fn replace_from_user(&self, requested: Self, sp: usize) -> Result<Self, SysErrNo> {
        if self.is_on_stack(sp) {
            return Err(SysErrNo::EPERM);
        }

        let mode = requested.flags & !SignalStackFlags::AUTODISARM.bits;
        if mode != 0
            && mode != SignalStackFlags::DISABLE.bits
            && mode != SignalStackFlags::ONSTACK.bits
        {
            return Err(SysErrNo::EINVAL);
        }

        if mode == SignalStackFlags::DISABLE.bits {
            return Ok(Self {
                sp: 0,
                flags: requested.flags,
                _pad: 0,
                size: 0,
            });
        }

        if requested.size < linux_raw_sys::general::MINSIGSTKSZ as usize {
            return Err(SysErrNo::ENOMEM);
        }

        Ok(Self {
            sp: requested.sp,
            flags: requested.flags,
            _pad: 0,
            size: requested.size,
        })
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SigInfo {
    si_signo: u32,  // 偏移量：0
    si_errno: u32,  // 偏移量：4
    si_code: u32,   // 偏移量：8
    si_12: u32,     // 64-bit Linux aligns the siginfo union to offset 16.
    si_pid: u32,    // 偏移量：16
    si_uid: u32,    // 偏移量：20
    si_status: u32, // 偏移量：24
    // unsupported fields
    __pad: [u8; 128 - 7 * core::mem::size_of::<u32>()],
}

impl SigInfo {
    pub fn new(si_signo: u32, si_errno: u32, si_code: u32, si_16: u32) -> Self {
        Self {
            si_signo: si_signo as u32,
            si_errno: si_errno as u32,
            si_code: si_code as u32,
            si_12: 0,
            si_pid: si_16,
            si_uid: 0,
            si_status: 0,
            __pad: [0; 128 - 7 * core::mem::size_of::<u32>()],
        }
    }

    /// 构造 kill(2)/tkill(2)/tgkill(2) 这类用户态发送信号的 siginfo_t。
    pub fn new_user(si_signo: u32, pid: u32, uid: u32) -> Self {
        Self {
            si_signo,
            si_errno: 0,
            si_code: 0, // SI_USER
            si_12: 0,
            si_pid: pid,
            si_uid: uid,
            si_status: 0,
            __pad: [0; 128 - 7 * core::mem::size_of::<u32>()],
        }
    }

    /// 构造 waitid()/SIGCHLD 使用的 siginfo_t。
    ///
    /// Linux/musl 在 SIGCHLD 场景下会从 siginfo union 的 child 分支读取
    /// si_pid 和 si_status；普通 `new()` 保持兼容旧调用，这个构造函数专门
    /// 用来填 waitid 需要返回给用户态的子进程退出信息。
    pub fn new_child(si_signo: u32, si_code: u32, pid: u32, status: u32) -> Self {
        Self {
            si_signo,
            si_errno: 0,
            si_code,
            si_12: 0,
            si_pid: pid,
            si_uid: 0,
            si_status: status,
            __pad: [0; 128 - 7 * core::mem::size_of::<u32>()],
        }
    }
}
