//! 来源于StarryOS(https://github.com/Starry-OS/StarryOS)
//! 和clone实现有关
use bitflags::bitflags;
use log::debug;

use crate::{
    signal::SIG_MAX_NUM,
    task::{current_task, ready_queue},
    utils::{SysErrNo, SyscallRet},
};

const CSIGNAL: u64 = 0xff;

bitflags! {
    /// 手册上clone_args的第一个字段，关于flags
    pub struct CloneFlags: u64 {
        // SIGCHLD 是一个信号，在UNIX和类UNIX操作系统中，当一个子进程改变了它的状态时，内核会向其父进程发送这个信号。这个信号可以用来通知父进程子进程已经终止或者停止了。父进程可以采取适当的行动，比如清理资源或者等待子进程的状态。
        // 以下是SIGCHLD信号的一些常见用途：
        // 子进程终止：当子进程结束运行时，无论是正常退出还是因为接收到信号而终止，操作系统都会向其父进程发送SIGCHLD信号。
        // 资源清理：父进程可以处理SIGCHLD信号来执行清理工作，例如释放子进程可能已经使用的资源。
        // 状态收集：父进程可以通过调用wait()或waitpid()系统调用来获取子进程的终止状态，了解子进程是如何结束的。
        // 孤儿进程处理：在某些情况下，如果父进程没有适当地处理SIGCHLD信号，子进程可能会变成孤儿进程。孤儿进程最终会被init进程（PID为1的进程）收养，并由init进程来处理其终止。
        // 避免僵尸进程：通过正确响应SIGCHLD信号，父进程可以避免产生僵尸进程（zombie process）。僵尸进程是已经终止但父进程尚未收集其终止状态的进程。
        // 默认情况下，SIGCHLD信号的处理方式是忽略，但是开发者可以根据需要设置自定义的信号处理函数来响应这个信号。在多线程程序中，如果需要，也可以将SIGCHLD信号的传递方式设置为线程安全。
        const SIGCHLD = (1 << 4) | (1 << 0);
        // 如果设置此标志，调用进程和子进程将共享同一内存空间
        // 在一个进程中的内存写入在另一个进程可以看到
        const CLONE_VM = 1 << 8;
        // 如果设置此标志，子进程将与父进程共享文件系统信息
        //这包括文件系统的根目录、当前工作目录和umask
        const CLONE_FS = 1 << 9;
        // 如果设置此标志，子进程将与父进程共享文件描述符表。
        const CLONE_FILES = 1 << 10;
        // 如果设置此标志，子进程将与父进程共享信号处理器。
        const CLONE_SIGHAND = 1 << 11;
        // clone_args.pidfd 必须指向父进程地址空间中一个有效的用户空间变量（类型通常是 pid_t，即 int）。
        // 在子进程创建成功后，内核会在该地址写入一个全新的文件描述符，这个文件描述符专门代表刚刚创建的子进程。
        // 从此文件描述符指向的 pidfd 永远指向该子进程，即使子进程 PID 被回收并复用，pidfd 仍然有效且唯一。
        // 不能与 CLONE_PARENT_SETTID 同时使用。
        // 子进程退出时，该 pidfd 变为可读（可被 poll/epoll 检出），可用于异步等待。
        // 使用完毕后必须显式 `close(pidfd)` 释放内核资源。
        const CLONE_PIDFD = 1 << 12;
        // 父进程如果被tracer追踪，子进程也会被同一个tracer追踪
        const CLONE_PTRACE = 1 << 13;
        // 如果设置了 CLONE_VFORK，父进程的执行会被挂起，
        // 直到子进程通过调用 execve(2) 或 _exit(2)释放其虚拟内存资源
        const CLONE_VFORK = 1 << 14;
        // 如果设置，将新创建的子进程的父进程设置为调用者的父进程
        const CLONE_PARENT = 1 << 15;
        // 如果设置该标志,子进程成为与调用者同一线程组内的一个新线程
        const CLONE_THREAD = 1 << 16;
        // 如果设置，创建一个新的挂载命名空间
        const CLONE_NEWNS = 1 << 17;
        // 如果设置了 CLONE_SYSVSEM，则子进程和调用进程共享一个 System V 信号量调整 (semadj) 值列表
        const CLONE_SYSVSEM = 1 << 18;
        // TLS（线程本地存储）描述符设置为 tls（将tp换成user_tp)
        const CLONE_SETTLS = 1 << 19;
        // 将子线程 ID 存储在父线程内存中 parent_tid
        const CLONE_PARENT_SETTID = 1 << 20;
        // 当子进程退出时，清除（归零）子进程内存中 child_tid
        const CLONE_CHILD_CLEARTID = 1 << 21;
        // 此标志仍然有定义，但在调用 clone() 时通常会被忽略。
        const CLONE_DETACHED = 1 << 22;
        // 如果指定了 CLONE_UNTRACED，则跟踪进程无法强制对该子进程执行 CLONE_PTRACE。
        const CLONE_UNTRACED = 1 << 23;
        // 将子线程 ID 存储在子进程内存中 child_tid
        const CLONE_CHILD_SETTID = 1 << 24;
        const CLONE_NEWCGROUP = 1 << 25;
        const CLONE_NEWUTS = 1 << 26;
        const CLONE_NEWIPC = 1 << 27;
        const CLONE_NEWUSER = 1 << 28;
        const CLONE_NEWPID = 1 << 29;
        const CLONE_NEWNET = 1 << 30;
        const CLONE_IO = 1 << 31;
        /// 清除子进程的信号处理表 (Linux 5.5+)
        const CLONE_CLEAR_SIGHAND = 1u64 << 32;
        /// 将子进程放入指定 cgroup (Linux 5.7+)
        const CLONE_INTO_CGROUP = 1u64 << 33;
    }
}
fn parse_clone_flags(raw_flags: usize) -> Result<(CloneFlags, i32), SysErrNo> {
    let exit_signal = (raw_flags as u64) & CSIGNAL;
    if exit_signal as usize > SIG_MAX_NUM {
        return Err(SysErrNo::EINVAL);
    }

    let flags = CloneFlags::from_bits((raw_flags as u64) & !CSIGNAL).ok_or(SysErrNo::EINVAL)?;
    validate_clone_flags(flags, exit_signal)?;

    Ok((
        flags,
        if exit_signal == 0 {
            -1
        } else {
            exit_signal as i32
        },
    ))
}

fn validate_clone_flags(flags: CloneFlags, exit_signal: u64) -> Result<(), SysErrNo> {
    if flags.contains(CloneFlags::CLONE_THREAD) {
        if !flags.contains(CloneFlags::CLONE_SIGHAND) || !flags.contains(CloneFlags::CLONE_VM) {
            return Err(SysErrNo::EINVAL);
        }
        if exit_signal != 0 {
            return Err(SysErrNo::EINVAL);
        }
    }

    if flags.contains(CloneFlags::CLONE_SIGHAND) && !flags.contains(CloneFlags::CLONE_VM) {
        return Err(SysErrNo::EINVAL);
    }
    if flags.contains(CloneFlags::CLONE_CLEAR_SIGHAND) && flags.contains(CloneFlags::CLONE_SIGHAND)
    {
        return Err(SysErrNo::EINVAL);
    }
    if flags.contains(CloneFlags::CLONE_PIDFD) && flags.contains(CloneFlags::CLONE_PARENT_SETTID) {
        return Err(SysErrNo::EINVAL);
    }
    if flags.intersects(
        CloneFlags::CLONE_PIDFD
            | CloneFlags::CLONE_NEWCGROUP
            | CloneFlags::CLONE_NEWIPC
            | CloneFlags::CLONE_NEWNET
            | CloneFlags::CLONE_NEWNS
            | CloneFlags::CLONE_NEWPID
            | CloneFlags::CLONE_NEWUSER
            | CloneFlags::CLONE_NEWUTS
            | CloneFlags::CLONE_INTO_CGROUP,
    ) {
        return Err(SysErrNo::EINVAL);
    }
    if flags.contains(CloneFlags::CLONE_NEWNS) && flags.contains(CloneFlags::CLONE_FS) {
        return Err(SysErrNo::EINVAL);
    }

    Ok(())
}

/// 参考 https://man7.org/linux/man-pages/man2/clone.2.html
/// void (*fn)(void* arg) 参数通过栈传递,如果stack_ptr!=0, fn=0(stack),arg=8(stack)
pub fn sys_clone(
    flags: usize,
    stack_ptr: usize,
    parent_tid_ptr: usize,
    #[cfg(target_arch = "loongarch64")] child_tid_ptr: usize,
    tls_ptr: usize,
    #[cfg(not(target_arch = "loongarch64"))] child_tid_ptr: usize,
) -> SyscallRet {
    let (flags, exit_signal) = parse_clone_flags(flags)?;
    debug!(
        "[sys_clone] flags={:?},stack:{:#x},parent_tid_ptr:{:#x},child_tid_ptr:{:#x},tls_ptr:{:#x}",
        flags, stack_ptr, parent_tid_ptr, child_tid_ptr, tls_ptr
    );
    // if current_task().unwrap().pid() == 4 {
    //     return Ok(current_task().unwrap().tid());
    // }

    let task = current_task().unwrap();
    let new_task = task.clone_process(
        flags,
        exit_signal,
        stack_ptr,
        parent_tid_ptr as *mut u32,
        tls_ptr,
        child_tid_ptr as *mut u32,
    )?;
    let new_tid = new_task.tid();
    // we do not have to move to next instruction since we have done it before
    // add new task to scheduler
    ready_queue::add_task(&new_task);
    Ok(new_tid)
}
