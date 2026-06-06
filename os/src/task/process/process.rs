use alloc::{
    collections::btree_map::BTreeMap,
    format,
    string::String,
    sync::{Arc, Weak},
    vec::Vec,
};
use futures_util::task::AtomicWaker;
use log::{debug, error, warn};
use spin::{
    rwlock::{RwLock, RwLockWriteGuard},
    Lazy, Mutex, MutexGuard, RwLockReadGuard,
};

use crate::{
    fs::{FSInfo, FdTable},
    mm::{MemorySet, MemorySetInner},
    signal::{send_signal_to_thread_group, SigSet, SigTable},
    syscall::CloneFlags,
    task::{TaskControlBlock, TidHandle},
    utils::{get_abs_path, is_abs_path, SysErrNo, SyscallRet},
};

/// 进程/线程组 类
/// 它的Arc是TCB
pub struct Process {
    pub inner: Mutex<ProcessInner>,
    pub pid: usize,
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
    pub fd_table: Arc<FdTable>,
    pub fs_info: Arc<FSInfo>,
    /// personality(2) — PER_LINUX = 0
    pub personality: u32,
}

impl Process {
    /// 退出时把尚未 wait 的子进程挂到 initproc，避免子进程继续强引用已退出父进程。
    pub fn exit_and_reparent(&self) {
        let orphans: Vec<Arc<Process>> = {
            let mut meta = self.meta_lock();
            let orphans: Vec<Arc<Process>> =
                meta.children.iter().filter_map(|w| w.upgrade()).collect();
            meta.children.clear();
            meta.tasks.clear();
            orphans
        };
        if orphans.is_empty() {
            return;
        }

        const INIT_PID: usize = 1;
        let initproc = Self::get_process_arc_by_pid(INIT_PID).expect("initproc not found!");
        for child in &orphans {
            child.meta_lock().parent_pid = INIT_PID;
            Self::link_child_to_parent(&initproc, child);
        }
        drop(initproc);

        let _ = send_signal_to_thread_group(INIT_PID, SigSet::SIGCHLD);
        debug!(
            "[exit_and_reparent] process[{}] reparented {} child(ren) to init",
            self.pid,
            orphans.len()
        );
    }

    /// 在父进程的 children 中登记子进程（按 pid 去重）。
    fn link_child_to_parent(parent: &Process, child: &Arc<Process>) {
        let mut meta = parent.meta_lock();
        if meta
            .children
            .iter()
            .filter_map(|w| w.upgrade())
            .any(|p| p.pid == child.pid)
        {
            return;
        }
        meta.children.push(Arc::downgrade(child));
    }
    /// 创建新进程
    pub fn new(
        memory_set: Arc<RwLock<MemorySet>>,
        sig_table: Arc<Mutex<SigTable>>,
        fd_table: Arc<FdTable>,
        fs_info: Arc<FSInfo>,
        pid: usize,
        parent_pid: usize,
    ) -> Arc<Self> {
        let id = pid;
        let ret = Arc::new(Self {
            inner: Mutex::new(ProcessInner {
                memory_set,
                sig_table,
                fd_table,
                fs_info,
                personality: 0,
            }),
            pid,
            meta: Mutex::new(ProcessMeta {
                tasks: Vec::new(),
                children: Vec::new(),
                parent_pid,
                child_exit_event: AtomicWaker::new(),
                exit_signal: -1,
            }),
        });
        if parent_pid != 0 {
            if let Some(parent_process) = Self::get_process_arc_by_pid(parent_pid) {
                Self::link_child_to_parent(&parent_process, &ret);
            }
        }
        debug!("inserting process {}", id);
        let oldval = PID_2_PROCESS_ARC
            .try_lock()
            .unwrap()
            .insert(id, ret.clone());
        if let Some(old_proc) = oldval {
            debug!("expected replacement? {}", old_proc.pid);
        }
        ret
    }
    /// 获取inner的锁
    pub fn inner_lock(&self) -> MutexGuard<'_, ProcessInner> {
        self.inner
            .try_lock()
            .expect(&format!("fail to get proc lock({})", self.pid))
    }
    /// 获取元数据的锁
    pub fn meta_lock(&self) -> MutexGuard<'_, ProcessMeta> {
        self.meta
            .try_lock()
            .expect(&format!("fail to get proc.meta lock({})", self.pid))
    }
    /// 获取父进程的 pid（0 表示无父进程，例如 initproc）
    pub fn ppid(&self) -> usize {
        self.meta_lock().parent_pid
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
    /// 向进程中添加一个线程
    pub fn add_task(&self, task: Arc<TaskControlBlock>) {
        self.meta_lock().tasks.push(Arc::downgrade(&task));
    }
    /// 获取当前进程中还活着的线程数量
    pub fn alive_tasks_count(&self) -> usize {
        self.meta_lock()
            .tasks
            .iter()
            .filter(|t| t.upgrade().is_some())
            .count()
    }

    // 只会在sys_fcntl里面调用的一些辅助函数

    // pub fn do_fcntl_setfd(&self,fd:usize, cloexec:bool)->SyscallRet {
    //     if cloexec {
    //         self.inner_lock().fd_table.set_cloexec(fd)
    //     }else{
    //         self.inner_lock().fd_table.unset_cloexec(fd)
    //     }
    // }
    // pub fn do_fcntl_setfl(&self, fd:usize, nonblock:bool)->SyscallRet {
    //     if nonblock {
    //         self.inner_lock().fd_table.set_nonblock(fd)
    //     }else {
    //         self.inner_lock().fd_table.unset_nonblock(fd)
    //     }
    // }
}

impl Drop for Process {
    fn drop(&mut self) {
        // PID_2_PROCESS_ARC.try_lock().unwrap().remove(&self.pid);
        debug!("proc {} is dropped", self.pid);
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
    /// 获取绝对路径
    pub fn get_abs_path(&self, dirfd: isize, path: &str) -> Result<String, SysErrNo> {
        if is_abs_path(path) {
            Ok(get_abs_path("/", path))
        } else if dirfd != -100 {
            // AT_FDCWD=-100
            let dirfd = dirfd as usize;
            if let Some(file) = self.fd_table.try_get(dirfd) {
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
            Ok(get_abs_path(&self.fs_info.get_cwd(), path))
        }
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
    /// 父进程 pid；0 表示无父进程
    pub parent_pid: usize,
    /// 子进程退出事件，用于唤醒等待中的父进程
    pub child_exit_event: AtomicWaker,
    /// 进程退出时发送给父进程的信号（对应 Linux task_struct.exit_signal）
    /// - 普通 fork (SIGCHLD): 值为 17 (SIGCHLD)，退出时通知父进程
    /// - clone/thread (无 SIGCHLD): 值为 -1，退出时不发送信号
    /// 用于 waitpid 的 __WALL/__WCLONE 过滤以及退出时是否发送信号给父进程
    pub exit_signal: i32,
}

static PID_2_PROCESS_ARC: Lazy<Mutex<BTreeMap<usize, Arc<Process>>>> =
    Lazy::new(|| Mutex::new(BTreeMap::new()));
