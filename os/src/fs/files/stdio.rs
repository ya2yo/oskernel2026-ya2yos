// 该文件定义了两个特殊的文件类：Stdin和Stdout
// 它们没有成员，但是实现了File trait
// 它们的底层是sbi.rs
use super::super::{File, Kstat, StMode};
use crate::utils::{SysErrNo, SyscallRet};
use crate::{
    arch::console::console_getchar, mm::UserBuffer, syscall::PollEvents,
    task::suspend_current_and_run_next,
};
use alloc::vec::Vec;

const LF: usize = 0x0a;
const CR: usize = 0x0d;

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
        while count < user_buf.len() {
            match console_getchar() {
                None => {
                    // 没有输入，阻塞，挂起当前任务并运行下一个任务
                    suspend_current_and_run_next();
                    continue;
                }
                Some(c) => match c {
                    b'\r' => {
                        buf.push(b'\n');
                        count += 1;
                        break;
                    }
                    b'\n' => {
                        buf.push(b'\n');
                        count += 1;
                        break;
                    }
                    _ => {
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
            print!("{}", core::str::from_utf8(buffer).unwrap());
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
}
