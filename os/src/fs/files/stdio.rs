// 该文件定义了两个特殊的文件类：Stdin和Stdout
// 它们没有成员，但是实现了File trait
// 它们的底层是sbi.rs
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
    sync::atomic::{AtomicU32, Ordering},
};

const LF: u8 = 0x0a;
const CR: u8 = 0x0d;
const BS: u8 = 0x08;
const DEL: u8 = 0x7f;

const TCGETS: u32 = 0x5401;
const TCSETS: u32 = 0x5402;
const TCSETSW: u32 = 0x5403;
const TCSETSF: u32 = 0x5404;
const TIOCGWINSZ: u32 = 0x5413;

const ISIG: u32 = 0x00001;
const ICANON: u32 = 0x00002;
const ECHO: u32 = 0x00008;
const ECHOE: u32 = 0x00010;
const ECHOK: u32 = 0x00020;
const ECHOCTL: u32 = 0x00200;
const IEXTEN: u32 = 0x08000;

const DEFAULT_LFLAG: u32 = ISIG | ICANON | ECHO | ECHOE | ECHOK | ECHOCTL | IEXTEN;
static TERMINAL_LFLAG: AtomicU32 = AtomicU32::new(DEFAULT_LFLAG);

#[repr(C)]
#[derive(Clone, Copy)]
struct RawTermios {
    c_iflag: u32,
    c_oflag: u32,
    c_cflag: u32,
    c_lflag: u32,
    c_line: u8,
    c_cc: [u8; 32],
    c_pad: [u8; 3],
    c_ispeed: u32,
    c_ospeed: u32,
}

impl RawTermios {
    fn current() -> Self {
        let mut c_cc = [0u8; 32];
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
            c_pad: [0; 3],
            c_ispeed: 0,
            c_ospeed: 0,
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

pub struct Stdin;

pub struct Stdout;

impl File for Stdin {
    fn readable(&self) -> bool {
        true
    }
    fn writable(&self) -> bool {
        false
    }
    fn read(&self, mut user_buf: UserBuffer) -> SyscallRet {
        // panic!("HXC: What do you want from stdin??");
        //一次读取多个字符
        let mut count: usize = 0;
        let mut buf = Vec::new();
        let lflag = TERMINAL_LFLAG.load(Ordering::Relaxed);
        let echo_enabled = lflag & ECHO != 0;
        let canonical = lflag & ICANON != 0;
        while count < user_buf.len() {
            match console_getchar() {
                None => {
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
        if events.contains(PollEvents::IN) {
            revents |= PollEvents::IN;
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
