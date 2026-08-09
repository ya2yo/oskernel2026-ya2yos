//!Implementation of [`TaskControlBlock`]
use super::super::process::Process;
use super::super::{
    aux::{Aux, AuxType},
    scheduler::SchedEntity,
    tid_to_task, RseqState, TaskContext, TidHandle,
};
#[cfg(feature = "perf")]
use crate::arch::time::get_ticks;
use crate::{
    arch::{
        config::HART_NUM,
        context::TrapContext,
        memory_layout::{
            PAGE_SIZE, PRE_ALLOC_PAGES, USER_HEAP_SIZE, USER_STACK_SIZE, USER_STACK_TOP,
            USER_TRAP_CONTEXT_TOP,
        },
        page_table::PageTable,
    },
    fs::{
        create_proc_dir, create_proc_dir_and_file, open, FSInfo, FdTable, OSFile, OpenFlags,
        DEFAULT_DIR_MODE, DEFAULT_FILE_MODE,
    },
    mm::{
        copy_to_user, copy_to_user_val, MapAreaType, MapPermission, MemorySet, MemorySetInner,
        PhysPageNum, VirtAddr, VirtPageNum,
    },
    signal::{SigInfo, SigSet, SigTable, SignalStack, SIG_MAX_NUM},
    syscall::MmapFlags,
    task::{
        futex::futex_wake_up, kernel_stack::KernelStackOnHeap, tid, CloneFlags, SeccompAction,
        SeccompState,
    },
    timer::{TimeData, Timer},
    trap::trap_types::{Exception, Trap},
    utils::{get_abs_path, is_abs_path, SysErrNo},
};
use alloc::{
    format,
    string::String,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::fmt::Debug;
use core::mem::size_of;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use futures_util::task::AtomicWaker;
use linux_raw_sys::general::CAP_LAST_CAP;
use log::{debug, error};
use spin::{Mutex, MutexGuard};

use crate::sync::RemoteTlbMutex;

#[repr(C)]
/// 对应 linux 的 robust_list_head
#[derive(Clone, Copy, Debug)]
pub struct RobustListHead {
    pub list: usize,            //robust_list(robust_list *) 用户空间的虚拟地址 0 if empty
    pub futex_offset: isize,    // relative offset
    pub list_op_pending: usize, // robust_list_ptr(** robust_list) first set this field when change
}

impl Default for RobustListHead {
    fn default() -> Self {
        RobustListHead {
            // 暂时设为 0，因为 Default 函数无法知道对象未来的内存地址
            list: 0,
            futex_offset: 0,
            list_op_pending: 0,
        }
    }
}

pub const CAPABILITY_U32S: usize = 2;

const fn capability_full_mask_word(word: usize) -> u32 {
    let first_cap = (word as u32) * 32;
    if CAP_LAST_CAP < first_cap {
        0
    } else {
        let bits = CAP_LAST_CAP - first_cap + 1;
        if bits >= 32 {
            u32::MAX
        } else {
            (1u32 << bits) - 1
        }
    }
}

pub const CAPABILITY_FULL_MASK: [u32; CAPABILITY_U32S] =
    [capability_full_mask_word(0), capability_full_mask_word(1)];

#[derive(Clone, Copy, Debug)]
pub struct CapabilitySets {
    pub effective: [u32; CAPABILITY_U32S],
    pub permitted: [u32; CAPABILITY_U32S],
    pub inheritable: [u32; CAPABILITY_U32S],
}

impl CapabilitySets {
    pub const fn full() -> Self {
        Self {
            effective: CAPABILITY_FULL_MASK,
            permitted: CAPABILITY_FULL_MASK,
            inheritable: [0; CAPABILITY_U32S],
        }
    }
}

pub struct TaskControlBlock {
    // immutable
    tid: TidHandle,
    kernel_stack: KernelStackOnHeap,
    pub process: Arc<Process>,
    /// Linux-visible CPU affinity mask for this thread (not its whole process).
    cpu_affinity: AtomicUsize,
    /// Hart that last selected this thread, also used for timer ownership and
    /// affinity-directed migration notifications.
    scheduled_hart: AtomicUsize,
    /// Linux task_struct::on_cpu equivalent. It remains set until the previous
    /// Hart has completely switched away from this task's saved context.
    on_cpu: AtomicBool,
    /// Counts of lwext4 resource-lock classes currently held by this task.
    ///
    /// This exists only in diagnostic builds. Unlike a per-Hart marker, it
    /// remains valid while a blocked task is resumed on another Hart.
    #[cfg(feature = "perf")]
    ext4_resource_lock_counts: [AtomicUsize; 9],
    // mutable
    // 异步中断/信号同步
    pub interrupted: AtomicBool,
    pub interrupt_waker: AtomicWaker,
    /// 调度器私有运行时间状态；RR 下为空，CFS 下保存 vruntime。
    pub(crate) sched_entity: SchedEntity,
    /// Internal task state. Timer-interrupt paths can contend on this lock
    /// while a page-table update awaits a remote TLB acknowledgement.
    inner: RemoteTlbMutex<TaskControlBlockInner>,
}

impl Drop for TaskControlBlock {
    fn drop(&mut self) {
        debug!("TCB {} dropped", self.tid());
    }
}

pub struct TaskControlBlockInner {
    trap_cx_ppn: PhysPageNum,  // TrapContext缓冲区物理页
    pub trap_cx_bottom: usize, // TrapContext缓冲区虚拟地址基地址

    pub task_cx: TaskContext,
    pub task_status: TaskStatus,
    // pub fd_table: Arc<FdTable>,
    // pub fs_info: Arc<Mutex<FsInfo>>,
    pub time_data: TimeData,
    // 用于TaskControlBlockInner::growproc
    pub user_heappoint: usize,  //堆顶指针,小于等于user_heaptop
    pub user_heapbottom: usize, //堆底指针
    /// 当线程退出时，要将这个指针指向的int置为0，并唤醒等待它的futex
    pub clear_child_tid: usize,
    /// VFORK: if non-zero, parent is suspended waiting for this child PID to
    /// exit or exec. Set by CLONE_VFORK, cleared when child wakes the parent.
    pub vfork_wait_child: usize,
    /// A present user PTE may still transiently fault while a translation
    /// catches up. Keep one retry per VPN; a consecutive second fault remains
    /// a SIGSEGV.
    present_page_fault_retry: Option<VirtPageNum>,
    /// Perf-only vfork lifecycle boundaries. A zero value means this task is
    /// not participating in the corresponding hand-off.
    #[cfg(feature = "perf")]
    pub vfork_published_at: usize,
    #[cfg(feature = "perf")]
    pub vfork_exec_started_at: usize,
    #[cfg(feature = "perf")]
    pub vfork_parent_ready_at: usize,
    /// 被屏蔽的信号
    pub sig_mask: SigSet,
    /// `rt_sigsuspend()` 临时替换信号掩码时保存的调用前掩码。
    /// 有 handler 时写入 signal frame 并由 `rt_sigreturn()` 恢复；无 handler 时由
    /// `trap_return()` 清理，不能在 syscall 返回前恢复。
    pub sigsuspend_restore_mask: Option<SigSet>,
    /// Per-thread alternate signal stack configured by sigaltstack(2).
    pub alt_signal_stack: SignalStack,
    /// 待处理信号集合
    pub sig_pending: SigSet,
    /// 与 sig_pending 位图并行保存的 siginfo_t；标准信号不排队，每个信号保留一份。
    pub sig_pending_info: [Option<SigInfo>; SIG_MAX_NUM + 1],
    /// `execve()` 用于回收 sibling 的内部 SIGKILL。它只允许当前线程退出，
    /// 不能被普通或用户态 SIGKILL 复用为线程组终止。
    pub exec_teardown_kill: bool,
    /// rseq ABI registration is thread-local, just like the user TLS area it
    /// references.
    pub(crate) rseq: RseqState,
    /// Set when an rseq-visible event requires publication before user return.
    /// Ordinary syscall returns leave this clear and avoid user-memory access.
    pub(crate) rseq_pending: bool,
    pub timer: Arc<Timer>,
    pub robust_list: RobustListHead,
    /// POSIX 进程凭证（Credentials）
    /// 参考 Linux task_struct 中 `struct cred` 的 UID/GID 三元组
    ///
    /// 每个 UID/GID 有三个值：
    ///   real     — 实际用户/组 ID（谁启动了这个进程），用于记账和信号权限
    ///   effective — 有效用户/组 ID（用于文件访问权限检查），setuid 程序运行时此值会变
    ///   saved     — 保存的 set-user-ID（允许进程通过 setresuid 来回切换 effective uid）
    ///
    /// 重要区别：
    ///   - `faccessat(2)` 检查 real uid/gid（用户实际是谁）
    ///   - `open(2)` / `creat(2)` / 文件权限检查使用 effective uid/gid（当前特权级别）
    ///   - root（euid=0）绕过所有权限检查
    ///
    /// setresuid(-1, uid, -1)：只改 effective uid，real uid 保持 root → 非 root 权限
    pub user_id: usize, // real uid（实际用户 ID）
    pub effective_uid: u32, // effective uid（有效用户 ID，权限检查用）
    pub saved_uid: u32,     // saved set-user-ID（setuid 保存值）
    pub real_gid: u32,      // real gid（实际组 ID）
    pub effective_gid: u32, // effective gid（有效组 ID，权限检查用）
    pub saved_gid: u32,     // saved set-group-ID（setgid 保存值）
    /// POSIX capabilities。当前只维护 V3 ABI 需要的两个 32-bit 槽。
    pub capabilities: CapabilitySets,
    /// PR_SET_PDEATHSIG 设置的父进程死亡信号 (0 表示未设置)
    pub pdeath_signal: u8,
    /// PR_SET_NO_NEW_PRIVS is monotonic and inherited by children.
    pub no_new_privs: bool,
    /// Seccomp policy is per-thread and is inherited by clone/fork.
    pub seccomp_state: SeccompState,
    /// PR_MCE_KILL policy, inherited by clone/fork.
    pub mce_kill_policy: u32,
    /// Timer slack in nanoseconds, inherited by fork/clone.
    pub timer_slack_ns: usize,

    // 用于futex
    pub futex_pa: usize,      // 当前正在等待的pa
    pub futex_key: usize,     // 当前正在等待的Wait的版本号
    pub futex_timedout: bool, // 本次 futex wait 因超时而唤醒
    /// 信号已交付但被透明处理（setup_frame），可中断 syscall 应返回 EINTR
    pub sig_eintr: bool,
    /// sigtimedwait 因超时而唤醒
    pub sigtimedwait_timedout: bool,
    /// nice 值，范围 -20..19，默认 0（用于 getpriority/setpriority syscall）
    pub nice: i32,
}

impl TaskControlBlockInner {
    pub fn trap_cx(&self) -> &'static mut TrapContext {
        self.trap_cx_ppn.as_mut()
    }

    pub fn is_zombie(&self) -> bool {
        self.task_status == TaskStatus::Zombie
    }

    pub fn retry_present_page_fault(&mut self, vpn: VirtPageNum) -> bool {
        if self.present_page_fault_retry == Some(vpn) {
            false
        } else {
            self.present_page_fault_retry = Some(vpn);
            true
        }
    }

    pub fn clear_present_page_fault_retry(&mut self) {
        self.present_page_fault_retry = None;
    }

    /// Publish that a real context switch requires rseq state to be refreshed
    /// before this task next resumes in user mode.
    pub(crate) fn mark_rseq_pending(&mut self) {
        if self.rseq != RseqState::default() {
            self.rseq_pending = true;
        }
    }
}

fn task_comm_from_argv0(argv0: &[u8]) -> String {
    let mut name = argv0;
    while name.last() == Some(&b'/') {
        name = &name[..name.len() - 1];
    }
    let name = name.rsplit(|byte| *byte == b'/').next().unwrap_or(name);
    let mut comm = String::new();
    if let Ok(name) = core::str::from_utf8(name) {
        for ch in name.chars().take(16) {
            comm.push(ch);
        }
    }
    if comm.is_empty() {
        String::from("?")
    } else {
        comm
    }
}

const EXEC_STACK_LAYOUT_SLACK: usize = 64;

fn checked_exec_stack_add(total: &mut usize, bytes: usize) -> Result<(), SysErrNo> {
    *total = total.checked_add(bytes).ok_or(SysErrNo::E2BIG)?;
    Ok(())
}

/// Reject an exec image whose initial argv/envp stack cannot fit before the
/// address space is replaced.  The slack covers the random bytes and all
/// alignment/padding steps in `TaskControlBlock::exec` below.
fn validate_exec_stack_layout(
    argv: &[Vec<u8>],
    env: &[Vec<u8>],
    elf_auxv_count: usize,
) -> Result<(), SysErrNo> {
    let mut required = 0;
    for value in argv.iter().chain(env.iter()) {
        checked_exec_stack_add(
            &mut required,
            value.len().checked_add(1).ok_or(SysErrNo::E2BIG)?,
        )?;
    }

    // argv/envp each have a trailing NULL, and argc occupies one word.
    let pointer_words = argv
        .len()
        .checked_add(env.len())
        .and_then(|count| count.checked_add(3))
        .ok_or(SysErrNo::E2BIG)?;
    checked_exec_stack_add(
        &mut required,
        pointer_words
            .checked_mul(size_of::<usize>())
            .ok_or(SysErrNo::E2BIG)?,
    )?;

    // exec appends AT_RANDOM, AT_EXECFN, and AT_NULL to the ELF auxiliary vector.
    let aux_entries = elf_auxv_count.checked_add(3).ok_or(SysErrNo::E2BIG)?;
    checked_exec_stack_add(
        &mut required,
        aux_entries
            .checked_mul(size_of::<Aux>())
            .ok_or(SysErrNo::E2BIG)?,
    )?;
    checked_exec_stack_add(&mut required, EXEC_STACK_LAYOUT_SLACK)?;

    if required > USER_STACK_SIZE {
        return Err(SysErrNo::E2BIG);
    }
    Ok(())
}

fn checked_exec_stack_sub(user_sp: &mut usize, bytes: usize) -> Result<usize, SysErrNo> {
    *user_sp = user_sp.checked_sub(bytes).ok_or(SysErrNo::E2BIG)?;
    Ok(*user_sp)
}

fn alloc_user_res_in_memory_set(
    memory_set: &MemorySet,
) -> Result<(usize, usize, PhysPageNum), SysErrNo> {
    memory_set.with_frame_preserving_mut(|ms| {
        let (u_bottom, u_top) = ms.lazy_insert_framed_area_with_hint(
            USER_STACK_TOP,
            USER_STACK_SIZE,
            MapPermission::R | MapPermission::W | MapPermission::U,
            MapAreaType::Stack,
        );
        let (trap_cx_bottom, _) = ms.insert_framed_area_with_hint(
            USER_TRAP_CONTEXT_TOP,
            PAGE_SIZE,
            MapPermission::R | MapPermission::W,
            MapAreaType::Trap,
        );
        let trap_cx_ppn = ms
            .translate(VirtAddr::from(trap_cx_bottom).floor())
            .ok_or(SysErrNo::ENOMEM)?;

        let stack_range = (
            VirtAddr::from(u_bottom).floor(),
            VirtAddr::from(u_top).floor(),
        );
        let area_idx = ms
            .areas
            .iter()
            .position(|area| area.vpn_range.range() == stack_range)
            .ok_or(SysErrNo::ENOMEM)?;
        let stack_end = ms.areas[area_idx].vpn_range.end().0;
        let (page_table, areas) = (&mut ms.page_table, &mut ms.areas);
        let area = &mut areas[area_idx];
        for i in 1..=PRE_ALLOC_PAGES {
            let vpn = (stack_end - i).into();
            if page_table.translate(vpn).is_none() && area.map_one(page_table, vpn).is_none() {
                return Err(SysErrNo::ENOMEM);
            }
        }

        Ok((u_top, trap_cx_bottom, trap_cx_ppn))
    })
}

fn prepare_exec_stack(
    memory_set: &MemorySet,
    ustack_top: usize,
    argv: &[Vec<u8>],
    env: &[Vec<u8>],
    auxv: &mut Vec<Aux>,
) -> Result<(usize, usize, usize), SysErrNo> {
    let mut envp = Vec::new();
    envp.try_reserve(env.len().checked_add(1).ok_or(SysErrNo::E2BIG)?)
        .map_err(|_| SysErrNo::ENOMEM)?;
    let mut argvp = Vec::new();
    argvp
        .try_reserve(argv.len().checked_add(1).ok_or(SysErrNo::E2BIG)?)
        .map_err(|_| SysErrNo::ENOMEM)?;
    auxv.try_reserve(3).map_err(|_| SysErrNo::ENOMEM)?;

    let mut user_sp = ustack_top;
    for value in env {
        let value_len = value.len().checked_add(1).ok_or(SysErrNo::E2BIG)?;
        let value_sp = checked_exec_stack_sub(&mut user_sp, value_len)?;
        envp.push(value_sp);
        copy_to_user(memory_set, value_sp, value)?;
        copy_to_user(
            memory_set,
            value_sp.checked_add(value.len()).ok_or(SysErrNo::E2BIG)?,
            &[0],
        )?;
    }
    envp.push(0);
    user_sp -= user_sp % size_of::<usize>();

    for value in argv {
        let value_len = value.len().checked_add(1).ok_or(SysErrNo::E2BIG)?;
        let value_sp = checked_exec_stack_sub(&mut user_sp, value_len)?;
        argvp.push(value_sp);
        copy_to_user(memory_set, value_sp, value)?;
        copy_to_user(
            memory_set,
            value_sp.checked_add(value.len()).ok_or(SysErrNo::E2BIG)?,
            &[0],
        )?;
    }
    user_sp -= user_sp % size_of::<usize>();
    argvp.push(0);

    let random_sp = checked_exec_stack_sub(&mut user_sp, 16)?;
    let mut random = [0u8; 15];
    for (index, byte) in random.iter_mut().enumerate() {
        *byte = index as u8;
    }
    copy_to_user(memory_set, random_sp, &random)?;
    user_sp -= user_sp % 16;

    let execfn = *argvp.first().ok_or(SysErrNo::E2BIG)?;
    auxv.push(Aux::new(AuxType::RANDOM, random_sp));
    auxv.push(Aux::new(AuxType::EXECFN, execfn));
    auxv.push(Aux::new(AuxType::NULL, 0));

    let initial_stack_words = 1 + argvp.len() + envp.len();
    if initial_stack_words % 2 != 0 {
        checked_exec_stack_sub(&mut user_sp, size_of::<usize>())?;
    }
    for aux in auxv.iter().rev() {
        let aux_sp = checked_exec_stack_sub(&mut user_sp, size_of::<Aux>())?;
        copy_to_user_val(memory_set, aux_sp as *mut usize, &(aux.aux_type as usize))?;
        copy_to_user_val(
            memory_set,
            (aux_sp + size_of::<usize>()) as *mut usize,
            &aux.value,
        )?;
    }

    let envp_bytes = envp
        .len()
        .checked_mul(size_of::<usize>())
        .ok_or(SysErrNo::E2BIG)?;
    let envp_base = checked_exec_stack_sub(&mut user_sp, envp_bytes)?;
    for (index, value) in envp.iter().enumerate() {
        copy_to_user_val(
            memory_set,
            (envp_base + index * size_of::<usize>()) as *mut usize,
            value,
        )?;
    }

    let argvp_bytes = argvp
        .len()
        .checked_mul(size_of::<usize>())
        .ok_or(SysErrNo::E2BIG)?;
    let argv_base = checked_exec_stack_sub(&mut user_sp, argvp_bytes)?;
    for (index, value) in argvp.iter().enumerate() {
        copy_to_user_val(
            memory_set,
            (argv_base + index * size_of::<usize>()) as *mut usize,
            value,
        )?;
    }

    let argc_sp = checked_exec_stack_sub(&mut user_sp, size_of::<usize>())?;
    copy_to_user_val(memory_set, argc_sp as *mut usize, &argv.len())?;
    debug_assert_eq!(argc_sp % 16, 0);
    Ok((argc_sp, argv_base, envp_base))
}

impl TaskControlBlock {
    /// Mask of all harts configured into this kernel image.
    #[inline]
    pub const fn online_cpu_mask() -> usize {
        if HART_NUM >= usize::BITS as usize {
            usize::MAX
        } else {
            (1usize << HART_NUM) - 1
        }
    }

    #[inline]
    fn default_cpu_affinity(home_hart: usize) -> usize {
        #[cfg(any(target_arch = "riscv64", target_arch = "loongarch64"))]
        {
            let _ = home_hart;
            Self::online_cpu_mask()
        }
    }

    /// CPU mask that this architecture can safely use for this task's address
    /// space. Both supported SMP targets have a resumable IPI and remote TLB
    /// shootdown path, so a shared `MemorySet` may execute on any online hart.
    #[inline]
    pub fn allowed_cpu_mask(&self) -> usize {
        let _ = self;
        Self::online_cpu_mask()
    }

    /// Pick the first allowed hart at or after `start`, wrapping at the end.
    #[inline]
    fn choose_hart(mask: usize, start: usize) -> usize {
        debug_assert_ne!(mask & Self::online_cpu_mask(), 0);
        for offset in 0..HART_NUM {
            let hart = (start + offset) % HART_NUM;
            if mask & (1usize << hart) != 0 {
                return hart;
            }
        }
        unreachable!("CPU affinity contains no online hart")
    }

    pub fn inner_lock(&self) -> MutexGuard<'_, TaskControlBlockInner> {
        self.inner.lock()
    }

    pub fn seccomp_action(&self, syscall_nr: usize) -> SeccompAction {
        self.inner_lock()
            .seccomp_state
            .action_for_syscall(syscall_nr)
    }
    pub fn tid(&self) -> usize {
        self.tid.0
    }

    #[cfg(feature = "perf")]
    #[inline]
    pub(crate) fn ext4_resource_lock_acquired(
        &self,
        class: crate::utils::perf::Ext4ResourceLockClass,
    ) {
        self.ext4_resource_lock_counts[class.index()].fetch_add(1, Ordering::Relaxed);
    }

    #[cfg(feature = "perf")]
    #[inline]
    pub(crate) fn ext4_resource_lock_released(
        &self,
        class: crate::utils::perf::Ext4ResourceLockClass,
    ) {
        let previous =
            self.ext4_resource_lock_counts[class.index()].fetch_sub(1, Ordering::Relaxed);
        debug_assert_ne!(previous, 0, "lwext4 resource-lock context underflow");
    }

    #[cfg(feature = "perf")]
    #[inline]
    pub(crate) fn ext4_resource_lock_context(&self) -> usize {
        self.ext4_resource_lock_counts
            .iter()
            .enumerate()
            .fold(0, |context, (index, count)| {
                if count.load(Ordering::Relaxed) != 0 {
                    context | (1usize << index)
                } else {
                    context
                }
            })
    }

    #[cfg(feature = "perf")]
    pub(crate) fn clear_ext4_resource_lock_context(&self) {
        for count in &self.ext4_resource_lock_counts {
            count.store(0, Ordering::Relaxed);
        }
    }

    #[inline]
    pub fn cpu_affinity(&self) -> usize {
        self.cpu_affinity.load(Ordering::Acquire)
    }

    #[inline]
    pub fn scheduled_hart(&self) -> usize {
        self.scheduled_hart.load(Ordering::Acquire)
    }

    /// Return whether this thread may be dispatched on the specified Hart.
    #[inline]
    pub(crate) fn can_run_on(&self, hartid: usize) -> bool {
        hartid < HART_NUM && self.cpu_affinity() & (1usize << hartid) != 0
    }

    /// Publish the Hart that actually selected this task.
    #[inline]
    pub(crate) fn set_scheduled_hart(&self, hartid: usize) {
        debug_assert!(hartid < HART_NUM);
        self.scheduled_hart.store(hartid, Ordering::Release);
    }

    /// Return whether a Hart still owns this task's live execution context.
    #[inline]
    pub(crate) fn is_on_cpu(&self) -> bool {
        self.on_cpu.load(Ordering::Acquire)
    }

    /// Claim the task immediately before restoring its saved context.
    #[inline]
    pub(crate) fn mark_on_cpu(&self) {
        self.on_cpu.store(true, Ordering::Release);
    }

    /// Publish that the previous context switch has completed.
    #[inline]
    pub(crate) fn mark_off_cpu(&self) {
        self.on_cpu.store(false, Ordering::Release);
    }

    /// Update the thread's affinity and return the Hart it was previously
    /// running on. Shared CFS entries remain in one queue and are filtered by
    /// the new mask when selected.
    pub fn set_cpu_affinity(&self, requested_mask: usize) -> usize {
        let mask = requested_mask & self.allowed_cpu_mask();
        debug_assert_ne!(mask, 0);
        self.cpu_affinity.store(mask, Ordering::Release);
        let old_hart = self.scheduled_hart();
        if mask & (1usize << old_hart) == 0 {
            let new_hart = Self::choose_hart(mask, old_hart.wrapping_add(1));
            self.scheduled_hart.store(new_hart, Ordering::Release);
        }
        old_hart
    }
    /// 获取当前进程的pid
    pub fn pid(&self) -> usize {
        self.process.pid
    }
    /// 获取父进程pid
    pub fn ppid(&self) -> usize {
        self.process.ppid()
    }
    /// 只有initproc会调用
    pub fn new(elf_data: &[u8]) -> Arc<Self> {
        // memory_set with elf program headers/trampoline/trap context/user stack
        let (memory_set, user_heapbottom, entry_point, _) =
            MemorySetInner::from_elf(elf_data).expect("initproc: OOM during ELF load");
        println!("Entry Point: {:#x}", entry_point);
        // alloc a pid and a kernel stack in kernel space
        let tid_handle = TidHandle::alloc().unwrap();
        let kernel_stack = KernelStackOnHeap::new();
        let kernel_stack_top = kernel_stack.top();
        debug!("TCB::new kstack top = {:#x}", kernel_stack_top);
        let memory_set = Arc::new(MemorySet::new(memory_set));
        let sig_table = Arc::new(Mutex::new(SigTable::new()));
        let process = Process::new(
            memory_set.clone(),
            sig_table.clone(),
            Arc::new(FdTable::new_with_stdio()),
            Arc::new(FSInfo::new_initproc()),
            tid_handle.0,
            0,
            tid_handle.0,
            tid_handle.0,
        );
        let task = Self {
            tid: tid_handle,
            kernel_stack,
            process: process.clone(),
            cpu_affinity: AtomicUsize::new(Self::default_cpu_affinity(process.home_hart())),
            scheduled_hart: AtomicUsize::new(process.home_hart()),
            on_cpu: AtomicBool::new(false),
            #[cfg(feature = "perf")]
            ext4_resource_lock_counts: [const { AtomicUsize::new(0) }; 9],
            interrupted: AtomicBool::new(false),
            interrupt_waker: AtomicWaker::new(),
            sched_entity: SchedEntity::new(),
            inner: RemoteTlbMutex::new(TaskControlBlockInner {
                trap_cx_ppn: 0.into(),
                trap_cx_bottom: 0,
                task_cx: TaskContext::goto_trap_return(kernel_stack_top),
                task_status: TaskStatus::Ready,
                // fd_table: Arc::new(FdTable::new_with_stdio()),
                // fs_info: Arc::new(Mutex::new(FsInfo::new_for_initproc())),
                time_data: TimeData::new(),
                user_heappoint: user_heapbottom,
                user_heapbottom,
                clear_child_tid: 0,
                vfork_wait_child: 0,
                present_page_fault_retry: None,
                #[cfg(feature = "perf")]
                vfork_published_at: 0,
                #[cfg(feature = "perf")]
                vfork_exec_started_at: 0,
                #[cfg(feature = "perf")]
                vfork_parent_ready_at: 0,
                sig_mask: SigSet::empty(),
                sigsuspend_restore_mask: None,
                alt_signal_stack: SignalStack::disabled(),
                sig_pending: SigSet::empty(),
                sig_pending_info: [None; SIG_MAX_NUM + 1],
                exec_teardown_kill: false,
                rseq: RseqState::default(),
                rseq_pending: false,
                timer: Arc::new(Timer::new()),
                robust_list: RobustListHead::default(),
                user_id: 0,
                effective_uid: 0,
                saved_uid: 0,
                real_gid: 0,
                effective_gid: 0,
                saved_gid: 0,
                capabilities: CapabilitySets::full(),
                pdeath_signal: 0,
                no_new_privs: false,
                seccomp_state: SeccompState::Disabled,
                mce_kill_policy: 2,
                timer_slack_ns: 50_000,
                futex_pa: 0,
                futex_key: 0,
                futex_timedout: false,
                sig_eintr: false,
                sigtimedwait_timedout: false,
                nice: 0,
            }),
        };
        let arc_task = Arc::new(task);
        process.meta_lock().tasks.push(Arc::downgrade(&arc_task));
        let mut task_inner = arc_task.inner_lock();
        let ustack_top = arc_task.alloc_user_res(&mut task_inner);
        // prepare TrapContext in user space
        let trap_cx = task_inner.trap_cx();
        *trap_cx = TrapContext::app_init_context(entry_point, ustack_top, kernel_stack_top);
        drop(task_inner);
        create_proc_dir_and_file(
            process.pid,
            0,
            process.pgid(),
            "initproc",
            &memory_set,
            0,
            0,
            0,
            0,
            0,
            0,
        )
        .expect("create initproc proc files");
        arc_task
    }
    /// exec的主逻辑
    pub fn exec(
        &self,
        elf_data: &[u8],
        executable_file: &Arc<OSFile>,
        argv: &[Vec<u8>],
        env: &[Vec<u8>],
    ) -> Result<(), SysErrNo> {
        //用户栈高地址到低地址：环境变量字符串/参数字符串/aux辅助向量/环境变量地址数组/参数地址数组/参数数量
        // memory_set with elf program headers/trampoline/trap context/user stack
        debug!("exec: goto from_elf");
        #[cfg(feature = "perf")]
        let from_elf_begin = get_ticks();
        let (memory_set, user_hp, entry_point, mut auxv) =
            MemorySetInner::from_elf_file(elf_data, executable_file).map_err(|_| {
                error!("exec: OOM during ELF load");
                SysErrNo::ENOMEM
            })?;
        #[cfg(feature = "perf")]
        crate::utils::perf::record_exec_from_elf_duration(
            get_ticks().saturating_sub(from_elf_begin),
        );
        validate_exec_stack_layout(argv, env, auxv.len())?;

        debug!("exec: return from from_elf");
        #[cfg(feature = "perf")]
        let stack_begin = get_ticks();
        let memory_set = MemorySet::new(memory_set);
        let (ustack_top, trap_cx_bottom, trap_cx_ppn) = alloc_user_res_in_memory_set(&memory_set)?;
        let (user_sp, argv_base, envp_base) =
            prepare_exec_stack(&memory_set, ustack_top, argv, env, &mut auxv)?;
        let mut trap_cx =
            TrapContext::app_init_context(entry_point, user_sp, self.kernel_stack.top());
        trap_cx.set_a0(argv.len());
        trap_cx.set_a1(argv_base);
        trap_cx.set_a2(envp_base);
        let new_comm = argv.first().map(|argv0| task_comm_from_argv0(argv0));
        #[cfg(feature = "perf")]
        crate::utils::perf::record_exec_stack_duration(get_ticks().saturating_sub(stack_begin));

        #[cfg(feature = "perf")]
        let commit_begin = get_ticks();
        // execve replaces a process-wide address space.  No sibling may keep
        // an old trap context or user stack once that replacement happens.
        crate::task::kill_other_threads_before_exec(self);
        // The SIGKILLs used to collapse sibling threads are an internal exec
        // detail, not a termination of the replacement program.
        self.process.meta_lock().termination_signal = None;

        // Snapshot the parent task list before taking this task's inner lock.
        // The lock order is ProcessMeta -> TaskControlBlockInner; retaining
        // the metadata guard while waking a parent task would otherwise let a
        // concurrent scheduler path form an AB-BA cycle.
        let ppid = self.ppid();
        let parent_tasks = Process::get_process_arc_by_pid(ppid)
            .map(|parent_proc| parent_proc.meta_lock().tasks.clone());
        let mut wake_parent_tasks = Vec::new();
        wake_parent_tasks
            .try_reserve(parent_tasks.as_ref().map_or(0, Vec::len))
            .map_err(|_| SysErrNo::ENOMEM)?;

        let mut task_inner = self.inner_lock();
        task_inner.time_data.clear();
        #[cfg(feature = "perf")]
        let vfork_exec_started_at = task_inner.vfork_exec_started_at;

        debug!(
            "task_inner.clear_child_tid={:#x}",
            task_inner.clear_child_tid
        );

        // execve会替换当前活跃的地址空间，clear_child_tid指向旧地址空间，
        // 替换后在新地址空间中没有对应映射，退出时translate_va会panic
        // 因此必须在替换前，在旧地址空间中完成写0和futex_wake
        if task_inner.clear_child_tid != 0 {
            let old_proc = &self.process;
            let old_memory_set = old_proc.memory_set_arc();
            let _ = copy_to_user(
                &old_memory_set,
                task_inner.clear_child_tid as usize,
                &[0u8; 4],
            );
            if let Some(pa) =
                old_memory_set.translate_va(VirtAddr::from(task_inner.clear_child_tid))
            {
                futex_wake_up(pa.0, 1);
            }
            drop(old_memory_set);
            task_inner.clear_child_tid = 0;
        }

        // Install the new page table on this hart before replacing the process
        // slot.  Replacing the slot drops the last Arc to the old address space
        // in the usual exec path; without this activation, its root page can be
        // recycled while satp still points at it and the next kernel allocation
        // faults or spins on a corrupted allocator lock.
        memory_set.activate();
        self.process
            .change_memory_set_and_sigtable(memory_set, SigTable::new());

        task_inner.sig_mask = SigSet::empty();
        task_inner.sigsuspend_restore_mask = None;
        task_inner.alt_signal_stack = SignalStack::disabled();
        task_inner.sig_pending = SigSet::empty();
        task_inner.sig_pending_info = [None; SIG_MAX_NUM + 1];
        task_inner.exec_teardown_kill = false;
        // robust_list is an address in the old image.  Keeping it across exec
        // would make a signal arriving before the new libc calls
        // set_robust_list() interpret stale user memory during thread exit.
        task_inner.robust_list = RobustListHead::default();
        // rseq retains a pointer into the replaced user image, so exec starts
        // with no registered area.
        task_inner.rseq = RseqState::default();
        task_inner.rseq_pending = true;
        self.process.fd_table.close_on_exec();
        task_inner.trap_cx_ppn = trap_cx_ppn;
        task_inner.trap_cx_bottom = trap_cx_bottom;
        *task_inner.trap_cx() = trap_cx;
        task_inner.user_heappoint = user_hp;
        task_inner.user_heapbottom = user_hp;
        drop(task_inner);
        if let Some(new_comm) = new_comm {
            self.process.meta_lock().comm = new_comm;
        }

        // vfork(2) releases its parent only after the child no longer uses
        // the shared address space. The new page table and trap context above
        // are fully installed at this point.
        #[cfg(feature = "perf")]
        let vfork_parent_ready_at = get_ticks();
        #[cfg(feature = "perf")]
        let mut released_vfork_parent = false;
        if let Some(parent_tasks) = parent_tasks {
            for task_weak in &parent_tasks {
                if let Some(t) = task_weak.upgrade() {
                    let mut parent_inner = t.inner_lock();
                    if parent_inner.vfork_wait_child == self.tid()
                        && parent_inner.task_status == TaskStatus::VforkBlocked
                    {
                        parent_inner.vfork_wait_child = 0;
                        #[cfg(feature = "perf")]
                        {
                            parent_inner.vfork_parent_ready_at = vfork_parent_ready_at;
                            released_vfork_parent = true;
                        }
                        parent_inner.task_status = TaskStatus::Ready;
                        drop(parent_inner);
                        wake_parent_tasks.push(t);
                    }
                }
            }
        }
        for parent_task in wake_parent_tasks {
            crate::task::ready_queue::add_task(&parent_task);
        }
        #[cfg(feature = "perf")]
        if released_vfork_parent {
            crate::utils::perf::record_vfork_release_exec();
            if vfork_exec_started_at != 0 {
                crate::utils::perf::record_vfork_exec_to_parent_ready_duration(
                    vfork_parent_ready_at.saturating_sub(vfork_exec_started_at),
                );
            }
        }
        #[cfg(feature = "perf")]
        crate::utils::perf::record_exec_commit_duration(get_ticks().saturating_sub(commit_begin));
        Ok(())
    }
    /// 复制进程，注意这里需要实现 fork 的主要逻辑
    ///
    /// 采用两阶段模式避免死锁：
    /// 1. 在父进程锁内提取所有需要的数据（Arc clone + Copy 字段）
    /// 2. 释放父进程锁后再构造和设置子进程
    pub fn clone_process(
        self: &Arc<TaskControlBlock>,
        flags: CloneFlags,
        exit_signal: i32,
        stack: usize,
        parent_tid: *mut u32,
        tls: usize,
        child_tid: *mut u32,
    ) -> Result<Arc<TaskControlBlock>, SysErrNo> {
        #[cfg(feature = "perf")]
        let clone_process_begin = get_ticks();
        let tid_handle = TidHandle::alloc().unwrap();
        let kernel_stack = KernelStackOnHeap::new();
        let kernel_stack_top = kernel_stack.top();
        debug!("TCB::new kstack top = {:#x}", kernel_stack_top);

        // ==================== Phase 1: 提取父进程元数据和任务状态 ====================
        //
        // 退出路径的锁顺序是 ProcessMeta -> TaskControlBlockInner。不要在
        // 持有 TaskControlBlockInner 时再获取 ProcessMeta，否则父进程并发
        // fork、子进程退出会形成 AB-BA 死锁。
        let (
            child_memory_set_arc,
            child_fs_info,
            child_fd_table,
            child_sig_table,
            child_pid,
            child_ppid,
            child_timer,
            child_sig_mask,
            child_alt_signal_stack,
            clear_child_tid,
            parent_memory_set_arc,
            parent_trap_cx,
            parent_heappoint,
            parent_heapbottom,
            parent_user_id,
            parent_euid,
            parent_suid,
            parent_rgid,
            parent_egid,
            parent_sgid,
            parent_capabilities,
            parent_nice,
            parent_rseq,
            parent_no_new_privs,
            parent_seccomp_state,
            parent_mce_kill_policy,
            parent_timer_slack_ns,
            parent_comm,
            parent_pgid,
            parent_sid,
        );
        let parent_pid;
        {
            let parent_meta = self.process.meta_lock();
            parent_pid = parent_meta.parent_pid;
            parent_pgid = parent_meta.pgid;
            parent_sid = parent_meta.sid;
            parent_comm = parent_meta.comm.clone();
        }
        #[cfg(feature = "perf")]
        crate::utils::perf::record_clone_bootstrap_duration(
            get_ticks().saturating_sub(clone_process_begin),
        );

        // Snapshot the resource-slot Arc while holding TaskControlBlockInner,
        // following the documented lock order. This only takes and releases
        // the slot lock; no MemorySet-internal lock is acquired here.
        #[cfg(feature = "perf")]
        let parent_state_begin = get_ticks();
        {
            let parent_inner = self.inner.lock();
            parent_memory_set_arc = self.process.memory_set_arc();

            clear_child_tid = if flags.contains(CloneFlags::CLONE_CHILD_CLEARTID) {
                child_tid as usize
            } else {
                0
            };

            // 确定 pid / 进程归属
            if flags.contains(CloneFlags::CLONE_THREAD) {
                child_pid = self.pid();
                child_ppid = parent_pid;
                child_timer = Arc::clone(&parent_inner.timer);
                child_sig_mask = parent_inner.sig_mask;
            } else {
                child_pid = tid_handle.0;
                child_ppid = if flags.contains(CloneFlags::CLONE_PARENT) {
                    parent_pid
                } else {
                    self.pid()
                };
                child_timer = Arc::new(Timer::new());
                child_sig_mask = parent_inner.sig_mask;
            }
            // Linux clears the alternate stack for clone(CLONE_VM) threads,
            // except the CLONE_VM | CLONE_VFORK exec hand-off case.
            child_alt_signal_stack = if flags.contains(CloneFlags::CLONE_VM)
                && !flags.contains(CloneFlags::CLONE_VFORK)
            {
                SignalStack::disabled()
            } else {
                parent_inner.alt_signal_stack
            };

            // 提取父进程 inner 中需要复制给子进程的字段
            parent_trap_cx = *parent_inner.trap_cx();
            parent_heappoint = parent_inner.user_heappoint;
            parent_heapbottom = parent_inner.user_heapbottom;
            parent_user_id = parent_inner.user_id;
            parent_euid = parent_inner.effective_uid;
            parent_suid = parent_inner.saved_uid;
            parent_rgid = parent_inner.real_gid;
            parent_egid = parent_inner.effective_gid;
            parent_sgid = parent_inner.saved_gid;
            parent_capabilities = parent_inner.capabilities;
            parent_nice = parent_inner.nice;
            parent_no_new_privs = parent_inner.no_new_privs;
            parent_seccomp_state = parent_inner.seccomp_state.clone();
            parent_mce_kill_policy = parent_inner.mce_kill_policy;
            parent_timer_slack_ns = parent_inner.timer_slack_ns;
            // Linux inherits rseq on fork but clears it for CLONE_VM, whose
            // child gets a distinct thread-local rseq ABI area.
            parent_rseq = if flags.contains(CloneFlags::CLONE_VM) {
                RseqState::default()
            } else {
                parent_inner.rseq
            };
        } // parent_inner 在此释放
        #[cfg(feature = "perf")]
        crate::utils::perf::record_clone_parent_state_duration(
            get_ticks().saturating_sub(parent_state_begin),
        );

        #[cfg(feature = "perf")]
        let address_space_start = get_ticks();
        // Do not hold TaskControlBlockInner while taking MemorySet's write
        // lock. Pre-faulting a shared mapping can enter ext4 and block, which
        // would otherwise strand this task's PCB lock and the parent MM lock.
        child_memory_set_arc = if flags.contains(CloneFlags::CLONE_VM) {
            Arc::clone(&parent_memory_set_arc)
        } else {
            Arc::new(MemorySet::new(MemorySetInner::from_existed_user(
                &parent_memory_set_arc,
            )))
        };
        #[cfg(feature = "perf")]
        crate::utils::perf::record_clone_address_space_duration(
            get_ticks().saturating_sub(address_space_start),
        );

        child_fs_info = if flags.contains(CloneFlags::CLONE_FS) {
            Arc::clone(&self.process.fs_info)
        } else {
            Arc::new(FSInfo::from_another(&self.process.fs_info))
        };
        child_fd_table = if flags.contains(CloneFlags::CLONE_FILES) {
            Arc::clone(&self.process.fd_table)
        } else {
            Arc::new(FdTable::from_another(&self.process.fd_table))
        };
        child_sig_table = if flags.contains(CloneFlags::CLONE_SIGHAND) {
            self.process.sig_table_arc()
        } else if flags.contains(CloneFlags::CLONE_CLEAR_SIGHAND) {
            Arc::new(Mutex::new(SigTable::new()))
        } else {
            Arc::new(Mutex::new(
                self.process
                    .with_sigtable(|sigtable| SigTable::from_another(sigtable)),
            ))
        };

        // CLONE_PARENT_SETTID accesses user memory, so it must stay outside
        // the parent PCB lock as well.
        if flags.contains(CloneFlags::CLONE_PARENT_SETTID) {
            copy_to_user_val(&*parent_memory_set_arc, parent_tid, &(tid_handle.0 as u32))?;
        }

        // Process::new() 会登记父子关系并获取 ProcessMeta。必须在父任务
        // inner 锁释放后执行，避免与子进程退出路径反向获取锁。
        #[cfg(feature = "perf")]
        let process_create_begin = get_ticks();
        let process_arc = if flags.contains(CloneFlags::CLONE_THREAD) {
            self.process.clone()
        } else if flags.contains(CloneFlags::CLONE_VM) && !flags.contains(CloneFlags::CLONE_VFORK) {
            // Start a regular CLONE_VM child on its parent's hart for cache
            // locality. Its task-level all-hart affinity remains movable
            // because remote TLB shootdown now protects the shared MemorySet.
            // CLONE_VM | CLONE_VFORK is different: the parent is marked
            // VforkBlocked before this child is made runnable, and the child
            // replaces the shared address space with execve() before the
            // parent can resume. Giving that exec hand-off a new process
            // placement lets Cargo's posix_spawn rustc workers use all harts.
            Process::new_on_hart(
                child_memory_set_arc.clone(),
                child_sig_table.clone(),
                child_fd_table,
                child_fs_info,
                child_pid,
                child_ppid,
                parent_pgid,
                parent_sid,
                self.process.home_hart(),
            )
        } else {
            Process::new(
                child_memory_set_arc.clone(),
                child_sig_table.clone(),
                child_fd_table,
                child_fs_info,
                child_pid,
                child_ppid,
                parent_pgid,
                parent_sid,
            )
        };
        #[cfg(feature = "perf")]
        crate::utils::perf::record_clone_process_create_duration(
            get_ticks().saturating_sub(process_create_begin),
        );

        // ==================== Phase 2: 构造子进程（不持有父进程锁）====================
        #[cfg(feature = "perf")]
        let task_setup_begin = get_ticks();
        process_arc.meta_lock().comm = parent_comm;

        let (child_cpu_affinity, child_scheduled_hart) = if flags.contains(CloneFlags::CLONE_THREAD)
        {
            let affinity = self.cpu_affinity();
            (
                affinity,
                Self::choose_hart(affinity, self.scheduled_hart().wrapping_add(1)),
            )
        } else {
            let home_hart = process_arc.home_hart();
            (Self::default_cpu_affinity(home_hart), home_hart)
        };

        let child = Arc::new(TaskControlBlock {
            tid: tid_handle,
            kernel_stack,
            process: process_arc,
            cpu_affinity: AtomicUsize::new(child_cpu_affinity),
            scheduled_hart: AtomicUsize::new(child_scheduled_hart),
            on_cpu: AtomicBool::new(false),
            #[cfg(feature = "perf")]
            ext4_resource_lock_counts: [const { AtomicUsize::new(0) }; 9],
            interrupted: AtomicBool::new(false),
            interrupt_waker: AtomicWaker::new(),
            // First enqueue places the child in its destination hart's
            // min_vruntime coordinate system.
            sched_entity: SchedEntity::new(),
            inner: RemoteTlbMutex::new(TaskControlBlockInner {
                trap_cx_ppn: 0.into(),
                trap_cx_bottom: 0,
                task_cx: TaskContext::goto_trap_return(kernel_stack_top),
                task_status: TaskStatus::Ready,
                time_data: TimeData::new(),
                user_heappoint: parent_heappoint,
                user_heapbottom: parent_heapbottom,
                clear_child_tid,
                vfork_wait_child: 0,
                present_page_fault_retry: None,
                #[cfg(feature = "perf")]
                vfork_published_at: 0,
                #[cfg(feature = "perf")]
                vfork_exec_started_at: 0,
                #[cfg(feature = "perf")]
                vfork_parent_ready_at: 0,
                sig_mask: child_sig_mask,
                sigsuspend_restore_mask: None,
                alt_signal_stack: child_alt_signal_stack,
                sig_pending: SigSet::empty(),
                sig_pending_info: [None; SIG_MAX_NUM + 1],
                exec_teardown_kill: false,
                rseq: parent_rseq,
                rseq_pending: parent_rseq != RseqState::default(),
                timer: child_timer,
                robust_list: RobustListHead::default(),
                user_id: parent_user_id,
                effective_uid: parent_euid,
                saved_uid: parent_suid,
                real_gid: parent_rgid,
                effective_gid: parent_egid,
                saved_gid: parent_sgid,
                capabilities: parent_capabilities,
                pdeath_signal: 0,
                no_new_privs: parent_no_new_privs,
                seccomp_state: parent_seccomp_state,
                mce_kill_policy: parent_mce_kill_policy,
                timer_slack_ns: parent_timer_slack_ns,
                futex_pa: 0,
                futex_key: 0,
                futex_timedout: false,
                sig_eintr: false,
                sigtimedwait_timedout: false,
                nice: parent_nice,
            }),
        });

        // 将子进程/线程注册到进程的任务列表
        {
            let mut child_meta = child.process.meta_lock();
            child_meta.tasks.retain(|weak| weak.upgrade().is_some());
            child_meta.tasks.push(Arc::downgrade(&child));
        }

        let mut child_inner = child.inner_lock();

        if flags.contains(CloneFlags::CLONE_VM) {
            if stack != 0 {
                child.alloc_trap_context_only(&mut child_inner);
            } else {
                child.alloc_user_res(&mut child_inner);
            }
            *child_inner.trap_cx() = parent_trap_cx;
            child_inner.trap_cx().set_a0(0);
        } else {
            // fork: 从父进程复制内存
            child.alloc_user_res(&mut child_inner);
            *child_inner.trap_cx() = parent_trap_cx;

            let child_proc = &child.process;
            let child_mm = child_proc.memory_set_arc();
            let child_stack_bottom = child_mm
                .get_ref()
                .areas
                .iter()
                .find(|area| {
                    area.area_type == MapAreaType::Stack
                        && !area.mmap_flags.contains(MmapFlags::MAP_STACK)
                })
                .map(|area| area.vpn_range.start())
                .expect("fork: child has no Stack area");
            child_mm.lazy_clone_area(child_stack_bottom, &parent_memory_set_arc);
            child_inner.trap_cx().set_a0(0);
        }

        let trap_cx = child_inner.trap_cx();
        trap_cx.kernel_stack = kernel_stack_top;
        if stack != 0 {
            trap_cx.set_sp(stack);
        }
        if flags.contains(CloneFlags::CLONE_SETTLS) {
            trap_cx.set_tp(tls);
        }

        drop(child_inner);

        // CLONE_CHILD_SETTID: 写入子进程地址空间
        if flags.contains(CloneFlags::CLONE_CHILD_SETTID) {
            let child_proc_inner = &child.process;
            let child_mem = child_proc_inner.memory_set_arc();
            copy_to_user_val(&*child_mem, child_tid, &(child.tid() as u32))?;
        }

        // exit_signal: 仅 fork（非线程）才设置，线程共享进程不能覆盖已有值
        if !flags.contains(CloneFlags::CLONE_THREAD) {
            let mut child_meta = child.process.meta_lock();
            child_meta.exit_signal = exit_signal;
            debug!(
                "[clone_process] fork pid={}, flags={:?}, exit_signal={}",
                child_pid, flags, child_meta.exit_signal
            );
        }
        #[cfg(feature = "perf")]
        crate::utils::perf::record_clone_task_setup_duration(
            get_ticks().saturating_sub(task_setup_begin),
        );

        // Threads share the process, so /proc/<pid> is only created for a new process.
        if !flags.contains(CloneFlags::CLONE_THREAD) {
            #[cfg(feature = "perf")]
            let procfs_start = get_ticks();
            let _ = create_proc_dir(child_pid);
            #[cfg(feature = "perf")]
            crate::utils::perf::record_clone_procfs_register_duration(
                get_ticks().saturating_sub(procfs_start),
            );
        }

        // VFORK: 挂起父进程直到子进程 exec 或退出
        #[cfg(feature = "perf")]
        let publish_begin = get_ticks();
        {
            let mut parent_inner = self.inner_lock();
            if flags.contains(CloneFlags::CLONE_VFORK) {
                parent_inner.vfork_wait_child = child.tid();
                #[cfg(feature = "perf")]
                {
                    parent_inner.vfork_parent_ready_at = 0;
                }
                parent_inner.task_status = TaskStatus::VforkBlocked;
            }
        }
        #[cfg(feature = "perf")]
        if flags.contains(CloneFlags::CLONE_VFORK) {
            child.inner_lock().vfork_published_at = get_ticks();
        }
        tid_to_task::insert(child.tid(), &child);
        if !flags.contains(CloneFlags::CLONE_THREAD) {
            if flags.contains(CloneFlags::CLONE_FILES) {
                child.process.fd_table.acquire_owner();
            }
            if flags.contains(CloneFlags::CLONE_FS) {
                child.process.fs_info.acquire_owner();
            }
        }
        #[cfg(feature = "perf")]
        crate::utils::perf::record_clone_publish_duration(
            get_ticks().saturating_sub(publish_begin),
        );
        #[cfg(feature = "perf")]
        crate::utils::perf::record_clone_process_total_duration(
            get_ticks().saturating_sub(clone_process_begin),
        );
        Ok(child.clone())
    }

    ///修改数据段大小，懒分配
    pub fn growproc(&self, grow_size: isize) -> Option<usize> {
        let mut inner = self.inner_lock();
        let process = &self.process;
        let memory_set = process.memory_set_arc();

        if grow_size == 0 {
            return Some(inner.user_heappoint);
        }

        let ret = memory_set.grow(grow_size, inner.user_heappoint, inner.user_heapbottom);

        if let Some(ret) = ret {
            inner.user_heappoint = ret;
        }
        ret
    }
    pub fn set_status(&self, status: TaskStatus) {
        let mut task_inner = self.inner_lock();
        task_inner.task_status = status;
        drop(task_inner);
    }
    pub fn poll_interrupt(&self, cx: &mut core::task::Context) -> core::task::Poll<()> {
        // Register first, then check the condition. If interrupt() races with
        // registration, AtomicWaker guarantees either this check observes the
        // flag or the newly registered waker is notified.
        self.interrupt_waker.register(cx.waker());
        if self.interrupted.swap(false, Ordering::AcqRel) {
            core::task::Poll::Ready(())
        } else {
            core::task::Poll::Pending
        }
    }
    pub fn clear_interrupt(&self) {
        self.interrupted.store(false, Ordering::Release);
    }
    pub fn clear_interrupt_waiter(&self) {
        self.interrupted.store(false, Ordering::Release);
        self.interrupt_waker.take();
    }
    pub fn interrupt(&self) {
        self.interrupted.store(true, Ordering::Release);
        self.interrupt_waker.wake();
    }
    /// Wake a task that is sleeping in an interruptible kernel wait.
    pub fn wake_interruptible(&self) -> bool {
        if let Some(waker) = self.interrupt_waker.take() {
            self.interrupted.store(true, Ordering::Release);
            waker.wake();
            true
        } else {
            false
        }
    }
    /// Return whether the task has an interruptible wait registered.
    pub fn has_interruptible_waiter(&self) -> bool {
        if let Some(waker) = self.interrupt_waker.take() {
            self.interrupt_waker.register(&waker);
            true
        } else {
            false
        }
    }
    /// 分配用户栈和 trap context 区域，并返回用户栈顶地址
    fn alloc_user_res(&self, task_inner: &mut TaskControlBlockInner) -> usize {
        let memory_set = self.process.memory_set_arc();
        let (ustack_top, trap_cx_bottom, trap_cx_ppn) = alloc_user_res_in_memory_set(&memory_set)
            .expect("failed to allocate task user resources");
        task_inner.trap_cx_ppn = trap_cx_ppn;
        task_inner.trap_cx_bottom = trap_cx_bottom;
        ustack_top
    }
    /// 仅为线程分配 trap context 区域（不分配栈空间，栈由用户提供）。
    /// 用于 CLONE_THREAD 且用户指定了栈地址的场景。
    fn alloc_trap_context_only(&self, task_inner: &mut TaskControlBlockInner) {
        let (trap_cx_bottom, trap_cx_ppn) = {
            let proc_inner = &self.process;
            let memory_set = proc_inner.memory_set_arc();
            memory_set.with_frame_preserving_mut(|ms| {
                let (t_cx, _) = ms.insert_framed_area_with_hint(
                    USER_TRAP_CONTEXT_TOP,
                    PAGE_SIZE,
                    MapPermission::R | MapPermission::W,
                    MapAreaType::Trap,
                );
                let t_cx_ppn = ms.translate(VirtAddr::from(t_cx).floor()).unwrap();
                (t_cx, t_cx_ppn)
            })
        };
        task_inner.trap_cx_ppn = trap_cx_ppn;
        task_inner.trap_cx_bottom = trap_cx_bottom;
    }
}

#[derive(Copy, Clone, PartialEq, Debug)]
pub enum TaskStatus {
    Ready,
    Running,
    Zombie,
    Blocked,
    /// Stopped by SIGSTOP/SIGTSTP/SIGTTIN/SIGTTOU until a SIGCONT arrives.
    Stopped,
    /// VFORK: parent is suspended until child execs or exits.
    VforkBlocked,
}
pub type TaskRef = Arc<TaskControlBlock>;
pub type WeakTaskRef = Weak<TaskControlBlock>;
