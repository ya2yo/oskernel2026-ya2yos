//!Implementation of [`TaskControlBlock`]
use super::super::process::Process;
use super::super::{scheduler::SchedEntity, tid_to_task, RseqState, TaskContext, TidHandle};
use super::exec::alloc_user_res_in_memory_set;
#[cfg(feature = "fault-diagnostics")]
use crate::signal::SignalFrameTrace;
use crate::{
    arch::{
        context::TrapContext,
        hardware::MAX_SUPPORTED_HARTS,
        memory_layout::{PAGE_SIZE, USER_TRAP_CONTEXT_TOP},
    },
    fs::{create_proc_dir_and_file, FSInfo, FdTable, DEFAULT_DIR_MODE, DEFAULT_FILE_MODE},
    mm::{
        MapAreaType, MapPermission, MemorySet, MemorySetInner, PhysPageNum, VirtAddr, VirtPageNum,
    },
    signal::{SigInfo, SigSet, SigTable, SignalStack, SIG_MAX_NUM},
    task::{kernel_stack::KernelStackOnHeap, SeccompAction, SeccompState},
    timer::{TimeData, Timer},
};
use alloc::sync::{Arc, Weak};
use core::fmt::Debug;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use futures_util::task::AtomicWaker;
use linux_raw_sys::general::CAP_LAST_CAP;
use log::debug;
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
    pub(super) tid: TidHandle,
    pub(super) kernel_stack: KernelStackOnHeap,
    pub process: Arc<Process>,
    /// Linux-visible CPU affinity mask for this thread (not its whole process).
    pub(super) cpu_affinity: AtomicUsize,
    /// Hart that last selected this thread, also used for timer ownership and
    /// affinity-directed migration notifications.
    pub(super) scheduled_hart: AtomicUsize,
    /// Linux task_struct::on_cpu equivalent. It remains set until the previous
    /// Hart has completely switched away from this task's saved context.
    pub(super) on_cpu: AtomicBool,
    /// Counts of lwext4 resource-lock classes currently held by this task.
    ///
    /// This exists only in diagnostic builds. Unlike a per-Hart marker, it
    /// remains valid while a blocked task is resumed on another Hart.
    #[cfg(feature = "perf")]
    pub(super) ext4_resource_lock_counts: [AtomicUsize; 9],
    // mutable
    // 异步中断/信号同步
    pub interrupted: AtomicBool,
    pub interrupt_waker: AtomicWaker,
    /// 调度器私有运行时间状态；RR 下为空，CFS 下保存 vruntime。
    pub(crate) sched_entity: SchedEntity,
    /// Internal task state. Timer-interrupt paths can contend on this lock
    /// while a page-table update awaits a remote TLB acknowledgement.
    pub(super) inner: RemoteTlbMutex<TaskControlBlockInner>,
}

impl Drop for TaskControlBlock {
    fn drop(&mut self) {
        debug!("TCB {} dropped", self.tid());
    }
}

pub struct TaskControlBlockInner {
    pub(super) trap_cx_ppn: PhysPageNum, // TrapContext缓冲区物理页
    pub trap_cx_bottom: usize,           // TrapContext缓冲区虚拟地址基地址

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
    pub(super) present_page_fault_retry: Option<VirtPageNum>,
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
    /// Recent successfully constructed signal frames, retained only to
    /// diagnose a later `rt_sigreturn` that arrives with a bad stack pointer
    /// or a corrupted user frame.
    #[cfg(feature = "fault-diagnostics")]
    pub(crate) signal_frame_trace: SignalFrameTrace,
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

impl TaskControlBlock {
    /// Mask of all harts configured into this kernel image.
    #[inline]
    pub fn online_cpu_mask() -> usize {
        let hart_num = crate::arch::hardware::hart_count().min(MAX_SUPPORTED_HARTS);
        if hart_num >= usize::BITS as usize {
            usize::MAX
        } else {
            (1usize << hart_num) - 1
        }
    }

    #[inline]
    pub(super) fn default_cpu_affinity(home_hart: usize) -> usize {
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
    pub(super) fn choose_hart(mask: usize, start: usize) -> usize {
        debug_assert_ne!(mask & Self::online_cpu_mask(), 0);
        let hart_num = crate::arch::hardware::hart_count().min(MAX_SUPPORTED_HARTS);
        for offset in 0..hart_num {
            let hart = (start + offset) % hart_num;
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
        hartid < crate::arch::hardware::hart_count().min(MAX_SUPPORTED_HARTS)
            && self.cpu_affinity() & (1usize << hartid) != 0
    }

    /// Publish the Hart that actually selected this task.
    #[inline]
    pub(crate) fn set_scheduled_hart(&self, hartid: usize) {
        debug_assert!(hartid < crate::arch::hardware::hart_count().min(MAX_SUPPORTED_HARTS));
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
                #[cfg(feature = "fault-diagnostics")]
                signal_frame_trace: SignalFrameTrace::new(),
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
    pub(super) fn alloc_user_res(&self, task_inner: &mut TaskControlBlockInner) -> usize {
        let memory_set = self.process.memory_set_arc();
        let (ustack_top, trap_cx_bottom, trap_cx_ppn) = alloc_user_res_in_memory_set(&memory_set)
            .expect("failed to allocate task user resources");
        task_inner.trap_cx_ppn = trap_cx_ppn;
        task_inner.trap_cx_bottom = trap_cx_bottom;
        ustack_top
    }
    /// 仅为线程分配 trap context 区域（不分配栈空间，栈由用户提供）。
    /// 用于 CLONE_THREAD 且用户指定了栈地址的场景。
    pub(super) fn alloc_trap_context_only(&self, task_inner: &mut TaskControlBlockInner) {
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
