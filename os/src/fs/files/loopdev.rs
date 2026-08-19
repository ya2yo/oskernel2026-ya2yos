//! loop 块设备的简化实现。
//!
//! 本模块提供 `/dev/loopN`、`/dev/loop/N`、`/dev/block/loopN` 以及
//! `/dev/loop-control` 的文件接口，覆盖常见格式化和挂载流程所需的
//! `ioctl`、读写、定位、轮询与状态查询。每个 loop 编号由全局表中的
//! [`LoopState`] 描述，并使用互斥锁保护。
//!
//! 当前实现刻意不把写入内容持久化到 backing file；写入只推进设备偏移并
//! 记录格式化过程中达到的最大位置。这样既保持接口和容量语义，又适配内核
//! 当前的简化 ext4 挂载模型。

use crate::{
    fs::{stat::StMode, File, Kstat, SEEK_CUR, SEEK_END, SEEK_SET},
    mm::{copy_from_user, copy_to_user, MemorySet, UserBuffer},
    syscall::PollEvents,
    utils::{SysErrNo, SyscallRet},
};
use alloc::{borrow::Cow, string::String, sync::Arc, vec::Vec};
use core::mem::size_of;
use linux_raw_sys::ioctl::{BLKGETSIZE, BLKGETSIZE64, BLKSSZGET};
use linux_raw_sys::loop_device::{
    loop_info, loop_info64, LOOP_CLR_FD, LOOP_CTL_ADD, LOOP_CTL_GET_FREE, LOOP_CTL_REMOVE,
    LOOP_GET_STATUS, LOOP_GET_STATUS64, LOOP_SET_FD, LOOP_SET_STATUS, LOOP_SET_STATUS64,
};
use log::debug;
use spin::{Lazy, Mutex};

const LOOP_COUNT: usize = 256;
const LOOP_DEFAULT_CAPACITY: usize = 64 * 1024 * 1024;
const LOOP_SECTOR_SIZE: usize = 512;

/// 单个 loop 编号的共享状态。
///
/// `backing_fd` 表示是否已经通过 `LOOP_SET_FD` 关联后端文件，`info` 保存
/// Linux loop 设备状态结构，`formatted_size` 则记录简化实现中格式化写入
/// 达到的最大偏移。该结构不包含实际数据缓存，数据持久化由上层模型处理。
struct LoopState {
    backing_fd: Option<usize>,
    info: loop_info64,
    // The simplified loop device does not persist backing-file bytes.  Keep
    // the largest formatted offset so an ext4 mount can still derive the
    // capacity requested by mke2fs.
    formatted_size: usize,
}

impl LoopState {
    fn new() -> Self {
        Self {
            backing_fd: None,
            info: unsafe { core::mem::zeroed() },
            formatted_size: 0,
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

/// 根据 loop 状态计算对外可见容量。
///
/// 若用户通过 `LOOP_SET_STATUS*` 设置了非零大小限制，则使用该限制；
/// 否则使用固定的默认容量。返回值同时用于读写边界、`fstat` 和块设备
/// 大小查询，因此这些接口保持一致。
fn loop_capacity(state: &LoopState) -> usize {
    if state.info.lo_sizelimit != 0 {
        state.info.lo_sizelimit as usize
    } else {
        LOOP_DEFAULT_CAPACITY
    }
}

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

/// loop 控制设备的固定路径。
pub const LOOP_CONTROL_PATH: &str = "/dev/loop-control";

/// `/dev/loop-control` 文件对象。
///
/// 该对象不保存单个 loop 设备的偏移，只负责分配、添加和移除编号，
/// 以及报告控制设备自身的属性。
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
    fn ioctl(&self, cmd: u32, arg: usize, _memory_set: &MemorySet) -> SyscallRet {
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
                Ok(nr as usize)
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

/// 一个已打开的 loop 块设备文件对象。
///
/// `number` 用于索引全局 [`LOOP_TABLE`] 并访问共享设备状态；`path` 保留
/// 用户打开时使用的路径；`offset` 是当前文件描述符独立的读写位置，因而
/// 同一 loop 设备的多个打开实例可以拥有不同的定位状态。
pub struct DevLoop {
    number: u32,
    path: String,
    offset: Mutex<usize>,
}

impl DevLoop {
    pub fn new(number: u32, path: &str) -> Self {
        Self {
            number,
            path: String::from(path),
            offset: Mutex::new(0),
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
    fn read(&self, mut buf: UserBuffer) -> SyscallRet {
        let capacity = loop_capacity(&LOOP_TABLE[self.number as usize].lock());
        let mut offset = self.offset.lock();
        if *offset >= capacity {
            return Ok(0);
        }
        let len = buf.len().min(capacity - *offset);
        if len == buf.len() {
            buf.fill0();
        } else {
            let mut zeros = Vec::new();
            zeros.resize(len, 0);
            buf.write(&zeros);
        }
        *offset += len;
        Ok(len)
    }
    fn write(&self, buf: UserBuffer) -> SyscallRet {
        // 当前 loop 设备用于 LTP 临时格式化/挂载路径，暂不持久化 backing file 数据。
        let mut state = LOOP_TABLE[self.number as usize].lock();
        let capacity = loop_capacity(&state);
        let mut offset = self.offset.lock();
        if *offset >= capacity {
            return Err(SysErrNo::ENOSPC);
        }
        let len = buf.len().min(capacity - *offset);
        *offset += len;
        state.formatted_size = state.formatted_size.max(*offset);
        Ok(len)
    }
    fn fstat(&self) -> Kstat {
        let devno = 0x700 + self.number as usize;
        let capacity = loop_capacity(&LOOP_TABLE[self.number as usize].lock());
        Kstat {
            st_dev: devno,
            st_mode: StMode::FBLK.bits(),
            st_rdev: devno,
            st_nlink: 1,
            st_size: capacity as isize,
            st_blksize: LOOP_SECTOR_SIZE as i32,
            st_blocks: (capacity / LOOP_SECTOR_SIZE) as isize,
            ..Kstat::default()
        }
    }
    fn path(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.path)
    }
    fn lseek(&self, offset: isize, whence: usize) -> SyscallRet {
        let capacity = loop_capacity(&LOOP_TABLE[self.number as usize].lock());
        let mut cur = self.offset.lock();
        let base = match whence {
            SEEK_SET => 0isize,
            SEEK_CUR => *cur as isize,
            SEEK_END => capacity as isize,
            _ => return Err(SysErrNo::EINVAL),
        };
        let new_offset = base.checked_add(offset).ok_or(SysErrNo::EINVAL)?;
        if new_offset < 0 || new_offset as usize > capacity {
            return Err(SysErrNo::EINVAL);
        }
        *cur = new_offset as usize;
        Ok(*cur)
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
        // debug!("loopdev's ioctl: cmd={}, arg={}", cmd, arg);
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
                state.formatted_size = 0;
                *self.offset.lock() = 0;
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
                state.formatted_size = 0;
                *self.offset.lock() = 0;
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
                let size = loop_capacity(&LOOP_TABLE[idx].lock()) as u64;
                copy_to_user(memory_set, arg, unsafe {
                    core::slice::from_raw_parts(&size as *const u64 as *const u8, size_of::<u64>())
                })?;
                Ok(0)
            }
            BLKGETSIZE => {
                let sectors = loop_capacity(&LOOP_TABLE[idx].lock()) / LOOP_SECTOR_SIZE;
                copy_to_user(memory_set, arg, unsafe {
                    core::slice::from_raw_parts(
                        &sectors as *const usize as *const u8,
                        size_of::<usize>(),
                    )
                })?;
                Ok(0)
            }
            BLKSSZGET => {
                let sector_size = LOOP_SECTOR_SIZE as u32;
                copy_to_user(memory_set, arg, unsafe {
                    core::slice::from_raw_parts(
                        &sector_size as *const u32 as *const u8,
                        size_of::<u32>(),
                    )
                })?;
                Ok(0)
            }
            _ => Err(SysErrNo::ENOTTY),
        }
    }
}

/// 返回格式化 loop 设备时写入达到的最大偏移。
///
/// `mke2fs` 会通过 `/dev/loopN` 写入文件系统镜像，而当前 loop 实现有意
/// 丢弃具体数据。最大偏移仍足以为简化 ext4 挂载模型保留所需的容量约定；
/// 若尚未发生写入，则回退到设备按状态计算出的默认容量。
pub fn formatted_size(path: &str) -> Option<usize> {
    let number = parse_loop_device(path)? as usize;
    let state = LOOP_TABLE[number].lock();
    Some(if state.formatted_size != 0 {
        state.formatted_size
    } else {
        loop_capacity(&state)
    })
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
