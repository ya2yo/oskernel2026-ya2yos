use alloc::{
    collections::btree_map::BTreeMap,
    format,
    string::String,
    sync::{Arc, Weak},
    vec::Vec,
};
use futures_util::task::AtomicWaker;
use log::{debug, warn};
use spin::{Lazy, Mutex, MutexGuard};

use crate::{
    fs::{remove_proc_dir_and_file, FSInfo, FdTable},
    mm::MemorySet,
    signal::{send_signal_to_thread_group, SigSet, SigTable},
    task::{TaskControlBlock, TidHandle},
    utils::{get_abs_path, is_abs_path, ResourceSlot, SysErrNo},
};

/// 进程/线程组 类
/// 它的Arc是TCB
pub struct Process {
    /// 当前地址空间。`MemorySet` 内部自带锁；槽位只保护 exec 时替换
    /// 整份地址空间的 Arc 指针。
    memory_set: ResourceSlot<MemorySet>,
    /// 当前信号动作表。内层 Mutex 保护 `SigTable` 内容；槽位只保护
    /// exec/clone 后重置信号表时替换整张表的 Arc 指针。
    sig_table: ResourceSlot<Mutex<SigTable>>,
    /// 进程打开的文件描述符表。本身带锁，不再放入 PCB 内部锁。
    pub fd_table: Arc<FdTable>,
    /// 文件系统上下文。本身带锁，不再放入 PCB 内部锁。
    pub fs_info: Arc<FSInfo>,
    pub pid: usize,
    pub meta: Mutex<ProcessMeta>,
}
// 我们需要向编译器保证Process含有这样的特性……这样真的好吗？
unsafe impl Send for Process {}
unsafe impl Sync for Process {}

impl Process {
    /// 退出时把尚未 wait 的子进程挂到 initproc，避免子进程继续强引用已退出父进程。
    pub fn exit_and_reparent(&self) {
        let orphans: Vec<Arc<Process>> = {
            let mut meta = self.meta_lock();
            let orphans: Vec<Arc<Process>> =
                meta.children.iter().filter_map(|w| w.upgrade()).collect();
            debug!(
                "[exit_and_reparent] pid {} clearing {} children, {} tasks",
                self.pid,
                orphans.len(),
                meta.tasks.len()
            );
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
        memory_set: Arc<MemorySet>,
        sig_table: Arc<Mutex<SigTable>>,
        fd_table: Arc<FdTable>,
        fs_info: Arc<FSInfo>,
        pid: usize,
        parent_pid: usize,
        pgid: usize,
        sid: usize,
    ) -> Arc<Self> {
        let id = pid;
        let ret = Arc::new(Self {
            memory_set: ResourceSlot::new(memory_set),
            sig_table: ResourceSlot::new(sig_table),
            fd_table,
            fs_info,
            pid,
            meta: Mutex::new(ProcessMeta {
                tasks: Vec::new(),
                children: Vec::new(),
                parent_pid,
                pgid,
                sid,
                child_exit_event: AtomicWaker::new(),
                exit_signal: -1,
                group_exit_code: None,
                stopped_signal: None,
                continued_signal: None,
                termination_signal: None,
                usage: ProcessUsage::default(),
                comm: String::from("initproc"),
                personality: 0,
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
    /// 获取进程组 ID
    pub fn pgid(&self) -> usize {
        self.meta_lock().pgid
    }
    /// 获取会话 ID
    pub fn sid(&self) -> usize {
        self.meta_lock().sid
    }
    /// 获取 personality(2) 执行域。
    pub fn personality(&self) -> u32 {
        self.meta_lock().personality
    }
    /// 设置 personality(2) 执行域，返回旧值。
    pub fn set_personality(&self, persona: u32) -> u32 {
        let mut meta = self.meta_lock();
        let old = meta.personality;
        meta.personality = persona;
        old
    }
    /// 获取当前地址空间的资源指针。`MemorySet` 自身负责读写同步。
    pub fn memory_set_arc(&self) -> Arc<MemorySet> {
        self.memory_set.get()
    }
    /// 获取当前进程的文件描述表
    pub fn fd_table_arc(&self) -> Arc<FdTable> {
        self.fd_table.clone()
    }
    pub fn sig_table_arc(&self) -> Arc<Mutex<SigTable>> {
        self.sig_table.get()
    }
    pub fn memory_set_strong_count(&self) -> usize {
        self.memory_set.strong_count()
    }
    /// 在当前信号表锁内执行操作。
    pub fn with_sigtable<T>(&self, f: impl FnOnce(&mut SigTable) -> T) -> T {
        let sig_table = self.sig_table_arc();
        let mut sig_table = sig_table
            .try_lock()
            .expect("You should not fail to get sig_table lock in a 1 HART system!");
        f(&mut sig_table)
    }
    /// 获取绝对路径
    pub fn get_abs_path(&self, dirfd: isize, path: &str) -> Result<String, SysErrNo> {
        if is_abs_path(path) {
            // 绝对路径不受 dirfd/cwd 影响，统一从根目录解析。
            Ok(get_abs_path("/", path))
        } else if dirfd != -100 {
            // 相对路径且 dirfd 不是 AT_FDCWD(-100)：以 dirfd 指向的目录或文件路径为基准。
            let dirfd = dirfd as usize;
            if let Some(file) = self.fd_table.try_get(dirfd) {
                let base_path = file.file()?.inode.path();
                if path.is_empty() {
                    // 空路径用于部分 *at syscall 的 AT_EMPTY_PATH 语义，直接返回 dirfd 对应路径。
                    Ok(base_path)
                } else {
                    // 非空相对路径拼到 dirfd 对应路径下，再规范化成绝对路径。
                    Ok(get_abs_path(&base_path, path))
                }
            } else {
                // 显式 dirfd 无效时，路径解析失败。
                Err(SysErrNo::EINVAL)
            }
        } else {
            // 相对路径且 dirfd 为 AT_FDCWD(-100)：以当前进程 cwd 为基准。
            Ok(get_abs_path(&self.fs_info.get_cwd(), path))
        }
    }
    /// 线程组是否已经进入退出流程。
    pub fn is_group_exiting(&self) -> bool {
        self.meta_lock().group_exit_code.is_some()
    }
    /// 获取线程组退出码。
    pub fn group_exit_code(&self) -> i32 {
        self.meta_lock()
            .group_exit_code
            .expect("process exit code should have been set")
    }
    /// 首次设置线程组退出码；返回 false 表示已有其他线程设置过。
    pub fn set_group_exit_code_once(&self, exit_code: i32) -> bool {
        let mut meta = self.meta_lock();
        if meta.group_exit_code.is_some() {
            false
        } else {
            meta.group_exit_code = Some(exit_code);
            true
        }
    }
    /// 改变内存映射关系和信号表
    pub fn change_memory_set_and_sigtable(
        &self,
        new_memory_set: MemorySet,
        new_sigtable: SigTable,
    ) {
        self.memory_set.replace_with(new_memory_set);
        self.sig_table.replace_with(Mutex::new(new_sigtable));
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
        self.is_group_exiting()
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
        remove_proc_dir_and_file(pid);
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
}

impl Drop for Process {
    fn drop(&mut self) {
        // PID_2_PROCESS_ARC.try_lock().unwrap().remove(&self.pid);
        debug!("proc {} is dropped", self.pid);
    }
}

#[derive(Clone, Copy, Default)]
pub struct ProcessUsage {
    pub utime: isize,
    pub stime: isize,
    pub cutime: isize,
    pub cstime: isize,
    pub maxrss: usize,
    pub cmaxrss: usize,
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
    /// 进程组 ID，用于 waitpid(0)、waitpid(<-1)、setpgid/getpgid。
    pub pgid: usize,
    /// 会话 ID，用于 getsid/setsid 和进程组会话边界检查。
    pub sid: usize,
    /// 子进程退出事件，用于唤醒等待中的父进程
    pub child_exit_event: AtomicWaker,
    /// 进程退出时发送给父进程的信号（对应 Linux task_struct.exit_signal）
    /// - 普通 fork: 通常为 17 (SIGCHLD)，退出时通知父进程
    /// - clone 可指定其他退出信号；0 表示不发送信号，这里存为 -1
    /// - 线程共享进程元数据，不覆盖线程组原有 exit_signal
    /// 用于 waitpid 的 __WALL/__WCLONE 过滤以及退出时是否发送信号给父进程
    pub exit_signal: i32,
    /// 线程组退出码；属于进程/线程组状态，不能放在 CLONE_SIGHAND 共享的 SigTable 中。
    pub group_exit_code: Option<i32>,
    /// 最近一次导致该进程停止的信号，用于 waitid(WSTOPPED) 返回 CLD_STOPPED。
    pub stopped_signal: Option<usize>,
    /// 最近一次 SIGCONT 恢复 stopped 进程的事件，用于 waitid(WCONTINUED)。
    pub continued_signal: Option<usize>,
    /// 默认信号动作导致进程终止时记录信号号及是否 core dump。
    pub termination_signal: Option<(usize, bool)>,
    /// 进程退出时冻结的资源使用快照，供父进程 wait 后累计 RUSAGE_CHILDREN。
    pub usage: ProcessUsage,
    /// Linux task comm，供 /proc 与 process accounting 等只需要短命令名的路径使用。
    pub comm: String,
    /// personality(2) 执行域，属于进程级元数据。
    pub personality: u32,
}

static PID_2_PROCESS_ARC: Lazy<Mutex<BTreeMap<usize, Arc<Process>>>> =
    Lazy::new(|| Mutex::new(BTreeMap::new()));
