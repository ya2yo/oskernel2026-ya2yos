//!Implementation of [`TaskControlBlock`]
use super::super::{
    aux::{Aux, AuxType},
    tid_to_task, TaskContext, TidHandle,
};
use super::process::Process;
use crate::{
    arch::context::TrapContext,
    arch::memory_layout::{
        PAGE_SIZE, PRE_ALLOC_PAGES, USER_HEAP_SIZE, USER_STACK_SIZE, USER_STACK_TOP,
        USER_TRAP_CONTEXT_TOP,
    },
    arch::page_table::PageTable,
    fs::{
        create_proc_dir_and_file, open, FdTable, FsInfo, OpenFlags, DEFAULT_DIR_MODE,
        DEFAULT_FILE_MODE,
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
use core::{sync::atomic::AtomicBool, task::Poll};
use core::mem::size_of;
use log::debug;
use spin::{rwlock::RwLock, Mutex, MutexGuard};
use futures_util::task::AtomicWaker;

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

    inner: Mutex<TaskControlBlockInner>,
}

impl Drop for TaskControlBlock {
    fn drop(&mut self) {
        debug!("TCB {} dropped", self.tid());
    }
}

pub struct TaskControlBlockInner {
    tcb: Weak<TaskControlBlock>, // 我想要这么干，但是这是非法的
    trap_cx_ppn: PhysPageNum,    // TrapContext缓冲区物理页
    pub trap_cx_bottom: usize,   // TrapContext缓冲区虚拟地址基地址

    pub user_stack_top: usize, // exclusive
    pub task_cx: TaskContext,
    pub task_status: TaskStatus,
    // pub fd_table: Arc<FdTable>,
    pub fs_info: Arc<Mutex<FsInfo>>,
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
        let memory_set = process.get_locked_memory_set();

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

    pub fn get_abs_path(&self, tcb: &TaskControlBlock,dirfd: isize, path: &str) -> Result<String, SysErrNo> {
        if is_abs_path(path) {
            Ok(get_abs_path("/", path))
        } else if dirfd != -100 {
            // AT_FDCWD=-100
            let dirfd = dirfd as usize;
            if let Some(file) = tcb.get_fd_table().try_get(dirfd) {
                let base_path = file.file()?.inode.path();
                drop(proc_inner);
                if path.is_empty() {
                    Ok(base_path)
                } else {
                    Ok(get_abs_path(&base_path, path))
                }
            } else {
                Err(SysErrNo::EINVAL)
            }
        } else {
            Ok(get_abs_path(self.fs_info.lock().cwd(), path))
        }
    }
}

impl TaskControlBlock {
    pub fn inner_lock(&self) -> MutexGuard<TaskControlBlockInner> {
        self.inner.try_lock().expect("fail to get task inner")
    }
    pub fn get_process(&self) -> Arc<Process> {
        self.process.clone()
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
        let process = Process::new(memory_set.clone(), sig_table.clone(), Arc::new(FdTable::new_with_stdio()), 1, None);
        let task = Self {
            tid: tid_handle,
            kernel_stack,
            process: process.clone(),
            interrupted:AtomicBool::new(false),
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
                fs_info: Arc::new(Mutex::new(FsInfo::new_for_initproc())),
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
        // 锁住父对象以获取必要资源
        let parent_inner = self.inner.lock();
        let parent_proc_inner = self.process.inner_lock();

        let tid_handle = TidHandle::new();
        let kernel_stack = KernelStackOnHeap::new();
        let kernel_stack_top = kernel_stack.top();

        // 处理地址空间
        let memory_set = if flags.contains(CloneFlags::CLONE_VM) {
            // 线程：共享内存
            Arc::clone(&parent_proc_inner.memory_set)
        } else {
            // 进程：拷贝内存映射（Copy-on-Write 逻辑通常在这里触发）
            Arc::new(RwLock::new(MemorySet::new(
                MemorySetInner::from_existed_user(&*parent_proc_inner.get_locked_memory_set()),
            )))
        };

        // 处理文件系统信息
        let fs_info = if flags.contains(CloneFlags::CLONE_FS) {
            Arc::clone(&parent_inner.fs_info)
        } else {
            Arc::new(Mutex::new(FsInfo::from_another(&parent_inner.fs_info.lock())))
        };

        // 处理打开文件表
        // 注意：现在 fd_table 是从 parent_proc_inner 获取的
        let fd_table = if flags.contains(CloneFlags::CLONE_FILES) {
            Arc::clone(&parent_proc_inner.fd_table)
        } else {
            Arc::new(FdTable::from_another(&parent_proc_inner.fd_table))
        };

        // 处理信号处理程序表
        let sig_table = if flags.contains(CloneFlags::CLONE_SIGHAND) {
            Arc::clone(&parent_proc_inner.sig_table)
        } else {
            Arc::new(Mutex::new(SigTable::from_another(&*parent_proc_inner.get_locked_sigtable())))
        };

        // 确定子进程对象
        let (pid, ppid, timer, sig_mask);
        let process: Arc<Process>;

        if flags.contains(CloneFlags::CLONE_THREAD) {
            // 创建线程：属于同一个进程
            pid = self.pid(); // 线程组 ID (TGID) 相同
            ppid = self.ppid();
            timer = Arc::clone(&parent_inner.timer);
            sig_mask = SigSet::empty();
            process = Arc::clone(&self.process);
        } else {
            // 创建子进程 (Fork)
            pid = tid_handle.tid;
            ppid = self.pid();
            timer = Arc::new(Timer::new());
            sig_mask = parent_inner.sig_mask.clone();
            
            // 创建新的进程结构体，传入刚刚决定好的资源
            // 注意：这里需要给 Process::new 增加 fd_table 参数，或者单独设置
            process = Process::new(
                memory_set.clone(),
                sig_table.clone(),
                fd_table.clone(),
                pid,
                Some(Arc::clone(&self.process)),
            );
        }

        // 修改父线程中指定的内存地址
        if flags.contains(CloneFlags::CLONE_PARENT_SETTID) {
            let token = parent_proc_inner.get_locked_memory_set().token();
            *translated_refmut(token, parent_tid) = tid_handle.tid as u32;
        }

        // 创建 TCB
        let clear_child_tid = if flags.contains(CloneFlags::CLONE_CHILD_CLEARTID) {
            child_tid as usize
        } else {
            0
        };

        let child = Arc::new(TaskControlBlock {
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
                fs_info,
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

        // 设置 Weak 引用
        let mut child_inner = child.inner_lock();
        child_inner.tcb = Arc::downgrade(&child);
        
        // 将任务加入进程的任务列表
        process.meta_lock().tasks.push(Arc::downgrade(&child));

        // 处理用户态上下文
        child_inner.alloc_user_res();
        
        if flags.contains(CloneFlags::CLONE_THREAD) {
            // 线程逻辑：拷贝父线程的寄存器状态
            *child_inner.trap_cx() = *parent_inner.trap_cx();
        } else {
            // 进程逻辑：从父进程地址空间拷贝数据
            let parent_mm = parent_proc_inner.get_locked_memory_set();
            let child_mm = process.inner_lock().get_locked_memory_set();

            // 拷贝栈和 Trap 上下文所在的内存区域内容
            child_mm.lazy_clone_area(
                VirtAddr::from(child_inner.user_stack_top - USER_STACK_SIZE).floor(),
                parent_mm.get_ref(),
            );
            child_mm.clone_area(
                VirtAddr::from(child_inner.trap_cx_bottom).floor(),
                parent_mm.get_ref(),
            );
            // 子进程 fork 返回 0
            child_inner.trap_cx().set_a0(0);
        }

        // 处理特殊的线程启动参数 
        let trap_cx = child_inner.trap_cx();
        trap_cx.kernel_stack = kernel_stack_top;

        if stack != 0 {
            // 如果指定了新的用户栈（pthread_create）
            // 移除 alloc_user_res 自动分配的栈映射，改用指定的地址
            // ... (保持你原来的 remove_area 逻辑)
            trap_cx.set_sp(stack);
            
            // 设置线程入口
            let token = parent_proc_inner.get_locked_memory_set().token();
            let entry_point = get_data(token, stack as *const usize);
            let arg = get_data(token, (stack + 8) as *const usize);
            trap_cx.set_sepc(entry_point);
            trap_cx.set_a0(arg);
        }

        if flags.contains(CloneFlags::CLONE_SETTLS) {
            trap_cx.set_tp(tls);
        }

        if flags.contains(CloneFlags::CLONE_CHILD_SETTID) {
            let child_token = process.inner_lock().get_locked_memory_set().token();
            *translated_refmut(child_token, child_tid) = child.tid() as u32;
        }

        // 结尾
        drop(child_inner);
        drop(parent_proc_inner);
        drop(parent_inner);

        tid_to_task::insert(child.tid(), &child);
        
        Ok(child)
    }

    ///修改数据段大小，懒分配
    pub fn growproc(&self, grow_size: isize) -> usize {
        let mut inner = self.inner_lock();
        let process = self.process.inner_lock();
        let memory_set = process.get_locked_memory_set();

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
    pub fn set_status(&self,status:TaskStatus){
        let mut task_inner=self.inner_lock();
        task_inner.task_status=status;
        drop(task_inner);
    }
    pub fn poll_interrupt(&self,cx: core::task::Context) -> Poll<()>{
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
}

#[derive(Copy, Clone, PartialEq)]
pub enum TaskStatus {
    Ready,
    Running,
    Zombie,
    Blocked,
    Stopped,
}
pub type TaskRef=Arc<TaskControlBlock>;
pub type WeakTaskRef = Weak<TaskControlBlock>;
