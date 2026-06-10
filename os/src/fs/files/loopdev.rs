use crate::{
    fs::{stat::StMode, File, Kstat},
    mm::{copy_from_user, copy_to_user, MemorySet, UserBuffer},
    syscall::PollEvents,
    utils::{SysErrNo, SyscallRet},
};
use alloc::{borrow::Cow, string::String, sync::Arc, vec::Vec};
use core::mem::size_of;
use linux_raw_sys::ioctl::BLKGETSIZE64;
use linux_raw_sys::loop_device::{
    loop_info, loop_info64, LOOP_CLR_FD, LOOP_CTL_ADD, LOOP_CTL_GET_FREE, LOOP_CTL_REMOVE,
    LOOP_GET_STATUS, LOOP_GET_STATUS64, LOOP_SET_FD, LOOP_SET_STATUS, LOOP_SET_STATUS64,
};
use spin::{Lazy, Mutex};

const LOOP_COUNT: usize = 256;

struct LoopState {
    backing_fd: Option<usize>,
    info: loop_info64,
}

impl LoopState {
    fn new() -> Self {
        Self {
            backing_fd: None,
            info: unsafe { core::mem::zeroed() },
        }
    }
}

/// 用 Vec 逐个 push 初始化，避开 `core::array::from_fn` 在栈上构造
/// [Mutex<LoopState>; 256] (~66KB) 导致的内核栈溢出
static LOOP_TABLE: Lazy<Vec<Mutex<LoopState>>> = Lazy::new(|| {
    let mut v = Vec::with_capacity(LOOP_COUNT);
    for _ in 0..LOOP_COUNT {
        v.push(Mutex::new(LoopState::new()));
    }
    v
});

/// 解析 loop 设备路径：/dev/loopN、/dev/loop/N、/dev/block/loopN
pub fn parse_loop_device(path: &str) -> Option<u32> {
    const PREFIXES: [&str; 3] = ["/dev/loop", "/dev/loop/", "/dev/block/loop"];
    for prefix in PREFIXES {
        if let Some(suffix) = path.strip_prefix(prefix) {
            if suffix.is_empty() {
                return None;
            }
            if let Ok(num) = suffix.parse::<u32>() {
                if (num as usize) < LOOP_COUNT {
                    return Some(num);
                }
            }
        }
    }
    None
}

pub const LOOP_CONTROL_PATH: &str = "/dev/loop-control";

pub struct DevLoopControl;

impl DevLoopControl {
    pub fn open() -> Arc<Self> {
        Arc::new(Self)
    }
}

impl File for DevLoopControl {
    fn readable(&self) -> bool {
        true
    }
    fn writable(&self) -> bool {
        true
    }
    fn fstat(&self) -> Kstat {
        Kstat {
            st_dev: 0x7f0,
            st_mode: StMode::FCHR.bits(),
            st_rdev: 0x7f0,
            st_nlink: 1,
            ..Kstat::default()
        }
    }
    fn path(&self) -> Cow<'_, str> {
        Cow::Borrowed(LOOP_CONTROL_PATH)
    }
    fn poll(&self, events: PollEvents) -> PollEvents {
        let mut revents = PollEvents::empty();
        if events.contains(PollEvents::IN) {
            revents |= PollEvents::IN;
        }
        if events.contains(PollEvents::OUT) {
            revents |= PollEvents::OUT;
        }
        revents
    }
    fn ioctl(&self, cmd: u32, arg: usize, memory_set: &MemorySet) -> SyscallRet {
        match cmd {
            LOOP_CTL_GET_FREE => {
                let mut free_nr = None;
                for i in 0..LOOP_COUNT {
                    if LOOP_TABLE[i].lock().backing_fd.is_none() {
                        free_nr = Some(i as u32);
                        break;
                    }
                }
                let nr = free_nr.ok_or(SysErrNo::EOPNOTSUPP)?;
                copy_to_user(memory_set, arg, unsafe {
                    core::slice::from_raw_parts(&nr as *const u32 as *const u8, size_of::<u32>())
                })?;
                Ok(0)
            }
            LOOP_CTL_ADD => {
                if arg >= LOOP_COUNT as usize {
                    return Err(SysErrNo::EINVAL);
                }
                Ok(0)
            }
            LOOP_CTL_REMOVE => {
                if arg >= LOOP_COUNT {
                    return Err(SysErrNo::EINVAL);
                }
                if LOOP_TABLE[arg].lock().backing_fd.is_some() {
                    return Err(SysErrNo::EBUSY);
                }
                Ok(0)
            }
            _ => Err(SysErrNo::ENOTTY),
        }
    }
}

pub struct DevLoop {
    number: u32,
    path: String,
}

impl DevLoop {
    pub fn new(number: u32, path: &str) -> Self {
        Self {
            number,
            path: String::from(path),
        }
    }

    pub fn open(number: u32, path: &str) -> Arc<Self> {
        Arc::new(Self::new(number, path))
    }
}

impl File for DevLoop {
    fn readable(&self) -> bool {
        true
    }
    fn writable(&self) -> bool {
        true
    }
    fn read(&self, _buf: UserBuffer) -> SyscallRet {
        // loop 块设备，暂不实现数据中转到 backing file
        Ok(0)
    }
    fn write(&self, buf: UserBuffer) -> SyscallRet {
        // 吞掉所有写入，避免 unimplemented! panic
        Ok(buf.len())
    }
    fn fstat(&self) -> Kstat {
        let devno = 0x700 + self.number as usize;
        Kstat {
            st_dev: devno,
            st_mode: StMode::FBLK.bits(),
            st_rdev: devno,
            st_nlink: 1,
            ..Kstat::default()
        }
    }
    fn path(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.path)
    }
    fn poll(&self, events: PollEvents) -> PollEvents {
        let mut revents = PollEvents::empty();
        if events.contains(PollEvents::IN) {
            revents |= PollEvents::IN;
        }
        if events.contains(PollEvents::OUT) {
            revents |= PollEvents::OUT;
        }
        revents
    }
    fn ioctl(&self, cmd: u32, arg: usize, memory_set: &MemorySet) -> SyscallRet {
        let idx = self.number as usize;
        match cmd {
            LOOP_GET_STATUS64 => {
                let state = LOOP_TABLE[idx].lock();
                if state.backing_fd.is_none() {
                    return Err(SysErrNo::ENXIO);
                }
                let mut info = state.info;
                info.lo_number = self.number;
                copy_to_user(memory_set, arg, unsafe {
                    core::slice::from_raw_parts(
                        &info as *const loop_info64 as *const u8,
                        size_of::<loop_info64>(),
                    )
                })?;
                Ok(0)
            }
            LOOP_SET_FD => {
                let mut state = LOOP_TABLE[idx].lock();
                state.backing_fd = Some(arg);
                Ok(0)
            }
            LOOP_CLR_FD => {
                let mut state = LOOP_TABLE[idx].lock();
                if state.backing_fd.is_none() {
                    return Err(SysErrNo::ENXIO);
                }
                state.backing_fd = None;
                state.info = unsafe { core::mem::zeroed() };
                state.info.lo_number = self.number;
                Ok(0)
            }
            LOOP_SET_STATUS64 => {
                let mut info: loop_info64 = unsafe { core::mem::zeroed() };
                copy_from_user(memory_set, arg, unsafe {
                    core::slice::from_raw_parts_mut(
                        &mut info as *mut loop_info64 as *mut u8,
                        size_of::<loop_info64>(),
                    )
                })?;
                let mut state = LOOP_TABLE[idx].lock();
                state.info = info;
                state.info.lo_number = self.number;
                Ok(0)
            }
            // 兼容 32 位 loop_info (LOOP_SET_STATUS / LOOP_GET_STATUS)
            LOOP_SET_STATUS => {
                let mut li: loop_info = unsafe { core::mem::zeroed() };
                copy_from_user(memory_set, arg, unsafe {
                    core::slice::from_raw_parts_mut(
                        &mut li as *mut loop_info as *mut u8,
                        size_of::<loop_info>(),
                    )
                })?;
                let mut state = LOOP_TABLE[idx].lock();
                state.info = loop_info_to_info64(&li);
                state.info.lo_number = self.number;
                Ok(0)
            }
            LOOP_GET_STATUS => {
                let state = LOOP_TABLE[idx].lock();
                if state.backing_fd.is_none() {
                    return Err(SysErrNo::ENXIO);
                }
                let li = info64_to_loop_info(&state.info, self.number);
                copy_to_user(memory_set, arg, unsafe {
                    core::slice::from_raw_parts(
                        &li as *const loop_info as *const u8,
                        size_of::<loop_info>(),
                    )
                })?;
                Ok(0)
            }
            BLKGETSIZE64 => {
                let size: u64 = LOOP_TABLE[idx].lock().info.lo_sizelimit;
                copy_to_user(memory_set, arg, unsafe {
                    core::slice::from_raw_parts(&size as *const u64 as *const u8, size_of::<u64>())
                })?;
                Ok(0)
            }
            _ => Err(SysErrNo::ENOTTY),
        }
    }
}

/// 将 32 位 loop_info 转换为内部使用的 loop_info64
fn loop_info_to_info64(li: &loop_info) -> loop_info64 {
    let mut info: loop_info64 = unsafe { core::mem::zeroed() };
    info.lo_offset = li.lo_offset as u64;
    info.lo_number = li.lo_number as u32;
    info.lo_encrypt_type = li.lo_encrypt_type as u32;
    info.lo_encrypt_key_size = li.lo_encrypt_key_size as u32;
    info.lo_flags = li.lo_flags as u32;
    info.lo_file_name[..li.lo_name.len()].copy_from_slice(unsafe {
        core::slice::from_raw_parts(li.lo_name.as_ptr() as *const u8, li.lo_name.len())
    }); // c_char→u8: 目标 lo_file_name 固定是 [u8]，强转安全
    info.lo_encrypt_key.copy_from_slice(&li.lo_encrypt_key);
    info.lo_init[0] = li.lo_init[0] as u64;
    info.lo_init[1] = li.lo_init[1] as u64;
    info
}

/// 将内部 loop_info64 转换为 32 位 loop_info
fn info64_to_loop_info(info: &loop_info64, number: u32) -> loop_info {
    let mut li: loop_info = unsafe { core::mem::zeroed() };
    li.lo_number = number as i32;
    li.lo_offset = info.lo_offset as i32;
    li.lo_encrypt_type = info.lo_encrypt_type as i32;
    li.lo_encrypt_key_size = info.lo_encrypt_key_size as i32;
    li.lo_flags = info.lo_flags as i32;
    li.lo_name[..info.lo_file_name.len()].copy_from_slice(unsafe {
        core::slice::from_raw_parts(
            info.lo_file_name.as_ptr() as *const _, // c_char 在 riscv64 是 u8, loongarch64 是 i8
            info.lo_file_name.len(),
        )
    });
    li.lo_encrypt_key.copy_from_slice(&info.lo_encrypt_key);
    li.lo_init[0] = info.lo_init[0] as u64;
    li.lo_init[1] = info.lo_init[1] as u64;
    li
}
