//! 标准输入和标准输出对应的字符设备文件。
//!
//! [`Stdin`] 与 [`Stdout`] 本身不保存实例字段，底层通过架构控制台接口访问
//! SBI/UART。标准输入提供规范模式、回显、退格处理、非阻塞读取和终端 ioctl；
//! 标准输出将用户缓冲区逐段转换为 UTF-8 后写入内核控制台。终端状态采用
//! 模块级原子变量，因此同一内核中的标准输入描述符共享终端配置。
use super::super::{File, Kstat, StMode};
use crate::utils::{SysErrNo, SyscallRet};
use crate::{
    arch::console::{console_getchar, console_putchar},
    mm::{copy_from_user, copy_to_user, MemorySet, UserBuffer},
    syscall::PollEvents,
    task::suspend_current_and_run_next,
};
use alloc::vec::Vec;
use core::{
    mem::size_of,
    slice,
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
};
use spin::Mutex;

const LF: u8 = 0x0a;
const CR: u8 = 0x0d;
const BS: u8 = 0x08;
const DEL: u8 = 0x7f;

const TCGETS: u32 = 0x5401;
const TCSETS: u32 = 0x5402;
const TCSETSW: u32 = 0x5403;
const TCSETSF: u32 = 0x5404;
const TIOCGWINSZ: u32 = 0x5413;
const NCCS: usize = 19;

const ISIG: u32 = 0x00001;
const ICANON: u32 = 0x00002;
const ECHO: u32 = 0x00008;
const ECHOE: u32 = 0x00010;
const ECHOK: u32 = 0x00020;
const ECHOCTL: u32 = 0x00200;
const IEXTEN: u32 = 0x08000;

const DEFAULT_LFLAG: u32 = ISIG | ICANON | ECHO | ECHOE | ECHOK | ECHOCTL | IEXTEN;
static TERMINAL_LFLAG: AtomicU32 = AtomicU32::new(DEFAULT_LFLAG);
static STDIN_BUFFER: Mutex<Option<u8>> = Mutex::new(None);
static STDIN_NONBLOCKING: AtomicBool = AtomicBool::new(false);

/// 与 Linux `struct termios` 布局兼容的最小终端属性快照。
#[repr(C)]
#[derive(Clone, Copy)]
struct RawTermios {
    c_iflag: u32,
    c_oflag: u32,
    c_cflag: u32,
    c_lflag: u32,
    c_line: u8,
    c_cc: [u8; NCCS],
}

impl RawTermios {
    fn current() -> Self {
        let mut c_cc = [0u8; NCCS];
        c_cc[2] = DEL;
        c_cc[4] = 4;
        c_cc[5] = 0;
        c_cc[6] = 1;
        c_cc[11] = LF;
        Self {
            c_iflag: 0,
            c_oflag: 0,
            c_cflag: 0,
            c_lflag: TERMINAL_LFLAG.load(Ordering::Relaxed),
            c_line: 0,
            c_cc,
        }
    }

    fn as_bytes(&self) -> &[u8] {
        unsafe { slice::from_raw_parts((self as *const Self).cast::<u8>(), size_of::<Self>()) }
    }

    fn as_bytes_mut(&mut self) -> &mut [u8] {
        unsafe { slice::from_raw_parts_mut((self as *mut Self).cast::<u8>(), size_of::<Self>()) }
    }
}

#[repr(C)]
/// 与终端窗口大小 ioctl 对应的 C 布局结构。
#[repr(C)]
struct RawWinSize {
    ws_row: u16,
    ws_col: u16,
    ws_xpixel: u16,
    ws_ypixel: u16,
}

impl RawWinSize {
    fn default_console() -> Self {
        Self {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        }
    }

    fn as_bytes(&self) -> &[u8] {
        unsafe { slice::from_raw_parts((self as *const Self).cast::<u8>(), size_of::<Self>()) }
    }
}

/// 处理标准输入/输出共用的终端控制请求。
///
/// 这里仅实现当前内核需要的 termios 和窗口大小查询/设置；用户指针通过
/// `copy_to_user`/`copy_from_user` 访问，避免直接解引用用户地址。`TCSETS*`
/// 当前只更新行规程标志，暂不模拟输入输出波特率等未使用字段。
fn terminal_ioctl(cmd: u32, arg: usize, memory_set: &MemorySet) -> SyscallRet {
    match cmd {
        TCGETS => {
            let termios = RawTermios::current();
            copy_to_user(memory_set, arg, termios.as_bytes())?;
            Ok(0)
        }
        TCSETS | TCSETSW | TCSETSF => {
            let mut termios = RawTermios::current();
            copy_from_user(memory_set, arg, termios.as_bytes_mut())?;
            TERMINAL_LFLAG.store(termios.c_lflag, Ordering::Relaxed);
            Ok(0)
        }
        TIOCGWINSZ => {
            let winsize = RawWinSize::default_console();
            copy_to_user(memory_set, arg, winsize.as_bytes())?;
            Ok(0)
        }
        _ => Err(SysErrNo::ENOTTY),
    }
}

/// 按终端回显规则把输入字符显示到控制台。
///
/// 回车统一显示为 CRLF，退格/DEL 用“退格、空格、退格”擦除一个字符；
/// 其他字节直接输出。该函数只负责显示，不改变规范模式下的输入缓冲。
fn echo_input(c: u8) {
    match c {
        LF | CR => {
            console_putchar(b'\r');
            console_putchar(b'\n');
        }
        BS | DEL => {
            console_putchar(BS);
            console_putchar(b' ');
            console_putchar(BS);
        }
        _ => console_putchar(c),
    }
}

/// 优先消费内核暂存的单字节，再轮询架构控制台。
fn stdin_getchar() -> Option<u8> {
    STDIN_BUFFER.lock().take().or_else(console_getchar)
}

/// 查询输入是否就绪，并在必要时把控制台字节放入暂存槽。
fn stdin_has_input() -> bool {
    let mut buffered = STDIN_BUFFER.lock();
    if buffered.is_none() {
        *buffered = console_getchar();
    }
    buffered.is_some()
}

/// 内核标准输入文件对象；所有实例共享终端状态和输入暂存字节。
pub struct Stdin;

/// 内核标准输出文件对象；写入内容直接发送到架构控制台。
pub struct Stdout;

impl File for Stdin {
    fn readable(&self) -> bool {
        true
    }
    fn writable(&self) -> bool {
        false
    }
    /// 按当前终端行规程读取输入：规范模式读到换行即返回，非规范模式
    /// 读到一个字节即返回；无输入时根据全局非阻塞标志返回 `EAGAIN` 或让出 CPU。
    fn read(&self, mut user_buf: UserBuffer) -> SyscallRet {
        // panic!("HXC: What do you want from stdin??");
        //一次读取多个字符
        let mut count: usize = 0;
        let mut buf = Vec::new();
        let lflag = TERMINAL_LFLAG.load(Ordering::Relaxed);
        let echo_enabled = lflag & ECHO != 0;
        let canonical = lflag & ICANON != 0;
        while count < user_buf.len() {
            match stdin_getchar() {
                None => {
                    if STDIN_NONBLOCKING.load(Ordering::Acquire) {
                        if count == 0 {
                            return Err(SysErrNo::EAGAIN);
                        }
                        break;
                    }
                    // 没有输入，阻塞，挂起当前任务并运行下一个任务
                    suspend_current_and_run_next();
                    continue;
                }
                Some(c) if !canonical => {
                    if echo_enabled {
                        echo_input(c);
                    }
                    buf.push(c);
                    count += 1;
                    break;
                }
                Some(c) => match c {
                    b'\r' => {
                        if echo_enabled {
                            echo_input(c);
                        }
                        buf.push(b'\n');
                        count += 1;
                        break;
                    }
                    b'\n' => {
                        if echo_enabled {
                            echo_input(c);
                        }
                        buf.push(b'\n');
                        count += 1;
                        break;
                    }
                    BS | DEL => {
                        if count > 0 {
                            if echo_enabled {
                                echo_input(c);
                            }
                            buf.pop();
                            count -= 1;
                        }
                    }
                    _ => {
                        if echo_enabled {
                            echo_input(c);
                        }
                        buf.push(c);
                        count += 1;
                    }
                },
            }
        }
        user_buf.write(buf.as_slice());
        Ok(count)
    }
    fn write(&self, _user_buf: UserBuffer) -> SyscallRet {
        panic!("Cannot write to stdin!");
        Err(SysErrNo::EINVAL)
        // panic!("Cannot write to stdin!");
    }
    fn poll(&self, events: PollEvents) -> PollEvents {
        let mut revents = PollEvents::empty();
        if events.contains(PollEvents::IN) && stdin_has_input() {
            revents |= PollEvents::IN;
        }
        revents
    }
    fn register(&self, _context: &mut core::task::Context<'_>, _events: PollEvents) {
        // SBI/UART polling has no interrupt-backed waker. ppoll still wakes on
        // its timeout or on the other registered file descriptors.
    }
    fn nonblocking(&self) -> bool {
        STDIN_NONBLOCKING.load(Ordering::Acquire)
    }
    fn set_nonblocking(&self, nonblocking: bool) -> Result<(), SysErrNo> {
        STDIN_NONBLOCKING.store(nonblocking, Ordering::Release);
        Ok(())
    }
    fn fstat(&self) -> Kstat {
        Kstat {
            st_mode: StMode::FCHR.bits(),
            st_nlink: 1,
            ..Kstat::default()
        }
    }
    fn ioctl(&self, cmd: u32, arg: usize, memory_set: &MemorySet) -> SyscallRet {
        terminal_ioctl(cmd, arg, memory_set)
    }
}

impl File for Stdout {
    fn readable(&self) -> bool {
        false
    }
    fn writable(&self) -> bool {
        true
    }
    fn read(&self, _user_buf: UserBuffer) -> SyscallRet {
        panic!("Cannot read from stdout!");
    }
    fn write(&self, user_buf: UserBuffer) -> SyscallRet {
        for buffer in user_buf.buffers.iter() {
            print!(
                "{}",
                core::str::from_utf8(buffer).map_err(|_| SysErrNo::EINVAL)?
            );
        }
        Ok(user_buf.len())
    }
    fn poll(&self, events: PollEvents) -> PollEvents {
        let mut revents = PollEvents::empty();
        if events.contains(PollEvents::OUT) {
            revents |= PollEvents::OUT;
        }
        revents
    }
    fn fstat(&self) -> Kstat {
        Kstat {
            st_mode: StMode::FCHR.bits(),
            st_nlink: 1,
            ..Kstat::default()
        }
    }
    fn ioctl(&self, cmd: u32, arg: usize, memory_set: &MemorySet) -> SyscallRet {
        terminal_ioctl(cmd, arg, memory_set)
    }
}
