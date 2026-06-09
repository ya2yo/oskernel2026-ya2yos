//!Implementation of [`TaskControlBlock`]
use super::super::process::Process;
use super::super::{
    aux::{Aux, AuxType},
    tid_to_task, TaskContext, TidHandle,
};
use crate::{
    arch::{
        context::TrapContext,
        memory_layout::{
            PAGE_SIZE, PRE_ALLOC_PAGES, USER_HEAP_SIZE, USER_STACK_SIZE, USER_STACK_TOP,
            USER_TRAP_CONTEXT_TOP,
        },
        page_table::PageTable,
    },
    fs::{
        create_proc_dir_and_file, open, FSInfo, FdTable, OpenFlags, DEFAULT_DIR_MODE,
        DEFAULT_FILE_MODE,
    },
    mm::{
        copy_to_user, copy_to_user_val, MapAreaType, MapPermission, MemorySet, MemorySetInner,
        PhysPageNum, VirtAddr,
    },
    signal::{SigSet, SigTable},
    syscall::CloneFlags,
    task::{futex::futex_wake_up, kernel_stack::KernelStackOnHeap, tid},
    timer::{TimeData, TimeVal, Timer},
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
use core::{
    sync::atomic::{AtomicBool, Ordering},
    task::Poll,
};
use futures_util::task::AtomicWaker;
use log::{debug, error};
use spin::{rwlock::RwLock, Mutex, MutexGuard};

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

pub struct TaskControlBlock {
    // immutable
    tid: TidHandle,
    kernel_stack: KernelStackOnHeap,
    pub process: Arc<Process>,
    // mutable
    // 异步中断/信号同步
    pub interrupted: AtomicBool,
    pub interrupt_waker: AtomicWaker,
    /// 内部主要数据，使用锁进行包含保护
    inner: Mutex<TaskControlBlockInner>,
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
    /// 被屏蔽的信号
    pub sig_mask: SigSet,
    /// 待处理信号集合
    pub sig_pending: SigSet,
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
    /// PR_SET_PDEATHSIG 设置的父进程死亡信号 (0 表示未设置)
    pub pdeath_signal: u8,

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
}

impl TaskControlBlock {
    pub fn inner_lock(&self) -> MutexGuard<'_, TaskControlBlockInner> {
        self.inner.try_lock().expect("fail to get task inner")
    }
    pub fn tid(&self) -> usize {
        self.tid.0
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
        let memory_set = Arc::new(RwLock::new(MemorySet::new(memory_set)));
        let sig_table = Arc::new(Mutex::new(SigTable::new()));
        let process = Process::new(
            memory_set.clone(),
            sig_table.clone(),
            Arc::new(FdTable::new_with_stdio()),
            Arc::new(FSInfo::new_initproc()),
            tid_handle.0,
            0,
        );
        let task = Self {
            tid: tid_handle,
            kernel_stack,
            process: process.clone(),
            interrupted: AtomicBool::new(false),
            interrupt_waker: AtomicWaker::new(),
            inner: Mutex::new(TaskControlBlockInner {
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
                sig_mask: SigSet::empty(),
                sig_pending: SigSet::empty(),
                timer: Arc::new(Timer::new()),
                robust_list: RobustListHead::default(),
                user_id: 0,
                effective_uid: 0,
                saved_uid: 0,
                real_gid: 0,
                effective_gid: 0,
                saved_gid: 0,
                pdeath_signal: 0,
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
        arc_task
    }
    /// exec的主逻辑
    pub fn exec(&self, elf_data: &[u8], argv: &[String], env: &mut [String]) -> Result<(), ()> {
        let mut task_inner = self.inner_lock();
        //用户栈高地址到低地址：环境变量字符串/参数字符串/aux辅助向量/环境变量地址数组/参数地址数组/参数数量
        // memory_set with elf program headers/trampoline/trap context/user stack
        debug!("exec: goto from_elf");
        let (memory_set, user_hp, entry_point, mut auxv) = MemorySetInner::from_elf(elf_data)
            .map_err(|_| {
                error!("exec: OOM during ELF load");
            })?;
        debug!("exec: return from from_elf");
        let memory_set = MemorySet::new(memory_set);

        task_inner.time_data.clear();

        // VFORK: wake up parent if it was suspended waiting for this task
        // (exec replaces the process image, which counts as "done" for vfork)
        let ppid = self.ppid();
        if let Some(parent_proc) = Process::get_process_arc_by_pid(ppid) {
            for task_weak in &parent_proc.meta_lock().tasks {
                if let Some(t) = task_weak.upgrade() {
                    let mut parent_inner = t.inner_lock();
                    if parent_inner.vfork_wait_child == self.tid()
                        && parent_inner.task_status == TaskStatus::VforkBlocked
                    {
                        parent_inner.vfork_wait_child = 0;
                        parent_inner.task_status = TaskStatus::Ready;
                        drop(parent_inner);
                        crate::task::ready_queue::add_task(&t);
                    }
                }
            }
        }

        debug!(
            "task_inner.clear_child_tid={:#x}",
            task_inner.clear_child_tid
        );

        // execve会替换当前活跃的地址空间，clear_child_tid指向旧地址空间，
        // 替换后在新地址空间中没有对应映射，退出时translate_va会panic
        // 因此必须在替换前，在旧地址空间中完成写0和futex_wake
        if task_inner.clear_child_tid != 0 {
            let old_proc = self.process.inner_lock();
            let old_memory_set = old_proc.get_locked_memory_set_read();
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

        self.process
            .change_memory_set_and_sigtable(memory_set, SigTable::new());

        // 重新分配用户资源
        let ustack_top = self.alloc_user_res(&mut task_inner);
        {
            self.process.inner_lock().fd_table.close_on_exec();
        }
        task_inner.sig_mask = SigSet::empty();
        task_inner.sig_pending = SigSet::empty();

        // 获取新地址空间用于栈写入
        let proc_inner = self.process.inner_lock();
        let proc_mem = proc_inner.get_locked_memory_set_read();

        let mut user_sp = ustack_top;

        // println!("user_sp:{:#X}  argv:{:?}", user_sp, argv);

        //环境变量内容入栈
        let mut envp = Vec::new();
        for env in env.iter() {
            user_sp -= env.len() + 1;
            envp.push(user_sp);
            // println!("{:#X}:{}", user_sp, env);
            for (j, c) in env.as_bytes().iter().enumerate() {
                copy_to_user_val(&*proc_mem, (user_sp + j) as *mut u8, c).unwrap();
            }
            copy_to_user_val(&*proc_mem, (user_sp + env.len()) as *mut u8, &0u8).unwrap();
        }
        envp.push(0);
        user_sp -= user_sp % size_of::<usize>();

        //存放字符串首址的数组
        let mut argvp = Vec::new();
        for arg in argv.iter() {
            // 计算字符串在栈上的地址
            user_sp -= arg.len() + 1;
            argvp.push(user_sp);
            // println!("{:#X}:{}", user_sp, arg);
            for (j, c) in arg.as_bytes().iter().enumerate() {
                copy_to_user_val(&*proc_mem, (user_sp + j) as *mut u8, c).unwrap();
            }
            // 添加字符串末尾的 null 字符
            copy_to_user_val(&*proc_mem, (user_sp + arg.len()) as *mut u8, &0u8).unwrap();
        }
        user_sp -= user_sp % size_of::<usize>(); //以8字节对齐
        argvp.push(0);

        //需要随便放16个字节，不知道干嘛用的。
        user_sp -= 16;
        auxv.push(Aux::new(AuxType::RANDOM, user_sp));
        for i in 0..0xf {
            copy_to_user_val(&*proc_mem, (user_sp + i) as *mut u8, &(i as u8)).unwrap();
        }
        user_sp -= user_sp % 16;

        // println!("aux:");
        //将auxv放入栈中
        auxv.push(Aux::new(AuxType::EXECFN, argvp[0]));
        auxv.push(Aux::new(AuxType::NULL, 0));
        for aux in auxv.iter().rev() {
            // println!("{:?}", aux);
            user_sp -= size_of::<Aux>();
            copy_to_user_val(&*proc_mem, user_sp as *mut usize, &(aux.aux_type as usize)).unwrap();
            copy_to_user_val(
                &*proc_mem,
                (user_sp + size_of::<usize>()) as *mut usize,
                &aux.value,
            )
            .unwrap();
        }

        //将环境变量指针数组放入栈中
        // println!("env pointers:");
        user_sp -= envp.len() * size_of::<usize>();
        let envp_base = user_sp;
        for (i, data) in envp.iter().enumerate() {
            copy_to_user_val(
                &*proc_mem,
                (user_sp + i * size_of::<usize>()) as *mut usize,
                data,
            )
            .unwrap();
        }

        // println!("arg pointers:");
        user_sp -= argvp.len() * size_of::<usize>();
        let argv_base = user_sp;
        //将参数指针数组放入栈中
        for (i, &data) in argvp.iter().enumerate() {
            copy_to_user_val(
                &*proc_mem,
                (user_sp + i * size_of::<usize>()) as *mut usize,
                &data,
            )
            .unwrap();
        }

        //将argc放入栈中
        user_sp -= size_of::<usize>();
        copy_to_user_val(&*proc_mem, user_sp as *mut usize, &argv.len()).unwrap();

        //以8字节对齐
        user_sp -= user_sp % size_of::<usize>();
        //println!("user_sp:{:#X}", user_sp);

        // 将设置了O_CLOEXEC位的文件描述符关闭
        proc_inner.fd_table.close_on_exec();

        let mut trap_cx =
            TrapContext::app_init_context(entry_point, user_sp, self.kernel_stack.top());
        trap_cx.set_a0(argv.len());
        trap_cx.set_a1(argv_base);
        trap_cx.set_a2(envp_base);
        *task_inner.trap_cx() = trap_cx;
        task_inner.user_heappoint = user_hp;
        task_inner.user_heapbottom = user_hp;
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
        let tid_handle = TidHandle::alloc().unwrap();
        let kernel_stack = KernelStackOnHeap::new();
        let kernel_stack_top = kernel_stack.top();
        debug!("TCB::new kstack top = {:#x}", kernel_stack_top);

        // ==================== Phase 1: 在父进程锁内提取数据 ====================
        let (
            child_memory_set_arc,
            child_fs_info,
            child_fd_table,
            child_sig_table,
            child_pid,
            child_ppid,
            child_timer,
            child_sig_mask,
            process_arc,
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
            parent_nice,
        );
        {
            let parent_inner = self.inner.lock();
            let parent_proc_inner = self.process.inner_lock();

            // 保存父进程 memory_set Arc（fork 时需要读取父进程页面来 clone_area）
            parent_memory_set_arc = parent_proc_inner.memory_set.clone();

            // 子进程 memory_set
            child_memory_set_arc = if flags.contains(CloneFlags::CLONE_VM) {
                parent_proc_inner.memory_set.clone()
            } else {
                Arc::new(RwLock::new(MemorySet::new(
                    MemorySetInner::from_existed_user(
                        &parent_proc_inner.get_locked_memory_set_read(),
                    ),
                )))
            };

            // fs / fd / sig
            child_fs_info = if flags.contains(CloneFlags::CLONE_FS) {
                Arc::clone(&parent_proc_inner.fs_info)
            } else {
                Arc::new(FSInfo::from_another(&parent_proc_inner.fs_info))
            };
            child_fd_table = if flags.contains(CloneFlags::CLONE_FILES) {
                Arc::clone(&parent_proc_inner.fd_table)
            } else {
                Arc::new(FdTable::from_another(&parent_proc_inner.fd_table))
            };
            child_sig_table = if flags.contains(CloneFlags::CLONE_SIGHAND) {
                parent_proc_inner.sig_table.clone()
            } else if flags.contains(CloneFlags::CLONE_CLEAR_SIGHAND) {
                Arc::new(Mutex::new(SigTable::new()))
            } else {
                Arc::new(Mutex::new(SigTable::from_another(
                    &parent_proc_inner.get_locked_sigtable(),
                )))
            };

            // CLONE_PARENT_SETTID: 写入父进程地址空间
            if flags.contains(CloneFlags::CLONE_PARENT_SETTID) {
                let parent_mem = parent_proc_inner.get_locked_memory_set_read();
                copy_to_user_val(&*parent_mem, parent_tid, &(tid_handle.0 as u32))?;
            }

            clear_child_tid = if flags.contains(CloneFlags::CLONE_CHILD_CLEARTID) {
                child_tid as usize
            } else {
                0
            };

            // 确定 pid / 进程归属
            if flags.contains(CloneFlags::CLONE_THREAD) {
                child_pid = self.pid();
                child_ppid = self.ppid();
                child_timer = Arc::clone(&parent_inner.timer);
                child_sig_mask = parent_inner.sig_mask;
                process_arc = self.process.clone();
            } else {
                child_pid = tid_handle.0;
                child_ppid = if flags.contains(CloneFlags::CLONE_PARENT) {
                    self.ppid()
                } else {
                    self.pid()
                };
                child_timer = Arc::new(Timer::new());
                child_sig_mask = parent_inner.sig_mask;
                process_arc = Process::new(
                    child_memory_set_arc.clone(),
                    child_sig_table.clone(),
                    child_fd_table,
                    child_fs_info,
                    child_pid,
                    child_ppid,
                );
            }

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
            parent_nice = parent_inner.nice;
        } // parent_inner, parent_proc_inner 在此释放

        // ==================== Phase 2: 构造子进程（不持有父进程锁）====================

        let child = Arc::new(TaskControlBlock {
            tid: tid_handle,
            kernel_stack,
            process: process_arc,
            interrupted: AtomicBool::new(false),
            interrupt_waker: AtomicWaker::new(),
            inner: Mutex::new(TaskControlBlockInner {
                trap_cx_ppn: 0.into(),
                trap_cx_bottom: 0,
                task_cx: TaskContext::goto_trap_return(kernel_stack_top),
                task_status: TaskStatus::Ready,
                time_data: TimeData::new(),
                user_heappoint: parent_heappoint,
                user_heapbottom: parent_heapbottom,
                clear_child_tid,
                vfork_wait_child: 0,
                sig_mask: child_sig_mask,
                sig_pending: SigSet::empty(),
                timer: child_timer,
                robust_list: RobustListHead::default(),
                user_id: parent_user_id,
                effective_uid: parent_euid,
                saved_uid: parent_suid,
                real_gid: parent_rgid,
                effective_gid: parent_egid,
                saved_gid: parent_sgid,
                pdeath_signal: 0,
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

        if flags.contains(CloneFlags::CLONE_THREAD) {
            if stack != 0 {
                child.alloc_trap_context_only(&mut child_inner);
            } else {
                child.alloc_user_res(&mut child_inner);
            }
            *child_inner.trap_cx() = parent_trap_cx;
            child_inner.trap_cx().set_a0(0);
        } else {
            // fork: 从父进程复制内存
            let parent_mem = parent_memory_set_arc.read();
            let parent_ref = parent_mem.get_ref();
            child.alloc_user_res(&mut child_inner);
            *child_inner.trap_cx() = parent_trap_cx;

            let child_proc = child.process.inner_lock();
            let child_mm = child_proc.get_locked_memory_set_read();
            let child_stack_bottom = child_mm
                .get_ref()
                .areas
                .iter()
                .find(|area| area.area_type == MapAreaType::Stack)
                .map(|area| area.vpn_range.start())
                .expect("fork: child has no Stack area");
            child_mm.lazy_clone_area(child_stack_bottom, parent_ref);
            child_mm.clone_area(
                VirtAddr::from(child_inner.trap_cx_bottom).floor(),
                parent_ref,
            );
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

        // CLONE_CHILD_SETTID: 写入子进程地址空间
        if flags.contains(CloneFlags::CLONE_CHILD_SETTID) {
            let child_proc_inner = child.process.inner_lock();
            let child_mem = child_proc_inner.get_locked_memory_set_read();
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

        // Threads share the process, so /proc/<pid> is only created for a new process.
        if !flags.contains(CloneFlags::CLONE_THREAD) {
            let child_proc = child.process.inner_lock();
            let child_mm = child_proc.get_locked_memory_set_read();
            create_proc_dir_and_file(child_pid, child_ppid, &child_mm);
        }

        // VFORK: 挂起父进程直到子进程 exec 或退出
        {
            let mut parent_inner = self.inner_lock();
            if flags.contains(CloneFlags::CLONE_VFORK) {
                parent_inner.vfork_wait_child = child.tid();
                parent_inner.task_status = TaskStatus::VforkBlocked;
            }
        }

        drop(child_inner);
        tid_to_task::insert(child.tid(), &child);
        Ok(child.clone())
    }

    ///修改数据段大小，懒分配
    pub fn growproc(&self, grow_size: isize) -> usize {
        let mut inner = self.inner_lock();
        let process = self.process.inner_lock();
        let memory_set = process.get_locked_memory_set_write();

        if grow_size == 0 {
            return inner.user_heappoint;
        }

        let ret = memory_set
            .get_mut()
            .grow(grow_size, inner.user_heappoint, inner.user_heapbottom);

        inner.user_heappoint = ret;
        ret
    }
    /// 检查计时器
    pub fn check_timer(&self) {
        let mut task_inner = self.inner_lock();
        let timer = task_inner.timer.clone();
        let now = TimeVal::now();
        if timer.trigger_once() {
            // 只触发一次,单次计时器
            if now > timer.last_time() + timer.timer().it_value {
                // log::info!("Timer Alarm Once");
                task_inner.sig_pending |= SigSet::SIGALRM;
                timer.set_trigger_once(false);
                timer.set_last_time(now);
            }
        } else if !timer.timer().it_interval.is_empty() {
            //间隔触发
            if now > timer.last_time() + timer.timer().it_interval {
                // log::info!("Timer Alarm!");
                task_inner.sig_pending |= SigSet::SIGALRM;
                timer.set_last_time(now);
            }
        }
    }
    pub fn set_status(&self, status: TaskStatus) {
        let mut task_inner = self.inner_lock();
        task_inner.task_status = status;
        drop(task_inner);
    }
    pub fn poll_interrupt(&self, cx: &mut core::task::Context) -> Poll<()> {
        if self.interrupted.swap(false, Ordering::AcqRel) {
            Poll::Ready(())
        } else {
            self.interrupt_waker.register(cx.waker());
            Poll::Pending
        }
    }
    pub fn clear_interrupt(&self) {
        self.interrupted.store(false, Ordering::Release);
    }
    pub fn interrupt(&self) {
        self.interrupted.store(true, Ordering::Release);
        self.interrupt_waker.wake();
    }
    /// 分配用户栈和 trap context 区域，并返回用户栈顶地址
    fn alloc_user_res(&self, task_inner: &mut TaskControlBlockInner) -> usize {
        let (ustack_top, trap_cx_bottom, trap_cx_ppn) = {
            let proc_inner = self.process.inner_lock();
            let memory_set = proc_inner.get_locked_memory_set_read();
            let (u_bottom, u_top) = memory_set.lazy_insert_framed_area_with_hint(
                USER_STACK_TOP,
                USER_STACK_SIZE,
                MapPermission::R | MapPermission::W | MapPermission::U,
                MapAreaType::Stack,
            );
            let (t_cx, _) = memory_set.insert_framed_area_with_hint(
                USER_TRAP_CONTEXT_TOP,
                PAGE_SIZE,
                MapPermission::R | MapPermission::W,
                MapAreaType::Trap,
            );
            let t_cx_ppn = memory_set.translate(VirtAddr::from(t_cx).floor()).unwrap();
            // 预分配页
            let area = memory_set
                .get_mut()
                .find_area_by_range(
                    VirtAddr::from(u_bottom).floor(),
                    VirtAddr::from(u_top).floor(),
                )
                .unwrap();
            for i in 1..=PRE_ALLOC_PAGES {
                let vpn = (area.vpn_range.end().0 - i).into();
                if memory_set.translate(vpn).is_none() {
                    area.map_one(&mut memory_set.get_mut().page_table, vpn);
                }
            }
            (u_top, t_cx, t_cx_ppn)
        };
        task_inner.trap_cx_ppn = trap_cx_ppn;
        task_inner.trap_cx_bottom = trap_cx_bottom;
        ustack_top
    }
    /// 仅为线程分配 trap context 区域（不分配栈空间，栈由用户提供）。
    /// 用于 CLONE_THREAD 且用户指定了栈地址的场景。
    fn alloc_trap_context_only(&self, task_inner: &mut TaskControlBlockInner) {
        let (trap_cx_bottom, trap_cx_ppn) = {
            let proc_inner = self.process.inner_lock();
            let memory_set = proc_inner.get_locked_memory_set_read();
            let (t_cx, _) = memory_set.insert_framed_area_with_hint(
                USER_TRAP_CONTEXT_TOP,
                PAGE_SIZE,
                MapPermission::R | MapPermission::W,
                MapAreaType::Trap,
            );
            let t_cx_ppn = memory_set.translate(VirtAddr::from(t_cx).floor()).unwrap();
            (t_cx, t_cx_ppn)
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
    /// VFORK: parent is suspended until child execs or exits.
    VforkBlocked,
}
pub type TaskRef = Arc<TaskControlBlock>;
pub type WeakTaskRef = Weak<TaskControlBlock>;
