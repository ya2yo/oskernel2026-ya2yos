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
        copy_to_user, get_data, put_data, translate::strong_translated_refmut, translated_refmut,
        MapAreaType, MapPermission, MemorySet, MemorySetInner, PhysPageNum, VirtAddr,
    },
    signal::{SigSet, SigTable},
    syscall::CloneFlags,
    task::{
        futex::futex_wake_up,
        kernel_stack::KernelStackOnHeap,
        tid,
    },
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
use log::debug;
use spin::{rwlock::RwLock, Mutex, MutexGuard};

#[derive(Clone, Copy, Debug)]
pub struct RobustList {
    pub head: usize,
    pub len: usize,
}

pub const HEAD_SIZE: usize = 24;
impl Default for RobustList {
    fn default() -> Self {
        RobustList {
            head: 0,
            len: HEAD_SIZE,
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

    pub user_stack_top: usize, // exclusive
    pub task_cx: TaskContext,
    pub task_status: TaskStatus,
    // pub fd_table: Arc<FdTable>,
    // pub fs_info: Arc<Mutex<FsInfo>>,
    pub time_data: TimeData,

    // 用于TaskControlBlockInner::growproc
    user_heappoint: usize,  //堆顶指针,小于等于user_heaptop
    user_heapbottom: usize, //堆底指针

    /// 当线程退出时，要将这个指针指向的int置为0，并唤醒等待它的futex
    pub clear_child_tid: usize,
    /// 被屏蔽的信号
    pub sig_mask: SigSet,
    /// 待处理信号集合
    pub sig_pending: SigSet,
    pub timer: Arc<Timer>,
    pub robust_list: RobustList,
    pub user_id: usize,

    // 用于futex
    pub futex_pa: usize,  // 当前正在等待的pa
    pub futex_key: usize, // 当前正在等待的Wait的版本号
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
    pub fn pid(&self) -> usize {
        self.process.pid
    }
    pub fn ppid(&self) -> usize {
        self.process.ppid()
    }
    /// 只有initproc会调用
    pub fn new(elf_data: &[u8]) -> Arc<Self> {
        // memory_set with elf program headers/trampoline/trap context/user stack
        let (memory_set, user_heapbottom, entry_point, _) = MemorySetInner::from_elf(elf_data);
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
                user_stack_top: 0,
                task_cx: TaskContext::goto_trap_return(kernel_stack_top),
                task_status: TaskStatus::Ready,
                // fd_table: Arc::new(FdTable::new_with_stdio()),
                // fs_info: Arc::new(Mutex::new(FsInfo::new_for_initproc())),
                time_data: TimeData::new(),
                user_heappoint: user_heapbottom,
                user_heapbottom,
                clear_child_tid: 0,
                sig_mask: SigSet::empty(),
                sig_pending: SigSet::empty(),
                timer: Arc::new(Timer::new()),
                robust_list: RobustList::default(),
                user_id: 0,
                futex_pa: 0,
                futex_key: 0,
            }),
        };
        let arc_task = Arc::new(task);
        process.meta_lock().tasks.push(Arc::downgrade(&arc_task));
        let mut task_inner = arc_task.inner_lock();
        arc_task.alloc_user_res(&mut task_inner);
        // prepare TrapContext in user space
        let trap_cx = task_inner.trap_cx();
        *trap_cx =
            TrapContext::app_init_context(entry_point, task_inner.user_stack_top, kernel_stack_top);
        drop(task_inner);
        arc_task
    }
    /// exec的主逻辑
    pub fn exec(&self, elf_data: &[u8], argv: &[String], env: &mut [String]) {
        let mut task_inner = self.inner_lock();
        //用户栈高地址到低地址：环境变量字符串/参数字符串/aux辅助向量/环境变量地址数组/参数地址数组/参数数量
        // memory_set with elf program headers/trampoline/trap context/user stack
        debug!("exec: goto from_elf");
        let (memory_set, user_hp, entry_point, mut auxv) = MemorySetInner::from_elf(elf_data);
        debug!("exec: return from from_elf");
        let token = memory_set.token();
        let memory_set = MemorySet::new(memory_set);

        task_inner.time_data.clear();

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
            if let Some(pa) = old_memory_set.translate_va(VirtAddr::from(task_inner.clear_child_tid))
            {
                futex_wake_up(pa.0, 1);
            }
            drop(old_memory_set);
            task_inner.clear_child_tid = 0;
        }

        self.process
            .change_memory_set_and_sigtable(memory_set, SigTable::new());

        // 重新分配用户资源
        self.alloc_user_res(&mut task_inner);
        {
            self.get_fd_table().close_on_exec();
        }
        task_inner.sig_mask = SigSet::empty();
        task_inner.sig_pending = SigSet::empty();

        let mut user_sp = task_inner.user_stack_top;

        // println!("user_sp:{:#X}  argv:{:?}", user_sp, argv);

        //环境变量内容入栈
        let mut envp = Vec::new();
        for env in env.iter() {
            user_sp -= env.len() + 1;
            envp.push(user_sp);
            // println!("{:#X}:{}", user_sp, env);
            for (j, c) in env.as_bytes().iter().enumerate() {
                *translated_refmut(token, (user_sp + j) as *mut u8) = *c;
            }
            *translated_refmut(token, (user_sp + env.len()) as *mut u8) = 0;
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
                *translated_refmut(token, (user_sp + j) as *mut u8) = *c;
            }
            // 添加字符串末尾的 null 字符
            *translated_refmut(token, (user_sp + arg.len()) as *mut u8) = 0;
        }
        user_sp -= user_sp % size_of::<usize>(); //以8字节对齐
        argvp.push(0);

        //需要随便放16个字节，不知道干嘛用的。
        user_sp -= 16;
        auxv.push(Aux::new(AuxType::RANDOM, user_sp));
        for i in 0..0xf {
            *translated_refmut(token, (user_sp + i) as *mut u8) = i as u8;
        }
        user_sp -= user_sp % 16;

        // println!("aux:");
        //将auxv放入栈中
        auxv.push(Aux::new(AuxType::EXECFN, argvp[0]));
        auxv.push(Aux::new(AuxType::NULL, 0));
        for aux in auxv.iter().rev() {
            // println!("{:?}", aux);
            user_sp -= size_of::<Aux>();
            *translated_refmut(token, user_sp as *mut usize) = aux.aux_type as usize;
            *translated_refmut(token, (user_sp + size_of::<usize>()) as *mut usize) = aux.value;
        }

        //将环境变量指针数组放入栈中
        // println!("env pointers:");
        user_sp -= envp.len() * size_of::<usize>();
        let envp_base = user_sp;
        for (i, data) in envp.iter().enumerate() {
            put_data(
                token,
                (user_sp + i * size_of::<usize>()) as *mut usize,
                *data,
            );
        }

        // println!("arg pointers:");
        user_sp -= argvp.len() * size_of::<usize>();
        let argv_base = user_sp;
        //将参数指针数组放入栈中
        for (i, &data) in argvp.iter().enumerate() {
            put_data(
                token,
                (user_sp + i * size_of::<usize>()) as *mut usize,
                data,
            );
        }

        //将argc放入栈中
        user_sp -= size_of::<usize>();
        *translated_refmut(token, user_sp as *mut usize) = argv.len();

        //以8字节对齐
        user_sp -= user_sp % size_of::<usize>();
        //println!("user_sp:{:#X}", user_sp);

        // 将设置了O_CLOEXEC位的文件描述符关闭
        self.get_fd_table().close_on_exec();

        let mut trap_cx =
            TrapContext::app_init_context(entry_point, user_sp, self.kernel_stack.top());
        trap_cx.set_a0(argv.len());
        trap_cx.set_a1(argv_base);
        trap_cx.set_a2(envp_base);
        *task_inner.trap_cx() = trap_cx;
        task_inner.user_heappoint = user_hp;
        task_inner.user_heapbottom = user_hp;
    }
    /// 复制进程，注意这里需要实现 fork 的主要逻辑
    pub fn clone_process(
        self: &Arc<TaskControlBlock>,
        flags: CloneFlags,
        stack: usize,
        parent_tid: *mut u32,
        tls: usize,
        child_tid: *mut u32,
    ) -> Result<Arc<TaskControlBlock>, SysErrNo> {
        let parent_inner = self.inner.lock();

        let tid_handle = TidHandle::alloc().unwrap();
        let kernel_stack = KernelStackOnHeap::new();
        let kernel_stack_top = kernel_stack.top();
        debug!("TCB::new kstack top = {:#x}", kernel_stack_top);
        // 检查是否共享虚拟内存
        let memory_set = if flags.contains(CloneFlags::CLONE_VM) {
            self.process.inner.try_lock().unwrap().memory_set.clone()
        } else {
            Arc::new(RwLock::new(MemorySet::new(
                MemorySetInner::from_existed_user(
                    &self.process.inner_lock().get_locked_memory_set_read(),
                ),
            )))
        };
        // 检查是否共享文件系统信息
        let fs_info = if flags.contains(CloneFlags::CLONE_FS) {
            Arc::clone(&self.process.inner_lock().fs_info)
        } else {
            Arc::new(FSInfo::from_another(&self.process.inner_lock().fs_info))
        };
        // 检查是否共享打开文件表
        let fd_table = if flags.contains(CloneFlags::CLONE_FILES) {
            Arc::clone(&self.process.inner_lock().fd_table)
        } else {
            Arc::new(FdTable::from_another(&self.process.inner_lock().fd_table))
        };
        // 检查是否共享信号处理程序表
        let sig_table = if flags.contains(CloneFlags::CLONE_SIGHAND) {
            self.process.inner.try_lock().unwrap().sig_table.clone()
        } else {
            Arc::new(Mutex::new(SigTable::from_another(
                &self.process.inner_lock().get_locked_sigtable(),
            )))
        };
        // 检查是否需要设置 parent_tid
        if flags.contains(CloneFlags::CLONE_PARENT_SETTID) {
            *translated_refmut(
                self.process
                    .inner_lock()
                    .get_locked_memory_set_read()
                    .token(),
                parent_tid,
            ) = tid_handle.0 as u32;
        }
        let clear_child_tid = if flags.contains(CloneFlags::CLONE_CHILD_CLEARTID) {
            child_tid as usize
        } else {
            0
        };
        let (pid, mut ppid, timer, sig_mask);
        let process: Arc<Process>;

        // 检查是否创建线程
        if flags.contains(CloneFlags::CLONE_THREAD) {
            pid = self.pid();
            ppid = self.ppid();
            timer = Arc::clone(&parent_inner.timer);
            sig_mask = SigSet::empty();
            process = self.process.clone();
        } else {
            pid = tid_handle.0;
            let parent_pid = if flags.contains(CloneFlags::CLONE_PARENT) {
                self.ppid()
            } else {
                self.pid()
            };
            ppid = parent_pid;
            timer = Arc::new(Timer::new());
            sig_mask = parent_inner.sig_mask;
            process = Process::new(
                memory_set.clone(),
                sig_table.clone(),
                fd_table,
                fs_info,
                pid,
                parent_pid,
            );
        }

        let child = Arc::new(TaskControlBlock {
            tid: tid_handle,
            kernel_stack,
            process,
            interrupted: AtomicBool::new(false),
            interrupt_waker: AtomicWaker::new(),
            inner: Mutex::new(TaskControlBlockInner {
                trap_cx_ppn: 0.into(),
                trap_cx_bottom: 0,
                user_stack_top: 0,
                task_cx: TaskContext::goto_trap_return(kernel_stack_top),
                task_status: TaskStatus::Ready,
                // fd_table,
                // fs_info,
                time_data: TimeData::new(),
                user_heappoint: parent_inner.user_heappoint,
                user_heapbottom: parent_inner.user_heapbottom,
                clear_child_tid,
                sig_mask,
                sig_pending: SigSet::empty(),
                timer,
                robust_list: RobustList::default(),
                user_id: parent_inner.user_id,
                futex_pa: 0,
                futex_key: 0,
            }),
        });

        let mut child_inner = child.inner_lock();
        child.process.meta_lock().tasks.push(Arc::downgrade(&child));

        if flags.contains(CloneFlags::CLONE_THREAD) {
            // 线程
            self.alloc_user_res(&mut child_inner);
            *child_inner.trap_cx() = *parent_inner.trap_cx();
        } else {
            // fork
            let process = &self.process.inner_lock();
            let another = &*process.get_locked_memory_set_read();
            child.alloc_user_res(&mut child_inner);
            let child_proc = child.process.inner_lock();

            let child_mm = child_proc.get_locked_memory_set_read();
            child_mm.lazy_clone_area(
                VirtAddr::from(child_inner.user_stack_top - USER_STACK_SIZE).floor(),
                another.get_ref(),
            );
            child_mm.clone_area(
                VirtAddr::from(child_inner.trap_cx_bottom).floor(),
                another.get_ref(),
            );

            // for child process, fork returns 0
            child_inner.trap_cx().set_a0(0);
        }
        let trap_cx = child_inner.trap_cx();
        trap_cx.kernel_stack = kernel_stack_top;

        if stack != 0 {
            // 移除分配的stack
            let ustack = child_inner.user_stack_top;
            child
                .process
                .inner_lock()
                .get_locked_memory_set_read()
                .remove_area_with_start_vpn(VirtAddr::from(ustack - USER_STACK_SIZE).floor());
            child_inner.user_stack_top = 0;
            // 设置运行的起始地址和参数以及stack
            let token = self
                .process
                .inner_lock()
                .get_locked_memory_set_read()
                .token();
            let entry_point = get_data(token, stack as *const usize);
            let arg = get_data(token, (stack + 8) as *const usize);
            // sepc/entry
            // debug!("[new thread] entry_point:{:#x}", entry_point);
            trap_cx.set_sepc(entry_point);
            //a0
            trap_cx.set_a0(arg);
            //sp
            trap_cx.set_sp(stack);
        }

        if flags.contains(CloneFlags::CLONE_SETTLS) {
            // tp
            trap_cx.set_tp(tls);
        }
        // CLONE_CHILD_SETTID
        if flags.contains(CloneFlags::CLONE_CHILD_SETTID) {
            let child_token = child
                .process
                .inner_lock()
                .get_locked_memory_set_read()
                .token();
            *translated_refmut(child_token, child_tid) = child.tid() as u32;
        }

        if flags.contains(CloneFlags::SIGCHLD) {
            create_proc_dir_and_file(pid, ppid);
        }

        drop(child_inner);
        drop(parent_inner);
        tid_to_task::insert(child.tid(), &child);
        // if !flags.contains(CloneFlags::CLONE_THREAD) {
        //     insert_into_process_group(child.ppid(), &child);
        // }
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
    /// 获取当前任务的 FD 表,自动处理锁
    pub fn get_fd_table(&self) -> Arc<FdTable> {
        self.process.inner_lock().fd_table.clone()
    }
    /// 获取当前进程相关的文件使用信息
    pub fn get_fs_info(&self) -> Arc<FSInfo> {
        self.process.inner_lock().fs_info.clone()
    }
    /// 获取线程所在的进程
    pub fn get_process(&self) -> Arc<Process> {
        self.process.clone()
    }
    /// 在clone_user_res,
    fn alloc_user_res(&self, task_inner: &mut TaskControlBlockInner) {
        let (_, ustack_top, trap_cx_bottom, trap_cx_ppn) = {
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
            (u_bottom, u_top, t_cx, t_cx_ppn)
        };
        // 锁 TCB 并回写结果
        task_inner.user_stack_top = ustack_top;
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
    Stopped,
}
pub type TaskRef = Arc<TaskControlBlock>;
pub type WeakTaskRef = Weak<TaskControlBlock>;
