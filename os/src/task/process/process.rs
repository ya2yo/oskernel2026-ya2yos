use alloc::{
    collections::btree_map::BTreeMap,
    format,
    sync::{Arc, Weak},
    vec::Vec,
};
use log::{debug, error, warn};
use spin::{
    Lazy, Mutex, MutexGuard, RwLockReadGuard, rwlock::{RwLock, RwLockWriteGuard}
};

use crate::{
    fs::{FSInfo, FdTable}, mm::{MemorySet, MemorySetInner}, signal::SigTable, syscall::CloneFlags, task::{TaskControlBlock, TidHandle}, utils::SyscallRet
};

/// 进程/线程组 类
/// 它的Arc是TCB
pub struct Process {
    pub inner: Mutex<ProcessInner>,
    pub pid: TidHandle,
    pub parent: Option<Arc<Process>>,
    pub meta: Mutex<ProcessMeta>,
}
// 我们需要向编译器保证Process含有这样的特性……这样真的好吗？
unsafe impl Send for Process {}
unsafe impl Sync for Process {}

/// 进程可变部分
pub struct ProcessInner {
    pub memory_set: Arc<RwLock<MemorySet>>,
    pub sig_table: Arc<Mutex<SigTable>>,
    /// 进程打开的文件描述符表
    pub fd_table:Arc<FdTable>,
    pub fs_info: Arc<FSInfo>,
}

impl Process {
    /// 为实现fork做准备
    /// 从父进程 fork 出一个子进程
    /// 
    /// 1. 分配pid 2.修改parent 3.初始化元数据() 4. 拷贝inner里面的数据
    pub fn do_proc_clone(self:&Arc<Self>,flags:CloneFlags,new_pid:TidHandle)->Arc<Self> {
        let parent_inner=self.inner_lock();
        // 拷贝地址空间
        // 根据 CLONE_VM 决定共享还是拷贝地址空间
        let new_memory_set = if flags.contains(CloneFlags::CLONE_VM) {
            Arc::clone(&parent_inner.memory_set)
        } else {
            // COW 拷贝逻辑
            let old_aspace = parent_inner.memory_set.read();
            let new_aspace_inner = MemorySetInner::from_another(old_aspace.get_ref());
            Arc::new(RwLock::new(MemorySet::new(new_aspace_inner)))
        };

        // 拷贝信号表
        let new_sig_table = if flags.contains(CloneFlags::CLONE_SIGHAND) {
            Arc::clone(&parent_inner.sig_table)
        } else {
            // 拷贝信号处理动作（Dispositions）
            Arc::new(Mutex::new(SigTable::from_another(&parent_inner.get_locked_sigtable())))
        };

        // 处理文件描述符表 
        let new_fd_table = if flags.contains(CloneFlags::CLONE_FILES) {
            Arc::clone(&parent_inner.fd_table)
        } else {
            // 拷贝一份当前的文件表镜像
            Arc::new(FdTable::from_another(&parent_inner.fd_table))
        };

        // 拷贝文件系统环境
        let new_fs_info = if flags.contains(CloneFlags::CLONE_FS) {
            Arc::clone(&parent_inner.fs_info)
        } else {
            Arc::new(FSInfo::from_another(&parent_inner.fs_info))
        };
        
        // 元数据的初始化中，只有fork的那个线程会存在，因此在那里完成
        let new_proc = Arc::new(Self {
            pid: new_pid,
            parent: Some(Arc::clone(self)),
            inner: Mutex::new(ProcessInner {
                memory_set: new_memory_set,
                sig_table: new_sig_table,
                fd_table: new_fd_table,
                fs_info: new_fs_info,
            }),
            meta: Mutex::new(ProcessMeta {
                tasks: Vec::new(),
                children: Vec::new(),
            }),
        });
        self.meta_lock().children.push(Arc::downgrade(&new_proc));
        PID_2_PROCESS_ARC.lock().insert(new_proc.pid.0, Arc::clone(&new_proc));
        new_proc
    }
    /// 退出时调用，进行托孤
    pub fn exit_and_reparent(&self) {
        let mut meta=self.meta_lock();
        let initproc=Self::get_process_arc_by_pid(1).expect("initproc not found!");
        for child_weak in &meta.children {
            if let Some(child)=child_weak.upgrade() {
                initproc.meta_lock().children.push(Arc::downgrade(&child));
            }
        }
        meta.tasks.clear();
    }
    /// 创建新进程
    pub fn new(
        memory_set: Arc<RwLock<MemorySet>>,
        sig_table: Arc<Mutex<SigTable>>,
        fd_table: Arc<FdTable>,
        pid: TidHandle,
        parent: Option<Arc<Process>>,
    ) -> Arc<Self> {
        let id=pid.0;
        let ret = Arc::new(Self {
            inner: Mutex::new(ProcessInner {
                memory_set,
                sig_table,
                fd_table,
                fs_info: Arc::new(FSInfo::new_initproc()),
            }),
            pid,
            parent: parent.clone(),
            meta: Mutex::new(ProcessMeta {
                tasks: Vec::new(),
                children: Vec::new(),
            }),
        });
        if let Some(parent_process) = &parent {
            parent_process
                .meta
                .lock()
                .children
                .push(Arc::downgrade(&ret));
        }
        debug!("inserting process {}", id);
        let oldval = PID_2_PROCESS_ARC
            .try_lock()
            .unwrap()
            .insert(id, ret.clone());
        if let Some(old_proc) = oldval {
            debug!("expected replacement? {}", old_proc.pid.0);
        }
        ret
    }
    /// 获取inner的锁
    pub fn inner_lock(&self) -> MutexGuard<'_,ProcessInner> {
        self.inner
            .try_lock()
            .expect(&format!("fail to get proc lock({})", self.pid.0))
    }
    /// 获取元数据的锁
    pub fn meta_lock(&self) -> MutexGuard<'_,ProcessMeta> {
        self.meta
            .try_lock()
            .expect(&format!("fail to get proc.meta lock({})", self.pid.0))
    }
    /// 获取父进程的pid
    pub fn ppid(&self) -> usize {
        if let Some(parent) = &self.parent {
            parent.pid.0
        } else {
            0
        }
    }
    /// 改变内存映射关系和信号表
    pub fn change_memory_set_and_sigtable(
        &self,
        new_memory_set: MemorySet,
        new_sigtable: SigTable,
    ) {
        let mut inner_lock = self.inner.try_lock().expect("lock fail");
        inner_lock.memory_set = Arc::new(RwLock::new(new_memory_set));
        inner_lock.sig_table = Arc::new(Mutex::new(new_sigtable));
    }
    /// 通过pid获取对应的进程
    pub fn get_process_arc_by_pid(pid: usize) -> Option<Arc<Process>> {
        let ret = PID_2_PROCESS_ARC
            .try_lock()
            .expect("fail to get pid2process mapper")
            .get(&pid)
            .map(|x| x.clone());
        ret
    }

    /// 如果一个线程调用了ExitGroup，或者最后一个线程Exit，那么这个就会为true
    pub fn basically_exited(&self) -> bool {
        self.inner_lock().sig_table.try_lock().unwrap().is_exited()
    }

    /// 是否已经退出
    /// 注意，如果该进程有多线程，当第一个线程调用ExitGroup后
    /// 还不会立刻导致本函数return true
    /// 等其他线程接收到信号，并exit（使得本进程不再持有有效的Weak引用）时
    /// 本函数才会return true
    pub fn all_tasks_exited(&self) -> bool {
        self.meta_lock().tasks.iter().all(|x| x.upgrade().is_none())
    }

    /// Process被Wait4时会调用这个
    pub fn remove_from_global_map(pid: usize) {
        let ret = PID_2_PROCESS_ARC
            .try_lock()
            .expect("fail to get pid2process mapper")
            .remove(&pid);
        if let Some(arc) = ret {
            if Arc::strong_count(&arc) == 1 {
                debug!("remove process[{}] succeed!", pid);
            } else {
                warn!("unexpected ref cnt");
                warn!("the proc's children:");
                for i in arc.meta_lock().children.iter() {
                    warn!("{}", i.upgrade().unwrap().pid.0);
                }
                warn!("the proc's tasks:");
                let tasks: Vec<usize> = arc
                    .meta_lock()
                    .tasks
                    .iter()
                    .filter_map(|x| x.upgrade())
                    .map(|x| x.tid())
                    .collect();
                warn!("{:?}", tasks);
                panic!(
                    "process[{}] removed but still refed! refcnt={}",
                    pid,
                    Arc::strong_count(&arc)
                );
            }
        } else {
            panic!("remove process[{}] fail! it does not exist!", pid);
        }
    }
    /// 向进程中添加一个线程
    pub fn add_task(&self, task:Arc<TaskControlBlock>){
        self.meta_lock().tasks.push(Arc::downgrade(&task));
    }
    /// 获取当前进程中还活着的线程数量
    pub fn alive_tasks_count(&self) -> usize {
        self.meta_lock().tasks.iter().filter(|t| t.upgrade().is_some()).count()
    }

    // 只会在sys_fcntl里面调用的一些辅助函数

    pub fn do_fcntl_setfd(&self,fd:usize, cloexec:bool)->SyscallRet {
        if cloexec {
            self.inner_lock().fd_table.set_cloexec(fd)
        }else{
            self.inner_lock().fd_table.unset_cloexec(fd)
        }
    }
    pub fn do_fcntl_setfl(&self, fd:usize, nonblock:bool)->SyscallRet {
        if nonblock {
            self.inner_lock().fd_table.set_nonblock(fd)
        }else {
            self.inner_lock().fd_table.unset_nonblock(fd)
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        // PID_2_PROCESS_ARC.try_lock().unwrap().remove(&self.pid);
        debug!("proc {} is dropped", self.pid.0);
    }
}

impl ProcessInner {
    /// 内存相关的读锁
    pub fn get_locked_memory_set_read(&self) -> RwLockReadGuard<'_, MemorySet> {
        self.memory_set
            .try_read()
            .expect("You should not fail to get lock in a 1 HART system!")
    }
    /// 内存相关的写锁
    pub fn get_locked_memory_set_write(&self) -> RwLockWriteGuard<'_, MemorySet> {
        self.memory_set
            .try_write()
            .expect("You should not fail to get lock in a 1 HART system!")
    }
    /// 信号表获取
    pub fn get_locked_sigtable(&self) -> MutexGuard<'_, SigTable> {
        self.sig_table
            .try_lock()
            .expect("You should not fail to get lock in a 1 HART system!")
    }
}

pub struct ProcessMeta {
    /// 该进程有哪些线程
    /// 由task负责维护
    /// task创建时，向其中push_back
    /// task exit时，从中删除
    /// 该成员暂时闲置
    pub tasks: Vec<Weak<TaskControlBlock>>,
    /// 该进程有哪些子进程，仅记录pid
    /// Process::new时，向父进程的children中插入
    /// sys_wait4时，删除
    pub children: Vec<Weak<Process>>,
}

static PID_2_PROCESS_ARC: Lazy<Mutex<BTreeMap<usize, Arc<Process>>>> =
    Lazy::new(|| Mutex::new(BTreeMap::new()));
