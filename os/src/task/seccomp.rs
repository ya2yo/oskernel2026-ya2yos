//! Per-thread seccomp state and the small classic-BPF subset used by prctl(2).

use alloc::vec::Vec;

/// Linux limits classic BPF seccomp programs to 4096 instructions.
pub const SECCOMP_FILTER_MAX_INSNS: usize = 4096;

const BPF_LD_W_ABS: u16 = 0x20;
const BPF_JMP_JEQ_K: u16 = 0x15;
const BPF_RET_K: u16 = 0x06;

const SECCOMP_RET_ACTION: u32 = 0x7fff_0000;
const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;

/// User ABI for `struct sock_filter`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SockFilter {
    pub code: u16,
    pub jt: u8,
    pub jf: u8,
    pub k: u32,
}

/// Result of applying the current task's seccomp policy to a syscall.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SeccompAction {
    Allow,
    /// Strict mode rejects a syscall with SIGKILL.
    Kill,
    /// The supported filter rejection path is reported as SIGSYS.
    Trap,
}

/// Seccomp is thread-local. Fork and clone inherit this state from the caller.
#[derive(Clone, Debug)]
pub enum SeccompState {
    Disabled,
    Strict,
    Filter(Vec<SockFilter>),
}

impl Default for SeccompState {
    fn default() -> Self {
        Self::Disabled
    }
}

impl SeccompState {
    pub fn mode(&self) -> usize {
        match self {
            Self::Disabled => 0,
            Self::Strict => 1,
            Self::Filter(_) => 2,
        }
    }

    pub fn is_disabled(&self) -> bool {
        matches!(self, Self::Disabled)
    }

    /// Accept the cBPF instruction subset we can execute safely in the syscall
    /// path: load `seccomp_data.nr`, compare it with a constant, and return a
    /// constant action. The LTP prctl04 program uses exactly this subset.
    pub fn new_filter(program: Vec<SockFilter>) -> Option<Self> {
        validate_filter(&program).then_some(Self::Filter(program))
    }

    pub fn action_for_syscall(&self, syscall_nr: usize) -> SeccompAction {
        match self {
            Self::Disabled => SeccompAction::Allow,
            Self::Strict => {
                // Linux strict mode permits read, write, _exit, and
                // rt_sigreturn. These syscall numbers are shared by the
                // RISC-V and LoongArch64 generic syscall ABIs.
                match syscall_nr {
                    63 | 64 | 93 | 139 => SeccompAction::Allow,
                    _ => SeccompAction::Kill,
                }
            }
            Self::Filter(program) => match run_filter(program, syscall_nr as u32) {
                Some(ret) if ret & SECCOMP_RET_ACTION == SECCOMP_RET_ALLOW => SeccompAction::Allow,
                _ => SeccompAction::Trap,
            },
        }
    }
}

fn validate_filter(program: &[SockFilter]) -> bool {
    if program.is_empty() || program.len() > SECCOMP_FILTER_MAX_INSNS {
        return false;
    }

    for (pc, instruction) in program.iter().enumerate() {
        match instruction.code {
            // `seccomp_data.nr` is the first 32-bit word in seccomp_data.
            BPF_LD_W_ABS if instruction.k == 0 => {}
            BPF_JMP_JEQ_K => {
                let next = pc + 1;
                let true_target = next + instruction.jt as usize;
                let false_target = next + instruction.jf as usize;
                if true_target >= program.len() || false_target >= program.len() {
                    return false;
                }
            }
            BPF_RET_K => {}
            _ => return false,
        }
    }

    program
        .last()
        .is_some_and(|instruction| instruction.code == BPF_RET_K)
}

fn run_filter(program: &[SockFilter], syscall_nr: u32) -> Option<u32> {
    let mut accumulator = 0;
    let mut pc = 0;

    for _ in 0..program.len() {
        let instruction = program.get(pc)?;
        match instruction.code {
            BPF_LD_W_ABS if instruction.k == 0 => {
                accumulator = syscall_nr;
                pc += 1;
            }
            BPF_JMP_JEQ_K => {
                let offset = if accumulator == instruction.k {
                    instruction.jt
                } else {
                    instruction.jf
                };
                pc = pc.checked_add(1 + offset as usize)?;
            }
            BPF_RET_K => return Some(instruction.k),
            _ => return None,
        }
    }

    None
}
