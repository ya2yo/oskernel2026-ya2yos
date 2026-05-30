//! Implementation of syscalls
//!
//! The single entry point to all system calls, [`syscall()`], is called
//! whenever userspace wishes to perform a system call using the `ecall`
//! instruction. In this case, the processor raises an 'Environment call from
//! U-mode' exception, which is handled as one of the cases in
//! [`crate::trap::trap_handler`].
//!
//! For clarity, each single syscall is implemented as its own function, named
//! `sys_` then the name of the syscall. You can find functions like this in
//! submodules, and you should also implement syscalls this way.
use core::arch;

use linux_raw_sys::general::statx;
#[cfg(feature = "net")]
use linux_raw_sys::net::{msghdr, socklen_t};
use log::error;
use num_enum::FromPrimitive;
#[derive(Debug, PartialEq, FromPrimitive)]
#[repr(usize)]
pub enum Syscall {
    Getcwd = 17,
    EpollCreate1 = 20,
    EpollCtl = 21,
    EpollPwait = 22,
    Dup = 23,
    Dup3 = 24,
    Fcntl = 25,
    Ioctl = 29,
    Mkdirat = 34,
    Unlinkat = 35,
    Symlinkat = 36,
    Linkat = 37,
    Umount2 = 39,
    Mount = 40,
    StatFs = 43,
    Ftruncate = 46,
    Fallocate = 47,
    Faccessat = 48,
    Chdir = 49,
    Fchmod = 52,
    Fchmodat = 53,
    Fchownat = 54,
    Openat = 56,
    Close = 57,
    Pipe2 = 59,
    Getdents64 = 61,
    Lseek = 62,
    Read = 63,
    Write = 64,
    Readv = 65,
    Writev = 66,
    Pread64 = 67,
    Pwrite64 = 68,
    SendFile = 71,
    Pselect6 = 72,
    Ppoll = 73,
    ReadLinkat = 78,
    Fstatat = 79,
    Fstat = 80,
    Sync = 81,
    Fsync = 82,
    Utimensat = 88,
    Exit = 93,
    ExitGroup = 94,
    SetTidAddress = 96,
    Futex = 98,
    SetRobustList = 99,
    GetRobustList = 100,
    NanoSleep = 101,
    SetTimer = 103,
    ClockGettime = 113,
    ClockGetres = 114,
    ClockNanosleep = 115,
    SysLog = 116,
    SchedSetScheduler = 119,
    SchedGetScheduler = 120,
    SchedGetParam = 121,
    SchedSetaffinity = 122,
    SchedGetaffinity = 123,
    SchedYield = 124,
    SigKill = 129,
    Tkill = 130,
    Tgkill = 131,
    SigSuspend = 133,
    SigAction = 134,
    SigProcMask = 135,
    SigTimedWait = 137,
    SigReturn = 139,
    Setuid = 146,
    Times = 153,
    SetPGid = 154,
    GetPGid = 155,
    SetSid = 157,
    Uname = 160,
    GetRusage = 165,
    Umask = 166,
    GetTimeOfDay = 169,
    GetPid = 172,
    GetPPid = 173,
    GetUid = 174,
    GetEUid = 175,
    GetGid = 176,
    GetEGid = 177,
    GetTid = 178,
    SysInfo = 179,
    Shmget = 194,
    Shmctl = 195,
    Shmat = 196,
    Socket = 198,
    Socketpair = 199,
    Bind = 200,
    Listen = 201,
    Accept = 202,
    Connect = 203,
    GetSockName = 204,
    GetPeerName = 205,
    SendTo = 206,
    RecvFrom = 207,
    SetSockOpt = 208,
    GetSockOpt = 209,
    Shutdown = 210,
    SendMsg = 211,
    RecvMsg = 212,
    Brk = 214,
    Munmap = 215,
    Mremap = 216,
    Clone = 220,
    Mmap = 222,
    Execve = 221,
    Mprotect = 226,
    MSync = 227,
    Madvise = 233,
    GetMempolicy = 236,
    Accept4 = 242,
    Wait4 = 260,
    Prlimit = 261,
    Renameat2 = 276,
    Getrandom = 278,
    MemBarrier = 283,
    CopyFileRange = 285,
    Statx = 291,
    MachineShutdown = 1000,
    #[num_enum(default)]
    Default = 0,
}

mod fs;
mod io_mpx;
mod memory;
mod mm;
#[cfg(feature = "net")]
mod net;
mod options;
mod process;
mod resource;
mod signal;
mod sync;
mod sys;
mod task;
mod time;

use crate::task::{current_task, sys_futex};
use crate::{
    arch::cpu::shutdown,
    fs::{Kstat, Statfs},
    signal::{SigAction, SigInfo, SigSet},
    timer::{Itimerval, Rusage, Timespec, Tms},
    utils::SyscallRet,
};
use fs::*;
use io_mpx::*;
use memory::*;
use mm::*;
#[cfg(feature = "net")]
use net::*;
pub use options::{
    FutexCmd, FutexOpt, MmapFlags, MmapProt, PollEvents, RLimit, SignalMaskFlag, Utsname,
};
use process::*;
use resource::*;
use signal::*;
use sync::*;
use sys::*;
pub use task::*;
use time::*;

/// handle syscall exception with `syscall_id` and other arguments
pub fn syscall(syscall_id: usize, args: [usize; 6]) -> SyscallRet {
    let id = syscall_id;
    let syscall_id: Syscall = Syscall::from(syscall_id);
    // log::debug!(
    //     "[syscall begin] {:?} sepc = {:#x}",
    //     syscall_id,
    //     current_task().unwrap().inner_lock().trap_cx().get_sepc()
    // );
    match syscall_id {
        Syscall::Getcwd => sys_getcwd(args[0] as *const u8, args[1]),
        Syscall::Dup => sys_dup(args[0]),
        Syscall::Dup3 => sys_dup3(args[0], args[1], args[2] as u32),
        Syscall::Fcntl => sys_fcntl(args[0], args[1], args[2]),
        Syscall::Ioctl => sys_ioctl(args[0], args[1], args[2]),
        Syscall::Mkdirat => sys_mkdirat(args[0] as isize, args[1] as *const u8, args[2] as u32),
        Syscall::Unlinkat => sys_unlinkat(args[0] as isize, args[1] as *const u8, args[2] as u32),
        Syscall::Symlinkat => {
            sys_symlinkat(args[0] as *const u8, args[1] as isize, args[2] as *const u8)
        }
        Syscall::Linkat => sys_linkat(
            args[0] as isize,
            args[1] as *const u8,
            args[2] as isize,
            args[3] as *const u8,
            args[4] as u32,
        ),
        Syscall::Umount2 => sys_umount2(args[0] as *const u8, args[1] as u32),
        Syscall::Mount => sys_mount(
            args[0] as *const u8,
            args[1] as *const u8,
            args[2] as *const u8,
            args[3] as u32,
            args[4] as *const u8,
        ),
        Syscall::StatFs => sys_statfs(args[0] as *const u8, args[1] as *mut Statfs),
        Syscall::Ftruncate => sys_ftruncate(args[0], args[1] as i32),
        Syscall::Fallocate => sys_fallocate(
            args[0] as usize,
            args[1] as u32,
            args[2] as usize,
            args[3] as usize,
        ),
        Syscall::Faccessat => sys_faccessat(
            args[0] as i32,
            args[1] as *const u8,
            args[2] as u32,
            args[3],
        ),
        Syscall::Chdir => sys_chdir(args[0] as *const u8),
        Syscall::Fchmod => sys_fchmod(args[0] as usize, args[1] as u32),
        Syscall::Fchmodat => sys_fchmodat(
            args[0] as isize,
            args[1] as *const u8,
            args[2] as u32,
            args[3] as u32,
        ),
        Syscall::Fchownat => sys_fchownat(
            args[0] as isize,
            args[1] as *const u8,
            args[2] as usize,
            args[3] as usize,
            args[4] as u32,
        ),
        Syscall::Openat => sys_openat(
            args[0] as isize,
            args[1] as *const u8,
            args[2] as u32,
            args[3] as u32,
        ),
        Syscall::Close => sys_close(args[0]),
        Syscall::Pipe2 => sys_pipe2(args[0] as *mut u32),
        Syscall::Getdents64 => sys_getdents64(args[0], args[1] as *const u8, args[2]),
        Syscall::Lseek => sys_lseek(args[0], args[1] as isize, args[2]),
        Syscall::Read => sys_read(args[0], args[1] as *const u8, args[2]),
        Syscall::Write => sys_write(args[0], args[1] as *const u8, args[2]),
        Syscall::Readv => sys_readv(args[0], args[1] as *const u8, args[2]),
        Syscall::Writev => sys_writev(args[0], args[1] as *const u8, args[2]),
        Syscall::Pread64 => sys_pread64(args[0], args[1] as *const u8, args[2], args[3] as isize),
        Syscall::Pwrite64 => sys_pwrite64(args[0], args[1] as *const u8, args[2], args[3] as isize),
        Syscall::SendFile => sys_sendfile(args[0], args[1], args[2], args[3]),
        Syscall::Pselect6 => sys_pselect6(args[0], args[1], args[2], args[3], args[4], args[5]),
        Syscall::Ppoll => sys_ppoll(args[0], args[1], args[2], args[3]),
        Syscall::ReadLinkat => sys_readlinkat(
            args[0] as isize,
            args[1] as *const u8,
            args[2] as *const u8,
            args[3],
        ),
        Syscall::Fstatat => sys_fstatat(
            args[0] as isize,
            args[1] as *const u8,
            args[2] as *mut Kstat,
            args[3],
        ),
        Syscall::Fstat => sys_fstat(args[0], args[1] as *mut Kstat),
        Syscall::Statx => sys_statx(
            args[0] as isize,
            args[1] as *const u8,
            args[2],
            args[3] as u32,
            args[4] as *mut statx,
        ),
        Syscall::Sync => sys_sync(),
        Syscall::Fsync => sys_fsync(args[0]),
        Syscall::Utimensat => sys_utimensat(
            args[0] as isize,
            args[1] as *const u8,
            args[2] as *const Timespec,
            args[3],
        ),
        Syscall::Exit => sys_exit(args[0] as i32),
        Syscall::ExitGroup => sys_exit_group(args[0] as i32),
        Syscall::Futex => sys_futex(
            args[0] as *mut i32,
            args[1] as u32,
            args[2] as i32,
            args[3] as *const Timespec,
            args[4] as *mut u32,
            args[5] as i32,
        ),
        Syscall::SetRobustList => sys_set_robust_list(args[0], args[1]),
        Syscall::GetRobustList => {
            sys_get_robust_list(args[0], args[1] as *mut usize, args[2] as *mut usize)
        }
        Syscall::NanoSleep => sys_nanosleep(args[0] as *const Timespec, args[1] as *mut Timespec),
        Syscall::SetTimer => sys_settimer(
            args[0] as usize,
            args[1] as *const Itimerval,
            args[2] as *mut Itimerval,
        ),
        Syscall::ClockGettime => sys_clock_gettime(args[0], args[1] as *mut Timespec),
        Syscall::ClockGetres => sys_clock_getres(args[0] as usize, args[1] as *mut Timespec),
        Syscall::ClockNanosleep => sys_clock_nanosleep(
            args[0],
            args[1] as u32,
            args[2] as *const Timespec,
            args[3] as *mut Timespec,
        ),
        Syscall::SysLog => sys_syslog(args[0] as isize, args[1] as *const u8, args[2]),
        Syscall::SchedSetScheduler => {
            sys_sched_setscheduler(args[0], args[1], args[2] as *const u8)
        }
        Syscall::SchedGetScheduler => sys_sched_getscheduler(args[0]),
        Syscall::SchedGetParam => sys_sched_getparam(args[0], args[1] as *const u8),
        Syscall::SchedSetaffinity => sys_sched_setaffinity(args[0], args[1], args[2]),
        Syscall::SchedGetaffinity => sys_sched_getaffinity(args[0], args[1], args[2]),
        Syscall::SchedYield => sys_sched_yield(),
        Syscall::SigKill => sys_kill(args[0] as isize, args[1]),
        Syscall::Tkill => sys_tkill(args[0], args[1]),
        Syscall::Tgkill => sys_tgkill(args[0], args[1], args[2]),
        Syscall::SigSuspend => sys_rt_sigsuspend(args[0] as *const SigSet),
        Syscall::SigAction => sys_rt_sigaction(
            args[0],
            args[1] as *const SigAction,
            args[2] as *mut SigAction,
        ),
        Syscall::SigProcMask => sys_rt_sigprocmask(
            args[0] as u32,
            args[1] as *const SigSet,
            args[2] as *mut SigSet,
        ),
        Syscall::SigTimedWait => sys_rt_sigtimedwait(
            args[0] as *const SigSet,
            args[1] as *mut SigInfo,
            args[2] as *const Timespec,
        ),
        Syscall::SigReturn => sys_rt_sigreturn(),
        Syscall::Setuid => sys_setuid(args[0] as usize),
        Syscall::Times => sys_times(args[0] as *mut Tms),
        Syscall::SetPGid => sys_setpgid(args[0] as u32, args[1] as u32),
        Syscall::GetPGid => sys_getpgid(),
        Syscall::SetSid => sys_setsid(),
        Syscall::GetRusage => sys_getrusage(args[0] as isize, args[1] as *mut Rusage),
        Syscall::GetTimeOfDay => sys_gettimeofday(args[0] as *mut Timespec, args[1] as usize),
        Syscall::Uname => sys_uname(args[0] as *mut u8),
        Syscall::GetPid => sys_getpid(),
        Syscall::GetPPid => sys_getppid(),
        Syscall::GetUid => sys_getuid(),
        Syscall::GetEUid => sys_geteuid(),
        Syscall::GetGid => sys_getgid(),
        Syscall::GetEGid => sys_getegid(),
        Syscall::GetTid => sys_gettid(),
        Syscall::SysInfo => sys_sysinfo(args[0] as *const u8),
        Syscall::Shmget => sys_shmget(args[0] as i32, args[1], args[2] as i32),
        Syscall::Shmctl => sys_shmctl(args[0] as i32, args[1] as i32, args[2]),
        Syscall::Shmat => sys_shmat(args[0] as i32, args[1], args[2] as i32),
        #[cfg(feature = "net")]
        Syscall::Socket => sys_socket(args[0] as u32, args[1] as u32, args[2] as u32),
        #[cfg(feature = "net")]
        Syscall::Socketpair => sys_socketpair(
            args[0] as u32,
            args[1] as u32,
            args[2] as u32,
            args[3] as *mut u32,
        ),
        #[cfg(feature = "net")]
        Syscall::Bind => sys_bind(args[0], args[1] as *const u8, args[2] as u32),
        #[cfg(feature = "net")]
        Syscall::Listen => sys_listen(args[0], args[1] as u32),
        #[cfg(feature = "net")]
        Syscall::Accept => sys_accept(args[0], args[1] as *mut u8, args[2] as u32),
        #[cfg(feature = "net")]
        Syscall::Connect => sys_connect(args[0], args[1] as *const u8, args[2] as u32),
        #[cfg(feature = "net")]
        Syscall::GetSockName => sys_getsockname(args[0], args[1] as *const u8, args[2] as usize),
        #[cfg(feature = "net")]
        Syscall::GetPeerName => sys_getpeername(args[0], args[1] as *const u8, args[2] as u32),
        #[cfg(feature = "net")]
        Syscall::SendTo => sys_sendto(
            args[0],
            args[1] as *const u8,
            args[2],
            args[3] as u32,
            args[4] as *const u8,
            args[5] as u32,
        ),
        #[cfg(feature = "net")]
        Syscall::RecvFrom => sys_recvfrom(
            args[0],
            args[1] as *mut u8,
            args[2],
            args[3] as u32,
            args[4] as *mut u8,
            args[5] as *mut socklen_t,
        ),
        #[cfg(feature = "net")]
        Syscall::SetSockOpt => sys_setsockopt(
            args[0],
            args[1] as u32,
            args[2] as u32,
            args[3] as *const u8,
            args[4] as u32,
        ),
        #[cfg(feature = "net")]
        Syscall::GetSockOpt => sys_getsockopt(
            args[0],
            args[1] as u32,
            args[2] as u32,
            args[3] as *mut u8,
            args[4] as u32,
        ),
        #[cfg(feature = "net")]
        Syscall::Shutdown => sys_shutdown(args[0], args[1] as u32),
        #[cfg(feature = "net")]
        Syscall::SendMsg => sys_sendmsg(args[0], args[1] as *const msghdr, args[2] as u32),
        #[cfg(feature = "net")]
        Syscall::RecvMsg => sys_recvmsg(args[0], args[1] as *mut msghdr, args[2] as u32),
        #[cfg(feature = "net")]
        Syscall::Accept4 => sys_accept4(
            args[0] as usize,
            args[1] as *mut u8,
            args[2] as u32,
            args[3] as u32,
        ),
        Syscall::Clone => sys_clone(args[0], args[1], args[2], args[3], args[4]),
        Syscall::Brk => sys_brk(args[0]),

        Syscall::Mmap => sys_mmap(
            args[0],
            args[1],
            args[2] as u32,
            args[3] as u32,
            args[4],
            args[5],
        ),
        Syscall::Munmap => sys_munmap(args[0], args[1]),
        Syscall::Mremap => sys_mremap(args[0], args[1], args[2], args[3] as i32, args[4]),
        Syscall::Mprotect => sys_mprotect(args[0], args[1], args[2] as u32),
        Syscall::MSync => Ok(0),
        Syscall::Madvise => sys_madvise(args[0], args[1], args[2]),
        Syscall::Wait4 => sys_waitpid(args[0] as i32, args[1] as *mut i32, args[2] as u32),

        Syscall::Renameat2 => sys_renameat2(
            args[0] as isize,
            args[1] as *const u8,
            args[2] as isize,
            args[3] as *const u8,
            args[4] as u32,
        ),
        Syscall::Getrandom => sys_getrandom(args[0] as *const u8, args[1], args[2] as u32),
        Syscall::MemBarrier => Ok(0),
        Syscall::CopyFileRange => {
            sys_copy_file_range(args[0], args[1], args[2], args[3], args[4], args[5] as u32)
        }
        Syscall::EpollCreate1 => sys_epoll_create1(args[0] as u32),
        Syscall::EpollCtl => sys_epoll_ctl(args[0], args[1], args[2], args[3]),
        Syscall::EpollPwait => sys_epoll_pwait(args[0], args[1], args[2], args[3], args[4]),
        Syscall::MachineShutdown => shutdown(false),

        // task ops
        Syscall::Execve => sys_execve(
            args[0] as *const u8,
            args[1] as *const usize,
            args[2] as *const usize,
        ),
        Syscall::SetTidAddress => sys_settidaddress(args[0]),
        Syscall::Prlimit => sys_prlimit(
            args[0],
            args[1] as u32,
            args[2] as *const RLimit,
            args[3] as *mut RLimit,
        ),
        Syscall::Umask => sys_umask(args[0] as u32),
        Syscall::GetMempolicy => sys_get_mempolicy(args[0], args[1], args[2], args[3], args[4]),
        _ => {
            error!(
                "Unsupported syscall_id: {}, kernel exit this process with exitcode=-1!",
                id
            );
            sys_exit_group(-1)
        }
    }
}
