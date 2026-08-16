/// 存放系统调用的各种Option
use crate::mm::MapPermission;
use linux_raw_sys::general as linux;
use num_enum::FromPrimitive;

/// 可以打开文件的最大数量
pub const FILE_LIMIT: usize = 1024;

bitflags! {
    pub struct WaitOption:u32{
        const DEFAULT = 0;
        const WNOHANG = linux::WNOHANG;
        const WUNTRACED = linux::WUNTRACED;
        const WSTOPPED = linux::WSTOPPED;
        const WEXITED = linux::WEXITED;
        const WCONTINUED = linux::WCONTINUED;
        const WNOWAIT = linux::WNOWAIT;
        /// Don't wait on children of other threads in this group
        const __WONTHREAD = linux::__WNOTHREAD;
        /// Wait for all children regardless of exit_signal type (SIGCHLD or clone)
        const __WALL = linux::__WALL;
        /// Wait for clone children only (those created without SIGCHLD, exit_signal == -1)
        const __WCLONE = linux::__WCLONE;
    }
}
bitflags! {
    pub struct IdType: u32 {
        const P_ALL = linux::P_ALL;
        const P_PID = linux::P_PID;
        const P_PGID = linux::P_PGID;
        const P_PIDFD = linux::P_PIDFD;
    }
}

// For Mmap
bitflags! {
    /// Mmap permissions
    pub struct MmapProt: u32 {
        /// None
        const PROT_NONE = linux::PROT_NONE;
        /// Readable
        const PROT_READ = linux::PROT_READ;
        /// Writable
        const PROT_WRITE = linux::PROT_WRITE;
        /// Executable
        const PROT_EXEC = linux::PROT_EXEC;
    }
}

impl From<MmapProt> for MapPermission {
    fn from(prot: MmapProt) -> Self {
        let mut map_permission = MapPermission::U;
        if prot.contains(MmapProt::PROT_READ) {
            map_permission |= MapPermission::R;
        }
        if prot.contains(MmapProt::PROT_WRITE) {
            map_permission |= MapPermission::W;
        }
        if prot.contains(MmapProt::PROT_EXEC) {
            map_permission |= MapPermission::X;
        }
        map_permission
    }
}

bitflags! {
    /// Mmap flags
    pub struct MmapFlags: u32 {
        /// 修改会同步到文件，其他进程会看见这些修改
        const MAP_SHARED = linux::MAP_SHARED;
        /// 修改不会同步到文件
        const MAP_PRIVATE = linux::MAP_PRIVATE;
        /// 强制使用该位置进行映射
        const MAP_FIXED = linux::MAP_FIXED;
        /// 创建匿名映射​（不与文件关联，初始化为零），此时 fd 应为 -1。
        const MAP_ANONYMOUS = linux::MAP_ANONYMOUS;
        /// 向下增长的映射（用于栈）
        const MAP_GROWSDOWN = linux::MAP_GROWSDOWN;
        /// This flag is ignored.
        const MAP_EXECUTABLE = linux::MAP_EXECUTABLE;
        /// 有了这个标志后，禁止对映射文件写入（通过 write 调用），仅允许通过映射修改。
        const MAP_DENYWRITE = linux::MAP_DENYWRITE;
        /// 栈，自动延伸
        const MAP_STACK = linux::MAP_STACK;
        /// 将映射计入进程的锁定内存（VmLck）
        const MAP_LOCKED = linux::MAP_LOCKED;
        const MAP_NORESERVE = linux::MAP_NORESERVE;
        /// 预先填充页表（MAP_POPULATE），我们作为 no-op 接受
        const MAP_POPULATE = linux::MAP_POPULATE;
        /// Anonymous 2 MiB hugetlb mapping (the minimum supported huge page).
        const MAP_HUGETLB = linux::MAP_HUGETLB;
        /// MAP_FIXED_NOREPLACE: 类似 MAP_FIXED，但如果地址已被映射则返回 EEXIST 而不是替换
        const MAP_FIXED_NOREPLACE = linux::MAP_FIXED_NOREPLACE;
        const MAP_SHARED_VALIDATE = linux::MAP_SHARED_VALIDATE;
    }
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct PollFd {
    /// 等待的 fd
    pub fd: i32,
    /// 等待的事件
    pub events: PollEvents,
    /// 返回的事件
    pub revents: PollEvents,
}

impl PollFd {
    pub fn new() -> Self {
        Self {
            fd: 0,
            events: PollEvents::empty(),
            revents: PollEvents::empty(),
        }
    }
}

bitflags! {
    //表示对应在文件上等待或者发生过的事件
    pub struct PollEvents: u16 {
        /// 可读
        const IN = linux::POLLIN as u16;
        /// 紧急数据
        const PRI = linux::POLLPRI as u16;
        /// 可写
        const OUT = linux::POLLOUT as u16;
        /// 报错
        const ERR = linux::POLLERR as u16;
        /// 已终止，如 pipe 的另一端已关闭连接的情况
        const HUP = linux::POLLHUP as u16;
        /// 无效的 fd
        const INVAL = linux::POLLNVAL as u16;
        /// 普通数据可读
        const RDNORM = linux::POLLRDNORM as u16;
        /// 有优先带数据可以读
        const RDBAND = linux::POLLRDBAND as u16;
        /// 可以写入普通数据
        const WRNORM = linux::POLLWRNORM as u16;
        /// 可以写入优先带数据
        const WRBAND = linux::POLLWRBAND as u16;
        /// 有一个sigpoll消息可用
        const MSG = linux::POLLMSG as u16;
        /// 将该文件描述符从队列中移除
        const REMOVE = linux::POLLREMOVE as u16;
        /// socket关闭连接
        const RDHUP = linux::POLLRDHUP as u16;
    }
}

pub struct Utsname {
    pub sysname: [u8; 65],
    pub nodename: [u8; 65],
    pub release: [u8; 65],
    pub version: [u8; 65],
    pub machine: [u8; 65],
    pub domainname: [u8; 65],
}

#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct Iovec {
    /// user space buf starting address
    pub iov_base: usize,
    /// number of bytes to transfer
    pub iov_len: usize,
}
unsafe impl Send for Iovec {}
unsafe impl Sync for Iovec {}

// rlimit
#[allow(unused)]
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct RLimit {
    pub rlim_cur: usize, /* Soft limit */
    pub rlim_max: usize, /* Hard limit (ceiling for rlim_cur) */
}

bitflags! {
    pub struct SignalMaskFlag: u32 {
        const SIG_BLOCK = linux::SIG_BLOCK;
        const SIG_UNBLOCK = linux::SIG_UNBLOCK;
        const SIG_SETMASK = linux::SIG_SETMASK;
    }
}

bitflags! {
    pub struct FcntlCmd:usize{
        const F_DUPFD = linux::F_DUPFD as usize;
        const F_GETFD = linux::F_GETFD as usize;
        const F_SETFD = linux::F_SETFD as usize;
        const F_GETFL = linux::F_GETFL as usize;
        const F_SETFL = linux::F_SETFL as usize;
        const F_GETLK = linux::F_GETLK as usize;
        const F_SETLK = linux::F_SETLK as usize;
        const F_SETLKW = linux::F_SETLKW as usize;
        const F_SETOWN = linux::F_SETOWN as usize;
        const F_GETOWN = linux::F_GETOWN as usize;
        const F_SETSIG = linux::F_SETSIG as usize;
        const F_GETSIG = linux::F_GETSIG as usize;
        const F_GETLK64 = linux::F_GETLK as usize;
        const F_SETLK64 = linux::F_SETLK as usize;
        const F_SETLKW64 = linux::F_SETLKW as usize;
        const F_SETOWN_EX = linux::F_SETOWN_EX as usize;
        const F_GETOWN_EX = linux::F_GETOWN_EX as usize;
        const F_OFD_GETLK = linux::F_OFD_GETLK as usize;
        const F_OFD_SETLK = linux::F_OFD_SETLK as usize;
        const F_OFD_SETLKW = linux::F_OFD_SETLKW as usize;
        const F_SETLEASE = linux::F_SETLEASE as usize;
        const F_GETLEASE = linux::F_GETLEASE as usize;
        const F_NOTIFY = linux::F_NOTIFY as usize;
        const F_DUPFD_QUERY = linux::F_DUPFD_QUERY as usize;
        const F_DUPFD_CLOEXEC = linux::F_DUPFD_CLOEXEC as usize;
        const F_SETPIPE_SZ = linux::F_SETPIPE_SZ as usize;
        const F_GETPIPE_SZ = linux::F_GETPIPE_SZ as usize;
    }
}

pub const FD_SET_SIZE: usize = linux::__FD_SETSIZE as usize;
pub const FD_SET_LEN: usize = FD_SET_SIZE / (8 * core::mem::size_of::<usize>());
const FD_SET_BITS_PER_WORD: usize = 8 * core::mem::size_of::<usize>();

/// FdSet为大小为1024的位图，分装在16个大小为usize的fds_bits子位图中，每一个位都代表一个文件描述符
#[derive(Debug, Copy, Clone)]
#[repr(C)]
pub struct FdSet {
    pub fds_bits: [usize; FD_SET_LEN],
}

impl FdSet {
    pub fn clear_all(&mut self) {
        self.fds_bits.fill(0);
    }
    pub fn got_fd(&self, fd: usize) -> bool {
        assert!(fd < FD_SET_SIZE);
        let offset = fd % FD_SET_BITS_PER_WORD;
        (self.fds_bits[fd / FD_SET_BITS_PER_WORD] & (1 << offset)) != 0
    }
    pub fn mark_fd(&mut self, fd: usize, value: bool) {
        if fd >= FD_SET_SIZE {
            return;
        }
        let offset = fd % FD_SET_BITS_PER_WORD;
        if value {
            self.fds_bits[fd / FD_SET_BITS_PER_WORD] |= 1 << offset;
        } else {
            self.fds_bits[fd / FD_SET_BITS_PER_WORD] &= !(1 << offset);
        }
    }
}

#[repr(u32)] // 确保底层类型为u32，与C一致
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FutexCmd {
    Wait = linux::FUTEX_WAIT,
    Wake = linux::FUTEX_WAKE,
    Fd = linux::FUTEX_FD, // 已废弃
    Requeue = linux::FUTEX_REQUEUE,
    CmpRequeue = linux::FUTEX_CMP_REQUEUE,
    WakeOp = linux::FUTEX_WAKE_OP,
    LockPi = linux::FUTEX_LOCK_PI,
    UnlockPi = linux::FUTEX_UNLOCK_PI,
    TrylockPi = linux::FUTEX_TRYLOCK_PI,
    WaitBitset = linux::FUTEX_WAIT_BITSET,
    WakeBitset = linux::FUTEX_WAKE_BITSET,
}

impl TryFrom<u32> for FutexCmd {
    type Error = ();
    fn try_from(value: u32) -> Result<Self, ()> {
        match value {
            linux::FUTEX_WAIT => Ok(FutexCmd::Wait),
            linux::FUTEX_WAKE => Ok(FutexCmd::Wake),
            linux::FUTEX_FD => Ok(FutexCmd::Fd),
            linux::FUTEX_REQUEUE => Ok(FutexCmd::Requeue),
            linux::FUTEX_CMP_REQUEUE => Ok(FutexCmd::CmpRequeue),
            linux::FUTEX_WAKE_OP => Ok(FutexCmd::WakeOp),
            linux::FUTEX_LOCK_PI => Ok(FutexCmd::LockPi),
            linux::FUTEX_UNLOCK_PI => Ok(FutexCmd::UnlockPi),
            linux::FUTEX_TRYLOCK_PI => Ok(FutexCmd::TrylockPi),
            linux::FUTEX_WAIT_BITSET => Ok(FutexCmd::WaitBitset),
            linux::FUTEX_WAKE_BITSET => Ok(FutexCmd::WakeBitset),
            // ...其他匹配
            _ => Err(()),
        }
    }
}

bitflags! {
pub struct FutexOpt: u32 {
    const FUTEX_PRIVATE_FLAG = linux::FUTEX_PRIVATE_FLAG;
    const FUTEX_CLOCK_REALTIME = linux::FUTEX_CLOCK_REALTIME;
}
}

bitflags! {
    /// 文件元数据和创建 mode 的权限位（`st_mode` 的低 12 位）。
    ///
    /// 用于 `openat(O_CREAT/O_TMPFILE)`、`chmod`、`stat` 和权限判定；它描述的是
    /// 文件自身的权限与特殊位。`faccessat(2)` 的访问请求参数应使用
    /// [`FaccessatMode`]，两者不能互换。
    pub struct FileMode : u32 {
        const S_ISUID = linux::S_ISUID;
        const S_ISGID = linux::S_ISGID;
        const S_ISVTX = linux::S_ISVTX;

        const S_IRUSR = linux::S_IRUSR;
        const S_IWUSR = linux::S_IWUSR;
        const S_IXUSR = linux::S_IXUSR;
        const S_IRWXU = linux::S_IRWXU;
        const S_IRGRP = linux::S_IRGRP;
        const S_IWGRP = linux::S_IWGRP;
        const S_IXGRP = linux::S_IXGRP;
        const S_IRWXG = linux::S_IRWXG;
        const S_IROTH = linux::S_IROTH;
        const S_IWOTH = linux::S_IWOTH;
        const S_IXOTH = linux::S_IXOTH;
        const S_IRWXO = linux::S_IRWXO;
    }
}

bitflags! {
    /// `faccessat(2)` 的访问请求位。
    ///
    /// 这些位表示调用者希望检查读、写、执行还是仅存在，不是文件的 `st_mode`
    /// 权限位。文件权限与特殊位使用 [`FileMode`] 表示。
    pub struct FaccessatMode: u32 {
        const F_OK = linux::F_OK;
        const X_OK = linux::X_OK;
        const W_OK = linux::W_OK;
        const R_OK = linux::R_OK;
    }
}
