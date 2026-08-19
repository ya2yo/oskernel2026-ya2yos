//! 动态只读 `/proc/uptime` 文件。
//!
//! 每个打开描述符首次读取时生成一个快照，内容为“系统运行时间 空闲时间”两
//! 个保留两位小数的秒数；同一描述符后续分段读取和 seek 不会因时钟推进而改变
//! 已经开始读取的文本。重新 seek 到文件起点会重新生成快照。该文件没有持久
//! 化数据，也不接受写入，`poll(POLLIN)` 始终报告可读。

use crate::{
    arch::{
        memory_layout::PAGE_SIZE,
        time::{get_clock_freq, get_ticks},
    },
    fs::{File, Kstat, StMode, SEEK_CUR, SEEK_END, SEEK_SET},
    mm::UserBuffer,
    syscall::PollEvents,
    task::idle_ticks,
    utils::SyscallRet,
};
use alloc::{
    borrow::Cow,
    string::{String, ToString},
    sync::Arc,
};
use spin::Mutex;

const UPTIME_PATH: &str = "/proc/uptime";

/// 每次打开 `/proc/uptime` 对应的动态文件视图。
///
/// `inner` 同时保护读取偏移和当前快照，确保分段读取期间内容稳定。
pub struct UptimeFile {
    /// 当前描述符的偏移和已生成的内容快照。
    inner: Mutex<UptimeFileInner>,
}

struct UptimeFileInner {
    offset: usize,
    snapshot: Option<String>,
}

impl UptimeFile {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(UptimeFileInner {
                offset: 0,
                snapshot: None,
            }),
        })
    }

    fn content() -> String {
        let frequency = get_clock_freq().max(1) as u128;
        let uptime_ticks = get_ticks() as u128;
        let idle_ticks = idle_ticks() as u128;
        let uptime_centiseconds = uptime_ticks * 100 / frequency;
        let idle_centiseconds = idle_ticks * 100 / frequency;
        format_fixed_centiseconds(uptime_centiseconds, idle_centiseconds)
    }
}

fn format_fixed_centiseconds(uptime: u128, idle: u128) -> String {
    let mut content = String::new();
    let uptime_seconds = uptime / 100;
    let idle_seconds = idle / 100;
    content.push_str(&uptime_seconds.to_string());
    content.push('.');
    push_two_digits(&mut content, (uptime % 100) as u8);
    content.push(' ');
    content.push_str(&idle_seconds.to_string());
    content.push('.');
    push_two_digits(&mut content, (idle % 100) as u8);
    content.push('\n');
    content
}

fn push_two_digits(content: &mut String, value: u8) {
    content.push((b'0' + value / 10) as char);
    content.push((b'0' + value % 10) as char);
}

impl File for UptimeFile {
    /// `/proc/uptime` 是只读动态视图，读取从当前快照和偏移继续。
    fn readable(&self) -> bool {
        true
    }

    fn writable(&self) -> bool {
        false
    }

    fn read(&self, mut buf: UserBuffer) -> SyscallRet {
        let mut inner = self.inner.lock();
        if inner.snapshot.is_none() {
            inner.snapshot = Some(Self::content());
        }
        let content = inner.snapshot.as_ref().unwrap();
        if inner.offset >= content.len() {
            return Ok(0);
        }
        let read_len = buf.len().min(content.len() - inner.offset);
        let copied = buf.write(&content.as_bytes()[inner.offset..inner.offset + read_len]);
        inner.offset += copied;
        Ok(copied)
    }

    fn write(&self, _buf: UserBuffer) -> SyscallRet {
        Err(crate::utils::SysErrNo::EBADF)
    }

    fn fstat(&self) -> Kstat {
        let content = Self::content();
        Kstat {
            st_mode: StMode::FREG.bits() | 0o444,
            st_nlink: 1,
            st_size: content.len() as isize,
            st_blksize: PAGE_SIZE as i32,
            ..Kstat::default()
        }
    }

    fn path(&self) -> Cow<'_, str> {
        Cow::Borrowed(UPTIME_PATH)
    }

    fn lseek(&self, offset: isize, whence: usize) -> SyscallRet {
        let mut inner = self.inner.lock();
        if inner.snapshot.is_none() {
            inner.snapshot = Some(Self::content());
        }
        let content_len = inner.snapshot.as_ref().unwrap().len();
        let base = match whence {
            SEEK_SET => 0isize,
            SEEK_CUR => inner.offset as isize,
            SEEK_END => content_len as isize,
            _ => return Err(crate::utils::SysErrNo::EINVAL),
        };
        let next = base
            .checked_add(offset)
            .ok_or(crate::utils::SysErrNo::EINVAL)?;
        if next < 0 {
            return Err(crate::utils::SysErrNo::EINVAL);
        }
        inner.offset = next as usize;
        if whence == SEEK_SET && inner.offset == 0 {
            inner.snapshot = Some(Self::content());
        }
        Ok(inner.offset)
    }

    fn poll(&self, events: PollEvents) -> PollEvents {
        if events.contains(PollEvents::IN) {
            PollEvents::IN
        } else {
            PollEvents::empty()
        }
    }
}
