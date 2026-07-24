//! sys_bpf 系统调用实现
//!
//! 参考 https://man7.org/linux/man-pages/man2/bpf.2.html
//!
//! 目前支持的 BPF 命令：
//! * BPF_MAP_CREATE   — 创建 BPF map
//! * BPF_MAP_LOOKUP_ELEM  — 查找 key
//! * BPF_MAP_UPDATE_ELEM  — 更新/插入 key-value
//! * BPF_MAP_DELETE_ELEM  — 删除 key
//! * BPF_MAP_GET_NEXT_KEY — 获取下一个 key

use log::warn;

use crate::fs::bpf::{BpfMap, BpfMapType};
use crate::fs::{FileClass, FileDescriptor, OpenFlags};
use crate::mm::{copy_from_user, copy_to_user};
use crate::task::current_task;
use crate::utils::{SysErrNo, SyscallRet};

// ---------------------------------------------------------------------------
// BPF 命令常量
// ---------------------------------------------------------------------------

const BPF_MAP_CREATE: i32 = 0;
const BPF_MAP_LOOKUP_ELEM: i32 = 1;
const BPF_MAP_UPDATE_ELEM: i32 = 2;
const BPF_MAP_DELETE_ELEM: i32 = 3;
const BPF_MAP_GET_NEXT_KEY: i32 = 4;

// Map flags
const _BPF_F_NO_PREALLOC: u32 = 1;
const _BPF_F_RDONLY: u32 = 1 << 3;
const _BPF_F_WRONLY: u32 = 1 << 4;
const _BPF_F_RDONLY_PROG: u32 = 1 << 7;
const _BPF_F_WRONLY_PROG: u32 = 1 << 8;

// BPF_ANY / BPF_NOEXIST / BPF_EXIST for update_elem
const _BPF_ANY: u64 = 0;
const _BPF_NOEXIST: u64 = 1;
const _BPF_EXIST: u64 = 2;

// ---------------------------------------------------------------------------
// bpf_attr 结构体布局（用于从用户空间读取）
// ---------------------------------------------------------------------------
//
// union bpf_attr 大小为 120 字节（Linux 6.x 64-bit）。
// 我们只需要读取足够大的缓冲区来覆盖我们支持的命令。

/// bpf_attr 最小有效大小
const BPF_ATTR_MIN_SIZE: u32 = 4; // 至少需要读取第一个 u32

/// bpf_attr 中用于 MAP_CREATE 的字段偏移
/// struct { map_type, key_size, value_size, max_entries, map_flags }
const MAP_CREATE_MIN_SIZE: u32 = 20; // 5 个 u32

/// bpf_attr 中用于 *_ELEM 操作的字段偏移
/// struct { map_fd(u32), padding(u32), key(u64), value/next_key(u64), flags(u64) }
const MAP_ELEM_MIN_SIZE: u32 = 32; // 4 + 4 + 8 + 8 + 8 = 32

// ---------------------------------------------------------------------------
// 辅助：从 uattr 缓冲区读取 u32
// ---------------------------------------------------------------------------

fn read_u32(buf: &[u8], offset: usize) -> Result<u32, SysErrNo> {
    if offset + 4 > buf.len() {
        return Err(SysErrNo::EINVAL);
    }
    Ok(u32::from_ne_bytes(
        buf[offset..offset + 4].try_into().unwrap(),
    ))
}

fn read_u64(buf: &[u8], offset: usize) -> Result<u64, SysErrNo> {
    if offset + 8 > buf.len() {
        return Err(SysErrNo::EINVAL);
    }
    Ok(u64::from_ne_bytes(
        buf[offset..offset + 8].try_into().unwrap(),
    ))
}

// ---------------------------------------------------------------------------
// sys_bpf 主入口
// ---------------------------------------------------------------------------

/// `bpf(cmd, uattr, size)` — 执行 BPF 命令。
///
/// # 参数
/// * `cmd` — BPF 命令（如 BPF_MAP_CREATE）
/// * `uattr` — 指向包含命令特定参数的联合体 bpf_attr
/// * `size` — uattr 指向的数据大小
///
/// # 返回
/// * 成功时返回 0 或新 map 的 fd
/// * 失败时返回负 errno
pub fn sys_bpf(cmd: i32, uattr: *mut u8, size: u32) -> SyscallRet {
    if size < BPF_ATTR_MIN_SIZE || uattr.is_null() {
        return Err(SysErrNo::EINVAL);
    }

    let task = current_task().unwrap();
    let process = &task.process;
    let memory_set = process.memory_set_arc();

    let attr_len = size as usize;
    let mut attr_buf = alloc::vec![0u8; attr_len];
    copy_from_user(&memory_set, uattr as usize, &mut attr_buf)?;

    match cmd {
        BPF_MAP_CREATE => bpf_map_create(&attr_buf, attr_len, &memory_set),
        BPF_MAP_LOOKUP_ELEM => bpf_map_lookup_elem(&attr_buf, attr_len, &memory_set),
        BPF_MAP_UPDATE_ELEM => bpf_map_update_elem(&attr_buf, attr_len, &memory_set),
        BPF_MAP_DELETE_ELEM => bpf_map_delete_elem(&attr_buf, attr_len, &memory_set),
        BPF_MAP_GET_NEXT_KEY => bpf_map_get_next_key(&attr_buf, attr_len, &memory_set),
        _ => {
            warn!("[sys_bpf] unsupported cmd: {}", cmd);
            Err(SysErrNo::EINVAL)
        }
    }
}

// ---------------------------------------------------------------------------
// BPF_MAP_CREATE
// ---------------------------------------------------------------------------

fn bpf_map_create(attr: &[u8], attr_len: usize, _memory_set: &crate::mm::MemorySet) -> SyscallRet {
    if attr_len < MAP_CREATE_MIN_SIZE as usize {
        return Err(SysErrNo::EINVAL);
    }

    let map_type_val = read_u32(attr, 0)?;
    let key_size = read_u32(attr, 4)?;
    let value_size = read_u32(attr, 8)?;
    let max_entries = read_u32(attr, 12)?;
    let _map_flags = read_u32(attr, 16)?;

    let map_type = BpfMapType::from_u32(map_type_val).ok_or(SysErrNo::EINVAL)?;

    let bpf_map = BpfMap::new(map_type, key_size, value_size, max_entries);

    let task = current_task().unwrap();
    let proc_inner = &task.process;
    let fd = proc_inner.fd_table.alloc_fd()?;
    proc_inner.fd_table.set(
        fd,
        FileDescriptor::new(OpenFlags::O_RDWR, FileClass::Abs(bpf_map.clone())),
    )?;
    BpfMap::register_fd(fd, &bpf_map);

    Ok(fd)
}

// ---------------------------------------------------------------------------
// BPF_MAP_LOOKUP_ELEM
// ---------------------------------------------------------------------------

fn bpf_map_lookup_elem(
    attr: &[u8],
    attr_len: usize,
    memory_set: &crate::mm::MemorySet,
) -> SyscallRet {
    if attr_len < MAP_ELEM_MIN_SIZE as usize {
        return Err(SysErrNo::EINVAL);
    }

    let map_fd = read_u32(attr, 0)? as usize;
    let key_ptr = read_u64(attr, 8)? as usize;
    let value_ptr = read_u64(attr, 16)? as usize;
    // flags at offset 24

    if key_ptr == 0 || value_ptr == 0 {
        return Err(SysErrNo::EFAULT);
    }

    let map = BpfMap::lookup_fd(map_fd)?;
    let key_size = map.key_size() as usize;

    let mut key_buf = alloc::vec![0u8; key_size];
    copy_from_user(memory_set, key_ptr, &mut key_buf)?;

    match map.lookup_elem(&key_buf) {
        Some(value) => {
            copy_to_user(memory_set, value_ptr, &value)?;
            Ok(0)
        }
        None => Err(SysErrNo::ENOENT),
    }
}

// ---------------------------------------------------------------------------
// BPF_MAP_UPDATE_ELEM
// ---------------------------------------------------------------------------

fn bpf_map_update_elem(
    attr: &[u8],
    attr_len: usize,
    memory_set: &crate::mm::MemorySet,
) -> SyscallRet {
    if attr_len < MAP_ELEM_MIN_SIZE as usize {
        return Err(SysErrNo::EINVAL);
    }

    let map_fd = read_u32(attr, 0)? as usize;
    let key_ptr = read_u64(attr, 8)? as usize;
    let value_ptr = read_u64(attr, 16)? as usize;
    let flags = read_u64(attr, 24)?;

    if key_ptr == 0 || value_ptr == 0 {
        return Err(SysErrNo::EFAULT);
    }

    let map = BpfMap::lookup_fd(map_fd)?;
    let key_size = map.key_size() as usize;
    let value_size = map.value_size() as usize;

    let mut key_buf = alloc::vec![0u8; key_size];
    copy_from_user(memory_set, key_ptr, &mut key_buf)?;

    let mut value_buf = alloc::vec![0u8; value_size];
    copy_from_user(memory_set, value_ptr, &mut value_buf)?;

    map.update_elem(&key_buf, &value_buf, flags)
}

// ---------------------------------------------------------------------------
// BPF_MAP_DELETE_ELEM
// ---------------------------------------------------------------------------

fn bpf_map_delete_elem(
    attr: &[u8],
    attr_len: usize,
    memory_set: &crate::mm::MemorySet,
) -> SyscallRet {
    if attr_len < MAP_ELEM_MIN_SIZE as usize {
        return Err(SysErrNo::EINVAL);
    }

    let map_fd = read_u32(attr, 0)? as usize;
    let key_ptr = read_u64(attr, 8)? as usize;

    if key_ptr == 0 {
        return Err(SysErrNo::EFAULT);
    }

    let map = BpfMap::lookup_fd(map_fd)?;
    let key_size = map.key_size() as usize;

    let mut key_buf = alloc::vec![0u8; key_size];
    copy_from_user(memory_set, key_ptr, &mut key_buf)?;

    map.delete_elem(&key_buf)
}

// ---------------------------------------------------------------------------
// BPF_MAP_GET_NEXT_KEY
// ---------------------------------------------------------------------------

fn bpf_map_get_next_key(
    attr: &[u8],
    attr_len: usize,
    memory_set: &crate::mm::MemorySet,
) -> SyscallRet {
    if attr_len < MAP_ELEM_MIN_SIZE as usize {
        return Err(SysErrNo::EINVAL);
    }

    let map_fd = read_u32(attr, 0)? as usize;
    let key_ptr = read_u64(attr, 8)? as usize; // input: current key (NULL = first)
    let next_key_ptr = read_u64(attr, 16)? as usize; // output: next key

    if next_key_ptr == 0 {
        return Err(SysErrNo::EFAULT);
    }

    let map = BpfMap::lookup_fd(map_fd)?;
    let key_size = map.key_size() as usize;

    let current_key = if key_ptr != 0 {
        let mut key_buf = alloc::vec![0u8; key_size];
        copy_from_user(memory_set, key_ptr, &mut key_buf)?;
        Some(key_buf)
    } else {
        None
    };

    match map.get_next_key(current_key.as_deref()) {
        Some(next_key) => {
            copy_to_user(memory_set, next_key_ptr, &next_key)?;
            Ok(0)
        }
        None => Err(SysErrNo::ENOENT),
    }
}
