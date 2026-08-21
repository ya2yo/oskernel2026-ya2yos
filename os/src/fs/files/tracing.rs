//! Minimal tracefs-compatible virtual files used by the onsite ftrace suite.

use crate::{
    arch::{
        cpu::hart_id,
        memory_layout::PAGE_SIZE,
        time::{get_clock_freq, get_ticks},
    },
    fs::{File, Kstat, OpenFlags, StMode, SEEK_CUR, SEEK_END, SEEK_SET},
    mm::UserBuffer,
    syscall::PollEvents,
    task::current_task,
    utils::{SysErrNo, SyscallRet},
};
use alloc::{borrow::Cow, format, string::String, sync::Arc, vec::Vec};
use spin::{Lazy, Mutex};

const TRACING_ON: &str = "/sys/kernel/tracing/tracing_on";
const TRACE: &str = "/sys/kernel/tracing/trace";
const TRACE_MODE: &str = "/sys/kernel/tracing/trace_mode";
const MAX_ENTRIES: &str = "/sys/kernel/tracing/max_entries";

#[derive(Clone, Copy, Eq, PartialEq)]
enum TraceMode {
    List,
    Tree,
}

struct TraceEvent {
    time_us: u64,
    pid: usize,
    cpu: usize,
    function: &'static str,
}

struct TraceState {
    tracing_on: bool,
    mode: TraceMode,
    max_entries: usize,
    events: Vec<TraceEvent>,
}

static STATE: Lazy<Mutex<TraceState>> = Lazy::new(|| {
    Mutex::new(TraceState {
        tracing_on: false,
        mode: TraceMode::List,
        max_entries: 1024,
        events: Vec::new(),
    })
});

fn is_trace_path(path: &str) -> bool {
    matches!(path, TRACING_ON | TRACE | TRACE_MODE | MAX_ENTRIES)
}

fn parse_decimal(bytes: &[u8]) -> Result<usize, SysErrNo> {
    let trimmed = bytes
        .iter()
        .copied()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect::<Vec<_>>();
    if trimmed.is_empty() {
        return Err(SysErrNo::EINVAL);
    }
    let mut value = 0usize;
    for byte in trimmed {
        if !byte.is_ascii_digit() {
            return Err(SysErrNo::EINVAL);
        }
        value = value
            .checked_mul(10)
            .and_then(|value| value.checked_add((byte - b'0') as usize))
            .ok_or(SysErrNo::EINVAL)?;
    }
    Ok(value)
}

fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes[start..]
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(start, |offset| start + offset + 1);
    &bytes[start..end]
}

fn clear_events() {
    STATE.lock().events.clear();
}

/// Record a compact VFS event while tracing is enabled.
pub fn record_event(function: &'static str) {
    let Some(task) = current_task() else {
        return;
    };
    let mut state = STATE.lock();
    let event_limit = match state.mode {
        TraceMode::List => (state.max_entries / 2).max(1),
        TraceMode::Tree => state.max_entries.max(1),
    };
    if !state.tracing_on || state.events.len() >= event_limit {
        return;
    }
    let frequency = get_clock_freq().max(1) as u64;
    let time_us = (get_ticks() as u64).saturating_mul(1_000_000) / frequency;
    state.events.push(TraceEvent {
        time_us,
        pid: task.pid(),
        cpu: hart_id(),
        function,
    });
}

fn render_trace() -> Vec<u8> {
    let state = STATE.lock();
    let mut output = String::new();
    match state.mode {
        TraceMode::List => {
            output.push_str("mode: list\nTIME_US PID CPU FUNCTION EVENT\n");
            for event in &state.events {
                output.push_str(&format!(
                    "{} {} {} {} ENTER\n{} {} {} {} EXIT\n",
                    event.time_us,
                    event.pid,
                    event.cpu,
                    event.function,
                    event.time_us,
                    event.pid,
                    event.cpu,
                    event.function
                ));
            }
        }
        TraceMode::Tree => {
            output.push_str("mode: tree\n");
            for event in &state.events {
                output.push_str(&format!(
                    "PID-{} CPU {}\n  duration_us {} {}\n",
                    event.pid, event.cpu, 1, event.function
                ));
            }
        }
    }
    output.into_bytes()
}

fn render_value(path: &str) -> Vec<u8> {
    let state = STATE.lock();
    match path {
        TRACING_ON => format!("{}\n", usize::from(state.tracing_on)).into_bytes(),
        TRACE_MODE => match state.mode {
            TraceMode::List => b"list\n".to_vec(),
            TraceMode::Tree => b"tree\n".to_vec(),
        },
        MAX_ENTRIES => format!("{}\n", state.max_entries).into_bytes(),
        TRACE => Vec::new(),
        _ => Vec::new(),
    }
}

fn apply_write(path: &str, bytes: &[u8]) -> Result<(), SysErrNo> {
    let value = trim_ascii(bytes);
    let mut state = STATE.lock();
    match path {
        TRACING_ON => match parse_decimal(value)? {
            0 => state.tracing_on = false,
            1 => state.tracing_on = true,
            _ => return Err(SysErrNo::EINVAL),
        },
        TRACE_MODE => {
            state.mode = match value {
                b"list" => TraceMode::List,
                b"tree" => TraceMode::Tree,
                _ => return Err(SysErrNo::EINVAL),
            };
            let event_limit = match state.mode {
                TraceMode::List => (state.max_entries / 2).max(1),
                TraceMode::Tree => state.max_entries.max(1),
            };
            if state.events.len() > event_limit {
                state.events.truncate(event_limit);
            }
        }
        MAX_ENTRIES => {
            state.max_entries = parse_decimal(value)?.max(1);
            let max_entries = state.max_entries;
            let event_limit = match state.mode {
                TraceMode::List => (max_entries / 2).max(1),
                TraceMode::Tree => max_entries,
            };
            if state.events.len() > event_limit {
                state.events.truncate(event_limit);
            }
        }
        TRACE => state.events.clear(),
        _ => return Err(SysErrNo::ENOENT),
    }
    Ok(())
}

struct TracingFileInner {
    offset: usize,
    snapshot: Option<Vec<u8>>,
}

pub struct TracingFile {
    path: &'static str,
    readable: bool,
    writable: bool,
    inner: Mutex<TracingFileInner>,
}

impl TracingFile {
    pub fn open(path: &str, flags: OpenFlags) -> Result<Option<Arc<dyn File>>, SysErrNo> {
        if !is_trace_path(path) {
            return Ok(None);
        }
        // During early boot the directory entries are materialized as regular
        // placeholders. Runtime tasks use the dynamic tracefs view below.
        if current_task().is_none() {
            return Ok(None);
        }
        if flags.contains(OpenFlags::O_DIRECTORY) {
            return Err(SysErrNo::ENOTDIR);
        }
        if flags.contains(OpenFlags::O_EXCL) && flags.contains(OpenFlags::O_CREATE) {
            return Err(SysErrNo::EEXIST);
        }
        let (readable, writable) = flags.read_write();
        if path == TRACE && flags.contains(OpenFlags::O_TRUNC) {
            clear_events();
        }
        let path = match path {
            TRACING_ON => TRACING_ON,
            TRACE => TRACE,
            TRACE_MODE => TRACE_MODE,
            MAX_ENTRIES => MAX_ENTRIES,
            _ => unreachable!(),
        };
        Ok(Some(Arc::new(Self {
            path,
            readable,
            writable,
            inner: Mutex::new(TracingFileInner {
                offset: 0,
                snapshot: None,
            }),
        })))
    }

    fn content(&self) -> Vec<u8> {
        if self.path == TRACE {
            render_trace()
        } else {
            render_value(self.path)
        }
    }
}

impl File for TracingFile {
    fn readable(&self) -> bool {
        self.readable
    }

    fn writable(&self) -> bool {
        self.writable
    }

    fn read(&self, mut buf: UserBuffer) -> SyscallRet {
        let mut inner = self.inner.lock();
        if inner.snapshot.is_none() {
            inner.snapshot = Some(self.content());
        }
        let content = inner.snapshot.as_ref().unwrap();
        if inner.offset >= content.len() {
            return Ok(0);
        }
        let length = buf.len().min(content.len() - inner.offset);
        let copied = buf.write(&content[inner.offset..inner.offset + length]);
        inner.offset += copied;
        Ok(copied)
    }

    fn write(&self, mut buf: UserBuffer) -> SyscallRet {
        let bytes = buf.read(buf.len());
        apply_write(self.path, &bytes)?;
        Ok(bytes.len())
    }

    fn fstat(&self) -> Kstat {
        let size = self.content().len();
        Kstat {
            st_mode: StMode::FREG.bits() | 0o666,
            st_nlink: 1,
            st_size: size as isize,
            st_blksize: PAGE_SIZE as i32,
            ..Kstat::default()
        }
    }

    fn path(&self) -> Cow<'_, str> {
        Cow::Borrowed(self.path)
    }

    fn lseek(&self, offset: isize, whence: usize) -> SyscallRet {
        let mut inner = self.inner.lock();
        let size = inner
            .snapshot
            .as_ref()
            .map_or_else(|| self.content().len(), Vec::len);
        let base = match whence {
            SEEK_SET => 0isize,
            SEEK_CUR => inner.offset as isize,
            SEEK_END => size as isize,
            _ => return Err(SysErrNo::EINVAL),
        };
        let next = base.checked_add(offset).ok_or(SysErrNo::EINVAL)?;
        if next < 0 {
            return Err(SysErrNo::EINVAL);
        }
        inner.offset = next as usize;
        if inner.offset == 0 {
            inner.snapshot = None;
        }
        Ok(inner.offset)
    }

    fn poll(&self, events: PollEvents) -> PollEvents {
        let mut ready = PollEvents::empty();
        if events.contains(PollEvents::IN) && self.readable {
            ready |= PollEvents::IN;
        }
        if events.contains(PollEvents::OUT) && self.writable {
            ready |= PollEvents::OUT;
        }
        ready
    }
}
