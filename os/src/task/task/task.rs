//!Implementation of [`TaskControlBlock`]
use super::super::{
    aux::{Aux, AuxType},
    tid_to_task, TaskContext, TidHandle,
};
use super::super::process::Process;
use crate::{
    arch::context::TrapContext,
    arch::memory_layout::{
        PAGE_SIZE, PRE_ALLOC_PAGES, USER_HEAP_SIZE, USER_STACK_SIZE, USER_STACK_TOP,
        USER_TRAP_CONTEXT_TOP,
    },
    arch::page_table::PageTable,
    fs::{
        create_proc_dir_and_file, open, OpenFlags, DEFAULT_DIR_MODE,
        DEFAULT_FILE_MODE,FdTable,FSInfo
    },
    mm::{
        get_data, put_data, translated_refmut, MapAreaType, MapPermission, MemorySet,
        MemorySetInner, PhysPageNum, VirtAddr,
    },
    signal::{SigSet, SigTable},
    syscall::CloneFlags,
    task::kernel_stack::KernelStackOnHeap,
    timer::{TimeData, TimeVal, Timer},
    utils::{get_abs_path, is_abs_path, SysErrNo},
};
use alloc::{
    format,
    string::String,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::mem::size_of;
use core::{sync::atomic::{AtomicBool,Ordering}, task::Poll};
use futures_util::task::AtomicWaker;
use log::debug;
use spin::{rwlock::RwLock, Mutex, MutexGuard};

#[derive(Clone, Copy, Debug)]
pub struct RobustList {
    pub head: usize,
    pub len: usize,
}

impl RobustList {
    // from strace
    pub const HEAD_SIZE: usize = 24;
    pub fn default() -> Self {
        RobustList { head: 0, len: 24 }
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
    tcb: Weak<TaskControlBlock>, // 方便回到process去获取文件描述符表等公共资源
    trap_cx_ppn: PhysPageNum,    // TrapContext缓冲区物理页
    pub trap_cx_bottom: usize,   // TrapContext缓冲区虚拟地址基地址

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

    /// 在clone_user_res,
    fn alloc_user_res(&mut self) {
        let tcb_arc = self.tcb.upgrade().unwrap();
        let process = tcb_arc.process.inner_lock();
        let memory_set = process.get_locked_memory_set_read();

        let (ustack_bottom, ustack_top) = memory_set.lazy_insert_framed_area_with_hint(
            USER_STACK_TOP,
            USER_STACK_SIZE,
            MapPermission::R | MapPermission::W | MapPermission::U,
            MapAreaType::Stack,
        );
        let (trap_cx_bottom, _) = memory_set.insert_framed_area_with_hint(
            USER_TRAP_CONTEXT_TOP,
            PAGE_SIZE,
            MapPermission::R | MapPermission::W,
            MapAreaType::Trap,
        );
        let trap_cx_ppn = memory_set
            .translate(VirtAddr::from(trap_cx_bottom).floor())
            .unwrap();
        self.user_stack_top = ustack_top;
        self.trap_cx_ppn = trap_cx_ppn;
        self.trap_cx_bottom = trap_cx_bottom;

        //预先为栈顶分配几页，用于环境变量等初始数据

        // 在self.memory_set中找到第一个.range()==user_stack_range()的MapArea对象的可变引用
        // TrustOS中，这一步是在本函数中执行的
        // HXC在对mm模块进行重构时，将其移动到MemorySetInner中

        let area = memory_set
            .get_mut()
            .find_area_by_range(
                VirtAddr::from(ustack_bottom).floor(),
                VirtAddr::from(ustack_top).floor(),
            )
            .unwrap();

        for i in 1..=PRE_ALLOC_PAGES {
            let vpn = (area.vpn_range.end().0 - i).into();
            if memory_set.translate(vpn).is_none() {
                area.map_one(&mut memory_set.get_mut().page_table, vpn);
            }
        }
    }

    pub fn get_abs_path(
        &self,
        tcb: &TaskControlBlock,
        dirfd: isize,
        path: &str,
    ) -> Result<String, SysErrNo> {
        if is_abs_path(path) {
            Ok(get_abs_path("/", path))
        } else if dirfd != -100 {
            // AT_FDCWD=-100
            let dirfd = dirfd as usize;
            if let Some(file) = tcb.get_fd_table().try_get(dirfd) {
                let base_path = file.file()?.inode.path();
                // drop(proc_inner);
                if path.is_empty() {
                    Ok(base_path)
                } else {
                    Ok(get_abs_path(&base_path, path))
                }
            } else {
                Err(SysErrNo::EINVAL)
            }
        } else {
            Ok(get_abs_path(&tcb.get_fs_info().get_cwd(), path))
        }
    }
}

impl TaskControlBlock {
    pub fn inner_lock(&self) -> MutexGuard<'_,TaskControlBlockInner> {
        self.inner.try_lock().expect("fail to get task inner")
    }
    pub fn tid(&self) -> usize {
        self.tid.tid
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
        // alloc a pid and a kernel stack in kernel space
        let tid_handle = TidHandle::new();
        let kernel_stack = KernelStackOnHeap::new();
        let kernel_stack_top = kernel_stack.top();
        debug!("TCB::new kstack top = {:#x}", kernel_stack_top);
        let memory_set = Arc::new(RwLock::new(MemorySet::new(memory_set)));
        let sig_table = Arc::new(Mutex::new(SigTable::new()));
        let process = Process::new(
            memory_set.clone(),
            sig_table.clone(),
            Arc::new(FdTable::new_with_stdio()),
            1,
            None
        );
        let task = Self {
            tid: tid_handle,
            kernel_stack,
            process: process.clone(),
            interrupted: AtomicBool::new(false),
            interrupt_waker: AtomicWaker::new(),
            inner: Mutex::new(TaskControlBlockInner {
                tcb: Weak::new(),
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
        task_inner.tcb = Arc::downgrade(&arc_task);
        task_inner.alloc_user_res();
        // prepare TrapContext in user space
        let trap_cx = task_inner.trap_cx();
        *trap_cx =
            TrapContext::app_init_context(entry_point, task_inner.user_stack_top, kernel_stack_top);
        drop(task_inner);
        arc_task
    }
    /// exec的主逻辑
    pub fn exec(&self, elf_data: &[u8], argv: &Vec<String>, env: &mut Vec<String>) {
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

        // TODO: 此处需要clear_tid吗？

        self.process
            .change_memory_set_and_sigtable(memory_set, SigTable::new());

        // 重新分配用户资源
        task_inner.alloc_user_res();
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
        for i in 0..envp.len() {
            put_data(
                token,
                (user_sp + i * size_of::<usize>()) as *mut usize,
                envp[i],
            );
        }

        // println!("arg pointers:");
        user_sp -= argvp.len() * size_of::<usize>();
        let argv_base = user_sp;
        //将参数指针数组放入栈中
        for i in 0..argvp.len() {
            put_data(
                token,
                (user_sp + i * size_of::<usize>()) as *mut usize,
                argvp[i],
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
        // 获取/创建 PCB (ProcessControlBlock)
        let new_process = if flags.contains(CloneFlags::CLONE_THREAD) {
            // 情况 A: 创建新线程，共享当前进程
            Arc::clone(&self.process)
        } else {
            // 情况 B: fork，创建新进程
            // 注意：这里需要分配一个新的 PID，假设由全局分配器提供
            let new_pid = alloc_pid(); 
            self.process.do_proc_clone(new_pid)
        };

        // 为新线程分配内核栈和新 TID
        let new_tid = alloc_tid();
        let kernel_stack = KernelStack::new()?; 

        // 复制并修改 TrapContext
        // 我们需要获取当前线程在内核态保存的用户态寄存器快照
        let mut child_trap_cx = self.inner_lock().get_trap_cx().clone();
        
        // 子进程/子线程 fork 返回值为 0
        child_trap_cx.x[10] = 0; // x10 是 RISC-V 的 a0

        // 如果用户指定了新的栈（pthread_create），则更新 sp
        if stack != 0 {
            child_trap_cx.x[2] = stack; // x2 是 RISC-V 的 sp
        }

        // 如果设置了 CLONE_SETTLS，更新线程指针
        if flags.contains(CloneFlags::CLONE_SETTLS) {
            child_trap_cx.x[4] = tls; // x4 是 RISC-V 的 tp
        }

        // 4. 创建新的 TaskControlBlock (TCB)
        let new_task = Arc::new(TaskControlBlock {
            tid: new_tid,
            process: Arc::clone(&new_process),
            kernel_stack,
            inner: Mutex::new(TaskInner {
                trap_cx_ppn: ..., // 指向新分配的 trap_cx
                task_status: TaskStatus::Ready,
                // ... 其他初始化
            }),
        });

        // 5. 将新任务放入进程的任务列表中
        new_process.add_task(Arc::clone(&new_task));

        // 6. 处理 TID 写入用户空间 (根据 flags)
        if flags.contains(CloneFlags::CLONE_PARENT_SETTID) {
            // 安全地写入 parent_tid
            unsafe { *parent_tid = new_tid as u32; }
        }
        if flags.contains(CloneFlags::CLONE_CHILD_SETTID) {
            // 注意：这通常需要等到切换到子进程空间后再写，或者在创建时通过地址空间映射写入
        }

        Ok(new_task)
    }
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
        return ret;
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
    pub fn get_fs_info(&self)->Arc<FSInfo>{
        self.process.inner_lock().fs_info.clone()
    }
}

#[derive(Copy, Clone, PartialEq)]
pub enum TaskStatus {
    Ready,
    Running,
    Zombie,
    Blocked,
    Stopped,
}
pub type TaskRef = Arc<TaskControlBlock>;
pub type WeakTaskRef = Weak<TaskControlBlock>;
