use alloc::{
    collections::btree_map::BTreeMap,
    format,
    sync::{Arc, Weak},
    vec::Vec,
};
use log::{debug, error, warn};
use spin::{
    rwlock::{RwLock, RwLockWriteGuard},
    Lazy, Mutex, MutexGuard, RwLockReadGuard,
};

use crate::{
    fs::FdTable, mm::{MemorySet, MemorySetInner}, signal::SigTable, task::TaskControlBlock
};

/// 进程/线程组 类
/// 它的Arc是TCB
pub struct Process {
    pub inner: Mutex<ProcessInner>,
    pub pid: usize,
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
}

impl Process {
    pub fn new(
        memory_set: Arc<RwLock<MemorySet>>,
        sig_table: Arc<Mutex<SigTable>>,
        fd_table: Arc<FdTable>,
        pid: usize,
        parent: Option<Arc<Process>>,
    ) -> Arc<Self> {
        let ret = Arc::new(Self {
            inner: Mutex::new(ProcessInner {
                memory_set,
                sig_table,
                fd_table,
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
        debug!("inserting process {}", pid);
        let oldval = PID_2_PROCESS_ARC
            .try_lock()
            .unwrap()
            .insert(pid, ret.clone());
        if let Some(old_proc) = oldval {
            debug!("expected replacement? {}", old_proc.pid);
        }
        ret
    }

    pub fn inner_lock(&self) -> MutexGuard<ProcessInner> {
        self.inner
            .try_lock()
            .expect(&format!("fail to get proc lock({})", self.pid))
    }

    pub fn meta_lock(&self) -> MutexGuard<ProcessMeta> {
        self.meta
            .try_lock()
            .expect(&format!("fail to get proc.meta lock({})", self.pid))
    }

    pub fn ppid(&self) -> usize {
        if let Some(parent) = &self.parent {
            parent.pid
        } else {
            0
        }
    }

    pub fn change_memory_set_and_sigtable(
        &self,
        new_memory_set: MemorySet,
        new_sigtable: SigTable,
    ) {
        let mut inner_lock = self.inner.try_lock().expect("lock fail");
        inner_lock.memory_set = Arc::new(RwLock::new(new_memory_set));
        inner_lock.sig_table = Arc::new(Mutex::new(new_sigtable));
    }

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
                    warn!("{}", i.upgrade().unwrap().pid);
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
}

impl Drop for Process {
    fn drop(&mut self) {
        // PID_2_PROCESS_ARC.try_lock().unwrap().remove(&self.pid);
        debug!("proc {} is dropped", self.pid);
    }
}

impl ProcessInner {
    pub fn get_locked_memory_set(&self) -> RwLockReadGuard<'_, MemorySet> {
        self.memory_set
            .try_read()
            .expect("You should not fail to get lock in a 1 HART system!")
    }

    pub fn get_locked_memory_set_write(&self) -> RwLockWriteGuard<'_, MemorySet> {
        self.memory_set
            .try_write()
            .expect("You should not fail to get lock in a 1 HART system!")
    }

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
