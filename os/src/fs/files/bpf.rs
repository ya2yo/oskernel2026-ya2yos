//! BPF Map 文件对象实现
//!
//! 支持 BPF_MAP_TYPE_HASH 和 BPF_MAP_TYPE_ARRAY 两种 map 类型。
//! Map 通过文件描述符引用，操作（lookup/update/delete/get_next_key）通过 sys_bpf 系统调用。

use alloc::collections::BTreeMap;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use spin::{Lazy, Mutex};

use super::super::{File, Kstat};
use crate::mm::UserBuffer;
use crate::syscall::PollEvents;
use crate::utils::{SysErrNo, SysResult, SyscallRet};

// ---------------------------------------------------------------------------
// BPF Map 类型常量
// ---------------------------------------------------------------------------

/// BPF map types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum BpfMapType {
    Unspec = 0,
    Hash = 1,
    Array = 2,
    // 更多类型以后扩展
}

impl BpfMapType {
    pub fn from_u32(v: u32) -> Option<Self> {
        match v {
            1 => Some(Self::Hash),
            2 => Some(Self::Array),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// BPF Map 结构
// ---------------------------------------------------------------------------

/// BPF Map 对象，同时实现 File trait 以便 fd 表管理。
///
/// 内部使用 BTreeMap<Vec<u8>, Vec<u8>> 存储键值对。
/// 对于 ARRAY 类型，key 必须是 4 字节的小端表示，用于索引检查。
pub struct BpfMap {
    /// Map 类型
    map_type: BpfMapType,
    /// Key 大小（字节）
    key_size: u32,
    /// Value 大小（字节）
    value_size: u32,
    /// 最大条目数
    max_entries: u32,
    /// 内部键值存储
    inner: Mutex<BpfMapInner>,
}

struct BpfMapInner {
    entries: BTreeMap<Vec<u8>, Vec<u8>>,
}

// 全局 fd → BpfMap 映射，用于 sys_bpf 操作时根据 fd 查找 map
static BPF_MAP_TABLE: Lazy<Mutex<BTreeMap<usize, Weak<BpfMap>>>> =
    Lazy::new(|| Mutex::new(BTreeMap::new()));

impl BpfMap {
    /// 创建一个新的 BPF map
    pub fn new(
        map_type: BpfMapType,
        key_size: u32,
        value_size: u32,
        max_entries: u32,
    ) -> Arc<Self> {
        // key_size 和 value_size 至少为 1（除特殊类型外），max_entries 至少为 1
        let key_size = key_size.max(1);
        let value_size = value_size.max(1);
        let max_entries = max_entries.max(1);
        Arc::new(Self {
            map_type,
            key_size,
            value_size,
            max_entries,
            inner: Mutex::new(BpfMapInner {
                entries: BTreeMap::new(),
            }),
        })
    }

    /// 注册 fd → BpfMap 映射
    pub fn register_fd(fd: usize, map: &Arc<BpfMap>) {
        BPF_MAP_TABLE.lock().insert(fd, Arc::downgrade(map));
    }

    /// 根据 fd 查找 BpfMap
    pub fn lookup_fd(fd: usize) -> Result<Arc<BpfMap>, SysErrNo> {
        BPF_MAP_TABLE
            .lock()
            .get(&fd)
            .and_then(|w| w.upgrade())
            .ok_or(SysErrNo::EBADF)
    }

    /// 清理失效的弱引用
    pub fn cleanup_fd(fd: usize) {
        BPF_MAP_TABLE.lock().remove(&fd);
    }

    /// 获取 map 类型
    pub fn map_type(&self) -> BpfMapType {
        self.map_type
    }

    /// 获取 key 大小
    pub fn key_size(&self) -> u32 {
        self.key_size
    }

    /// 获取 value 大小
    pub fn value_size(&self) -> u32 {
        self.value_size
    }

    /// 获取最大条目数
    pub fn max_entries(&self) -> u32 {
        self.max_entries
    }

    // -----------------------------------------------------------------------
    // Map 操作
    // -----------------------------------------------------------------------

    /// 查找 key 对应的 value。
    ///
    /// 返回 `Some(value)` 如果找到（或 ARRAY map 的预分配零值）。
    /// 返回 `None` 如果 key 不存在（对应 ENOENT）。
    pub fn lookup_elem(&self, key: &[u8]) -> Option<Vec<u8>> {
        let inner = self.inner.lock();
        match inner.entries.get(key) {
            Some(v) => Some(v.clone()),
            None if self.map_type == BpfMapType::Array => {
                // ARRAY map 预分配：有效索引未设置时返回零值
                if key.len() == 4 {
                    let index = u32::from_ne_bytes(key[..4].try_into().unwrap());
                    if index < self.max_entries {
                        return Some(alloc::vec![0u8; self.value_size as usize]);
                    }
                }
                None
            }
            None => None,
        }
    }

    /// 更新或插入 key-value 对。
    ///
    /// 返回 `Ok(())` 成功。
    /// 返回 `Err(EBUSY)` 如果 map 已满且不是更新已有 key。
    /// 返回 `Err(E2BIG)` 如果 key/value 大小不匹配。
    /// 返回 `Err(EINVAL)` 对于 ARRAY map 的非法索引。
    pub fn update_elem(&self, key: &[u8], value: &[u8], _flags: u64) -> SyscallRet {
        if key.len() != self.key_size as usize {
            return Err(SysErrNo::E2BIG);
        }
        if value.len() != self.value_size as usize {
            return Err(SysErrNo::E2BIG);
        }

        // ARRAY map: 检查 key 是否是有效的 u32 索引
        if self.map_type == BpfMapType::Array {
            if key.len() != 4 {
                return Err(SysErrNo::EINVAL);
            }
            let index = u32::from_ne_bytes(key[..4].try_into().unwrap());
            if index >= self.max_entries {
                return Err(SysErrNo::EINVAL);
            }
        }

        let mut inner = self.inner.lock();
        // ARRAY map 预分配，不检查条目数上限
        if self.map_type != BpfMapType::Array {
            let is_new = !inner.entries.contains_key(key);
            if is_new && inner.entries.len() >= self.max_entries as usize {
                return Err(SysErrNo::EBUSY);
            }
        }
        inner.entries.insert(key.to_vec(), value.to_vec());
        Ok(0)
    }

    /// 删除 key 对应的条目。
    ///
    /// 返回 `Ok(())` 成功（包括 key 不存在的情况，这符合 Linux 语义）。
    pub fn delete_elem(&self, key: &[u8]) -> SyscallRet {
        if key.len() != self.key_size as usize {
            return Err(SysErrNo::E2BIG);
        }
        let mut inner = self.inner.lock();
        inner.entries.remove(key);
        Ok(0)
    }

    /// 获取下一个 key（用于迭代）。
    ///
    /// `current_key` 为 `None` 表示获取第一个 key。
    /// 返回 `Some(key_bytes)` 如果找到下一个 key。
    /// 返回 `None` 表示没有更多 key（对应 ENOENT）。
    pub fn get_next_key(&self, current_key: Option<&[u8]>) -> Option<Vec<u8>> {
        let inner = self.inner.lock();
        if inner.entries.is_empty() {
            return None;
        }

        if let Some(cur) = current_key {
            // 找到当前 key 后面的下一个
            let mut found = false;
            for k in inner.entries.keys() {
                if found {
                    return Some(k.clone());
                }
                if k.as_slice() == cur {
                    found = true;
                }
            }
            None // 当前 key 是最后一个
        } else {
            // 返回第一个 key
            inner.entries.keys().next().cloned()
        }
    }

    /// 获取当前条目数量
    pub fn count(&self) -> usize {
        self.inner.lock().entries.len()
    }
}

// ---------------------------------------------------------------------------
// File trait 实现
// ---------------------------------------------------------------------------

impl File for BpfMap {
    fn readable(&self) -> bool {
        true
    }

    fn writable(&self) -> bool {
        true
    }

    fn read(&self, _buf: UserBuffer) -> SyscallRet {
        Err(SysErrNo::EINVAL)
    }

    fn write(&self, _buf: UserBuffer) -> SyscallRet {
        Err(SysErrNo::EINVAL)
    }

    fn fstat(&self) -> Kstat {
        Kstat::default()
    }

    fn poll(&self, _events: PollEvents) -> PollEvents {
        PollEvents::empty()
    }

    fn set_nonblocking(&self, _nonblocking: bool) -> SysResult {
        Ok(())
    }

    fn nonblocking(&self) -> bool {
        false
    }
}
