/// 存放系统调用的各种Option
use crate::mm::MapPermission;
use linux_raw_sys::general::{WCONTINUED, WNOHANG, WNOWAIT, WUNTRACED};
use num_enum::FromPrimitive;

/// 可以打开文件的最大数量
pub const FILE_LIMIT: usize = 1024;

bitflags! {
    pub struct WaitOption:u32{
        const DEFAULT = 0;
        const WNOHANG = WNOHANG;
        const WUNTRACED = WUNTRACED;
        const WCONTINUED = WCONTINUED;
        const WNOWAIT = WNOWAIT;
    }
}

// For Mmap
bitflags! {
    /// Mmap permissions
    pub struct MmapProt: u32 {
        /// None
        const PROT_NONE = 0;
        /// Readable
        const PROT_READ = 1 << 0;
        /// Writable
        const PROT_WRITE = 1 << 1;
        /// Executable
        const PROT_EXEC = 1 << 2;
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
        const MAP_SHARED = 1 << 0;
        /// 修改不会同步到文件
        const MAP_PRIVATE = 1 << 1;
        /// 强制使用该位置进行映射
        const MAP_FIXED = 1 << 4;
        /// 创建匿名映射​（不与文件关联，初始化为零），此时 fd 应为 -1。
        const MAP_ANONYMOUS = 1 << 5;
        /// 有了这个标志后，禁止对映射文件写入（通过 write 调用），仅允许通过映射修改。
        const MAP_DENYWRITE = 1 << 11;
        /// 栈，自动延伸
        const MAP_STACK = 1 << 17;
        const MAP_14 = 1<<14;   // TODO: 奇奇怪怪，为什么glibc entry-static.exe pthread_cancel_points会用到这个标志位？
    }
}

#[repr(usize)]
#[allow(non_camel_case_types)]
#[derive(Debug, PartialEq, FromPrimitive)]
pub enum IoctlCommand {
    // For struct termios
    /// Gets the current serial port settings.
    TCGETS = 0x5401,
    /// Sets the serial port settings immediately.
    TCSETS = 0x5402,
    /// Sets the serial port settings after allowing the input and output buffers to drain/empty.
    TCSETSW = 0x5403,
    /// Sets the serial port settings after flushing the input and output buffers.
    TCSETSF = 0x5404,
    /// For struct termio
    /// Gets the current serial port settings.
    TCGETA = 0x5405,
    /// Sets the serial port settings immediately.
    TCSETA = 0x5406,
    /// Sets the serial port settings after allowing the input and output buffers to drain/empty.
    TCSETAW = 0x5407,
    /// Sets the serial port settings after flushing the input and output buffers.
    TCSETAF = 0x5408,
    /// Get the process group ID of the foreground process group on this terminal.
    TIOCGPGRP = 0x540F,
    /// Set the foreground process group ID of this terminal.
    TIOCSPGRP = 0x5410,
    /// Get window size.
    TIOCGWINSZ = 0x5413,
    /// Set window size.
    TIOCSWINSZ = 0x5414,
    /// Non-cloexec
    FIONCLEX = 0x5450,
    /// Cloexec
    FIOCLEX = 0x5451,
    /// rustc using pipe and ioctl pipe file with this request id
    /// for non-blocking/blocking IO control setting
    FIONBIO = 0x5421,
    /// Read time
    RTC_RD_TIME = 0x80247009,
    #[num_enum(default)]
    Default = 0,
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
        const IN = 0x0001;
        /// 紧急数据
        const PRI = 0x002;
        /// 可写
        const OUT = 0x0004;
        /// 报错
        const ERR = 0x0008;
        /// 已终止，如 pipe 的另一端已关闭连接的情况
        const HUP = 0x0010;
        /// 无效的 fd
        const INVAL = 0x0020;
        /// 普通数据可读
        const RDNORM=0x0040;
        /// 有优先带数据可以读
        const RDBAND = 0x0080;
        /// 可以写入普通数据
        const WRNORM = 0x0100;
        /// 可以写入优先带数据
        const WRBAND = 0x0200;
        /// 有一个sigpoll消息可用
        const MSG = 0x0400;
        /// 将该文件描述符从队列中移除
        const REMOVE = 0x1000;
        /// socket关闭连接
        const RDHUP = 0x2000;
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
        const SIG_BLOCK = 0;
        const SIG_UNBLOCK = 1;
        const SIG_SETMASK = 2;
    }
}

bitflags! {
    pub struct FcntlCmd:usize{
        const F_DUPFD = 0;
        const F_GETFD = 1;
        const F_SETFD = 2;
        const F_GETFL = 3;
        const F_SETFL = 4;
        const F_GETLK = 5;
        const F_SETLK = 6;
        const F_SETLKW = 7;
        const F_SETOWN = 8;
        const F_GETOWN = 9;
        const F_SETSIG = 10;
        const F_GETSIG = 11;
        const F_GETLK64 = 12;
        const F_SETLK64 = 13;
        const F_SETLKW64 = 14;
        const F_SETOWN_EX = 15;
        const F_GETOWN_EX = 16;
        const F_OFD_GETLK = 36;
        const F_OFD_SETLK = 37;
        const F_OFD_SETLKW = 38;
        const F_SETLEASE = 1024;
        const F_GETLEASE = 1025;
        const F_NOTIFY = 1026;
        const F_DUPFD_QUERY = 1027;
        const F_DUPFD_CLOEXEC = 1030;
        const F_SETPIPE_SZ = 1031;
        const F_GETPIPE_SZ = 1032;
    }
}

pub const FD_SET_SIZE: usize = 1024;
pub const FD_SET_LEN: usize = FD_SET_SIZE / (8 * core::mem::size_of::<usize>());

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
    pub fn got_fd(&mut self, fd: usize) -> bool {
        assert!(fd < FD_SET_SIZE);
        let offset = fd % FD_SET_LEN;
        (self.fds_bits[fd / FD_SET_LEN] & (1 << offset)) != 0
    }
    pub fn mark_fd(&mut self, fd: usize, value: bool) {
        if fd >= FD_SET_SIZE {
            return;
        }
        let offset = fd % FD_SET_LEN;
        if value {
            self.fds_bits[fd / FD_SET_LEN] |= 1 << offset;
        } else {
            self.fds_bits[fd / FD_SET_LEN] &= !(1 << offset);
        }
    }
}

#[repr(u32)] // 确保底层类型为u32，与C一致
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FutexCmd {
    Wait = 0,
    Wake = 1,
    Fd = 2, // 已废弃
    Requeue = 3,
    CmpRequeue = 4,
    WakeOp = 5,
    LockPi = 6,
    UnlockPi = 7,
    TrylockPi = 8,
    WaitBitset = 9,
    WakeBitset = 10,
}

impl TryFrom<u32> for FutexCmd {
    type Error = ();
    fn try_from(value: u32) -> Result<Self, ()> {
        match value {
            0 => Ok(FutexCmd::Wait),
            1 => Ok(FutexCmd::Wake),
            2 => Ok(FutexCmd::Fd),
            3 => Ok(FutexCmd::Requeue),
            4 => Ok(FutexCmd::CmpRequeue),
            5 => Ok(FutexCmd::WakeOp),
            6 => Ok(FutexCmd::LockPi),
            7 => Ok(FutexCmd::UnlockPi),
            8 => Ok(FutexCmd::TrylockPi),
            9 => Ok(FutexCmd::WaitBitset),
            10 => Ok(FutexCmd::WakeBitset),
            // ...其他匹配
            _ => Err(()),
        }
    }
}

bitflags! {
pub struct FutexOpt: u32 {
    const FUTEX_PRIVATE_FLAG = 128;
    const FUTEX_CLOCK_REALTIME = 256;
}
}

bitflags! {
    pub struct FaccessatFileMode : u32 {
        const S_ISUID = 0o04000;
        const S_ISGID = 0o02000;
        const S_ISVTX = 0o01000;

        const S_IRUSR = 0o0400;
        const S_IWUSR = 0o0200;
        const S_IXUSR = 0o0100;
        const S_IRWXU = 0o0700;
        const S_IRGRP = 0o0040;
        const S_IWGRP = 0o0020;
        const S_IXGRP = 0o0010;
        const S_IRWXG = 0o0070;
        const S_IROTH = 0o0004;
        const S_IWOTH = 0o0002;
        const S_IXOTH = 0o0001;
        const S_IRWXO = 0o0007;
    }
}

bitflags! {
    pub struct FaccessatMode: u32 {
        const F_OK = 0;
        const X_OK = 1;
        const W_OK = 2;
        const R_OK = 4;
    }
}
