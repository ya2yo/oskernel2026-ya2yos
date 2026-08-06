//! 密钥管理子系统（key management facility）
//!
//! 实现 add_key(2), keyctl(2), request_key(2) 系统调用。
//! 使用全局 BTreeMap 存储密钥条目，按序列号索引。
//!
//! 支持的密钥类型:
//!   - "keyring": 密钥环（payload 必须为空）
//!   - "user":    用户自定义密钥（payload ≤ 32767 字节）
//!
//! 支持的 keyctl 命令:
//!   - KEYCTL_GET_KEYRING_ID (0)
//!   - KEYCTL_JOIN_SESSION_KEYRING (1)
//!   - KEYCTL_REVOKE (3)
//!   - KEYCTL_READ (11)
//!   - KEYCTL_UNLINK (9)
//!   - KEYCTL_INVALIDATE (21)

use alloc::vec;
use alloc::{collections::btree_map::BTreeMap, format, string::String, sync::Arc, vec::Vec};
use log::{debug, warn};
use spin::{Lazy, Mutex};

use crate::fs::File;
use crate::mm::read_user_cstr;
use crate::task::current_task;
use crate::utils::{SysErrNo, SyscallRet};

// ---------------------------------------------------------------------------
// keyctl 命令常量
// ---------------------------------------------------------------------------
const KEYCTL_GET_KEYRING_ID: usize = 0;
const KEYCTL_JOIN_SESSION_KEYRING: usize = 1;
const KEYCTL_UPDATE: usize = 2;
const KEYCTL_REVOKE: usize = 3;
const KEYCTL_SETPERM: usize = 5;
const KEYCTL_CLEAR: usize = 7;
const KEYCTL_UNLINK: usize = 9;
const KEYCTL_READ: usize = 11;
const KEYCTL_SET_REQKEY_KEYRING: usize = 14;
const KEYCTL_SET_TIMEOUT: usize = 15;
const KEYCTL_INVALIDATE: usize = 21;
const KEYCTL_WATCH_KEY: usize = 32;

const WATCH_TYPE_KEY_NOTIFY: u32 = 1;
const NOTIFY_KEY_UPDATED: u8 = 1;
const KEY_NOTIFICATION_LEN: u32 = 16;

// KEY_SPEC 特殊值
const KEY_SPEC_THREAD_KEYRING: i32 = -1;
const KEY_SPEC_PROCESS_KEYRING: i32 = -2;
const KEY_SPEC_SESSION_KEYRING: i32 = -3;
const KEY_SPEC_USER_KEYRING: i32 = -4;
const KEY_SPEC_USER_SESSION_KEYRING: i32 = -5;

// ---------------------------------------------------------------------------
// 密钥数据结构
// ---------------------------------------------------------------------------

/// 单个密钥条目
struct KeyEntry {
    /// 密钥类型: "user" 或 "keyring"
    key_type: String,
    /// 密钥描述/名称
    description: String,
    /// 密钥负载数据
    payload: Vec<u8>,
    /// 权限位掩码
    _perm: u32,
    /// 是否已吊销
    revoked: bool,
    /// 是否已失效
    invalidated: bool,
}

#[derive(Clone)]
struct KeyWatcher {
    watch_id: u8,
    file: Arc<dyn File>,
}

/// 全局密钥数据库: 序列号 → 密钥条目
static KEY_STORE: Lazy<Mutex<BTreeMap<i32, KeyEntry>>> = Lazy::new(|| Mutex::new(BTreeMap::new()));

/// key serial → notification pipe watchers.
static KEY_WATCHERS: Lazy<Mutex<BTreeMap<i32, Vec<KeyWatcher>>>> =
    Lazy::new(|| Mutex::new(BTreeMap::new()));

/// 全局密钥序列号计数器，从 100 开始（避免与特殊值冲突）
static NEXT_SERIAL: Lazy<Mutex<i32>> = Lazy::new(|| Mutex::new(100));

/// 分配一个新的密钥序列号
fn alloc_serial() -> i32 {
    let mut s = NEXT_SERIAL.lock();
    let ret = *s;
    *s += 1;
    ret
}

/// 为特殊 keyring ID（KEY_SPEC_*）确保存在对应的密钥环条目
/// 返回该特殊 ID 对应的序列号
fn ensure_keyring(special_id: i32) -> i32 {
    let mut store = KEY_STORE.lock();
    if let Some(entry) = store.get(&special_id) {
        if !entry.revoked && !entry.invalidated {
            return special_id;
        }
    }
    // 创建一个新的密钥环
    let serial = alloc_serial();
    store.insert(
        serial,
        KeyEntry {
            key_type: String::from("keyring"),
            description: format!("keyring_{}", serial),
            payload: Vec::new(),
            _perm: 0x3f3f0000,
            revoked: false,
            invalidated: false,
        },
    );
    debug!("[keys] created keyring serial={}", serial);
    serial
}

/// 解析特殊 keyring ID，若非特殊 ID 则直接返回原值
fn resolve_keyring(id: i32) -> i32 {
    match id {
        KEY_SPEC_THREAD_KEYRING
        | KEY_SPEC_PROCESS_KEYRING
        | KEY_SPEC_SESSION_KEYRING
        | KEY_SPEC_USER_KEYRING
        | KEY_SPEC_USER_SESSION_KEYRING => ensure_keyring(id),
        _ => id,
    }
}

fn emit_key_notification(key_id: i32, subtype: u8) {
    let watchers = {
        let watchers = KEY_WATCHERS.lock();
        watchers.get(&key_id).cloned().unwrap_or_default()
    };

    if watchers.is_empty() {
        return;
    }

    for watcher in watchers {
        let mut record = [0u8; KEY_NOTIFICATION_LEN as usize];
        let type_subtype = WATCH_TYPE_KEY_NOTIFY | ((subtype as u32) << 24);
        let info = KEY_NOTIFICATION_LEN | ((watcher.watch_id as u32) << 8);
        record[0..4].copy_from_slice(&type_subtype.to_le_bytes());
        record[4..8].copy_from_slice(&info.to_le_bytes());
        record[8..12].copy_from_slice(&(key_id as u32).to_le_bytes());
        record[12..16].copy_from_slice(&0u32.to_le_bytes());

        if let Err(err) = watcher.file.write_kernel_bytes(&record) {
            warn!(
                "[keyctl] failed to emit watch notification key={} err={:?}",
                key_id, err
            );
        }
    }
}

/// 参考 https://man7.org/linux/man-pages/man2/add_key.2.html
pub fn sys_add_key(
    key_type: *const u8,
    desc: *const u8,
    payload: *const u8,
    plen: usize,
    keyring: isize,
) -> SyscallRet {
    let task = current_task().unwrap();
    let proc_inner = &task.process;
    let memory_set = proc_inner.memory_set_arc();

    let type_str = if !key_type.is_null() {
        read_user_cstr(&memory_set, key_type)?
    } else {
        return Err(SysErrNo::EFAULT);
    };

    debug!(
        "[add_key] type={}, desc={:#x}, payload={:#x}, plen={}, keyring={}",
        type_str, desc as usize, payload as usize, plen, keyring
    );

    // 翻译 description
    let desc_str = if !desc.is_null() {
        read_user_cstr(&memory_set, desc)?
    } else {
        String::new()
    };

    // add_key02 回归测试: NULL payload 配合非零长度应返回 EFAULT
    if payload.is_null() && plen > 0 {
        debug!("[add_key] NULL payload with non-zero length -> EFAULT");
        return Err(SysErrNo::EFAULT);
    }

    // 根据密钥类型进行基本校验
    let max_plen = match type_str.as_str() {
        "keyring" => {
            if plen > 0 {
                debug!("[add_key] keyring with non-zero plen -> EINVAL");
                return Err(SysErrNo::EINVAL);
            }
            0
        }
        "user" => {
            if plen > 32767 {
                debug!("[add_key] user key plen {} > 32767 -> EINVAL", plen);
                return Err(SysErrNo::EINVAL);
            }
            32767
        }
        "logon" | "big_key" | "asymmetric" | "cifs.idmap" | "cifs.spnego" | "pkcs7_test"
        | "rxrpc" | "rxrpc_s" => {
            debug!("[add_key] unsupported key type '{}' -> ENODEV", type_str);
            return Err(SysErrNo::ENODEV);
        }
        _ => {
            debug!("[add_key] unknown key type '{}' -> ENODEV", type_str);
            return Err(SysErrNo::ENODEV);
        }
    };

    // 从用户空间复制 payload
    let payload_data = if plen > 0 && !payload.is_null() {
        let mut buf = vec![0u8; plen.min(max_plen)];
        crate::mm::copy_from_user(&memory_set, payload as usize, &mut buf)?;
        buf
    } else {
        Vec::new()
    };

    // 解析目标 keyring（确保存在）
    let _target_ring = resolve_keyring(keyring as i32);

    // 分配序列号并插入数据库
    let serial = alloc_serial();
    let mut store = KEY_STORE.lock();
    store.insert(
        serial,
        KeyEntry {
            key_type: type_str,
            description: desc_str,
            payload: payload_data,
            _perm: 0x3f3f0000,
            revoked: false,
            invalidated: false,
        },
    );
    drop(store);

    debug!("[add_key] success, serial={}", serial);
    Ok(serial as usize)
}

/// 参考 https://man7.org/linux/man-pages/man2/request_key.2.html
pub fn sys_request_key(
    _key_type: *const u8,
    _description: *const u8,
    _callout_info: *const u8,
    _dest_ring: isize,
) -> SyscallRet {
    // 伪实现：直接分配一个新的密钥序列号
    let serial = alloc_serial();
    debug!("[request_key] stub, assigned serial={}", serial);
    Ok(serial as usize)
}

/// 参考 https://man7.org/linux/man-pages/man2/keyctl.2.html
pub fn sys_keyctl(option: usize, arg2: usize, arg3: usize, arg4: usize, arg5: usize) -> SyscallRet {
    debug!(
        "[keyctl] option={}, arg2={}, arg3={}, arg4={}, arg5={}",
        option, arg2, arg3, arg4, arg5
    );

    match option {
        KEYCTL_GET_KEYRING_ID => {
            // arg2: special keyring ID (KEY_SPEC_*)
            // arg3: create flag (1 = create if not exist)
            let id = arg2 as i32;
            let serial = resolve_keyring(id);
            debug!("[keyctl] GET_KEYRING_ID id={} -> serial={}", id, serial);
            Ok(serial as usize)
        }

        KEYCTL_JOIN_SESSION_KEYRING => {
            // arg2: name (or NULL for anonymous session)
            let serial = ensure_keyring(KEY_SPEC_SESSION_KEYRING);
            debug!("[keyctl] JOIN_SESSION_KEYRING -> serial={}", serial);
            Ok(serial as usize)
        }

        KEYCTL_UPDATE => {
            // arg2: key serial
            // arg3: payload pointer
            // arg4: payload length
            let key_id = arg2 as i32;
            let plen = arg4;
            let mut store = KEY_STORE.lock();
            let entry = match store.get_mut(&key_id) {
                Some(e) => {
                    if e.revoked {
                        return Err(SysErrNo::EKEYREVOKED);
                    }
                    e
                }
                None => return Err(SysErrNo::ENOKEY),
            };

            if plen > 0 {
                let task = current_task().unwrap();
                let proc_inner = &task.process;
                let memory_set = proc_inner.memory_set_arc();
                let mut buf = vec![0u8; plen];
                crate::mm::copy_from_user(&memory_set, arg3, &mut buf)?;
                entry.payload = buf;
            } else {
                entry.payload.clear();
            }
            debug!("[keyctl] UPDATE key={} plen={}", key_id, plen);
            drop(store);
            emit_key_notification(key_id, NOTIFY_KEY_UPDATED);
            Ok(0)
        }

        KEYCTL_WATCH_KEY => {
            let key_id = resolve_keyring(arg2 as i32);
            let fd = arg3;
            let watch_id = arg4 as u8;

            {
                let store = KEY_STORE.lock();
                if !store.contains_key(&key_id) {
                    debug!("[keyctl] WATCH_KEY key={} -> ENOKEY", key_id);
                    return Err(SysErrNo::ENOKEY);
                }
            }

            let task = current_task().unwrap();
            let proc_inner = &task.process;
            let file = proc_inner.fd_table.get(fd)?.any();

            let mut watchers = KEY_WATCHERS.lock();
            watchers
                .entry(key_id)
                .or_default()
                .push(KeyWatcher { watch_id, file });
            debug!(
                "[keyctl] WATCH_KEY key={} fd={} watch_id={}",
                key_id, fd, watch_id
            );
            Ok(0)
        }

        KEYCTL_REVOKE => {
            let key_id = arg2 as i32;
            let mut store = KEY_STORE.lock();
            match store.get_mut(&key_id) {
                Some(entry) => {
                    entry.revoked = true;
                    entry.payload.clear();
                    debug!("[keyctl] REVOKE key={}", key_id);
                    Ok(0)
                }
                None => {
                    debug!("[keyctl] REVOKE key={} -> ENOKEY", key_id);
                    Err(SysErrNo::ENOKEY)
                }
            }
        }

        KEYCTL_SETPERM => {
            let key_id = arg2 as i32;
            let perm = arg3 as u32;
            let mut store = KEY_STORE.lock();
            match store.get_mut(&key_id) {
                Some(entry) => {
                    entry._perm = perm;
                    debug!("[keyctl] SETPERM key={} perm={:#x}", key_id, perm);
                    Ok(0)
                }
                None => Err(SysErrNo::ENOKEY),
            }
        }

        KEYCTL_CLEAR => {
            let ring_id = arg2 as i32;
            let ring = resolve_keyring(ring_id);
            // 清除密钥环：删除所有非 revoked 条目
            let mut store = KEY_STORE.lock();
            let to_remove: Vec<i32> = store
                .iter()
                .filter(|(_, e)| !e.revoked && !e.invalidated)
                .map(|(&k, _)| k)
                .collect();
            for k in to_remove {
                // 保留 keyring 本身
                if k != ring {
                    store.remove(&k);
                }
            }
            debug!("[keyctl] CLEAR ring={}", ring);
            Ok(0)
        }

        KEYCTL_UNLINK => {
            let key_id = arg2 as i32;
            let ring_id = arg3 as i32;
            let mut store = KEY_STORE.lock();
            match store.remove(&key_id) {
                Some(_) => {
                    debug!("[keyctl] UNLINK key={} from ring={}", key_id, ring_id);
                    Ok(0)
                }
                None => {
                    // 密钥不存在也不算错误（Linux 语义）
                    debug!("[keyctl] UNLINK key={} not found -> OK", key_id);
                    Ok(0)
                }
            }
        }

        KEYCTL_READ => {
            let key_id = arg2 as i32;
            let buf_addr = arg3;
            let buf_len = arg4;
            let store = KEY_STORE.lock();
            let entry = match store.get(&key_id) {
                Some(e) => e,
                None => {
                    debug!("[keyctl] READ key={} -> ENOKEY", key_id);
                    return Err(SysErrNo::ENOKEY);
                }
            };
            if entry.revoked {
                debug!("[keyctl] READ key={} -> EKEYREVOKED", key_id);
                return Err(SysErrNo::EKEYREVOKED);
            }

            let data = &entry.payload;
            let copy_len = data.len().min(buf_len);
            if copy_len > 0 && buf_addr != 0 {
                let task = current_task().unwrap();
                let proc_inner = &task.process;
                let memory_set = proc_inner.memory_set_arc();
                crate::mm::copy_to_user(&memory_set, buf_addr, &data[..copy_len])?;
            }
            debug!("[keyctl] READ key={} len={}", key_id, entry.payload.len());
            Ok(entry.payload.len())
        }

        KEYCTL_SET_REQKEY_KEYRING => {
            // 伪实现：接受但不做实际操作
            warn!("[keyctl] SET_REQKEY_KEYRING stub");
            Ok(0)
        }

        KEYCTL_SET_TIMEOUT => {
            // 伪实现：接受但不做实际操作
            debug!("[keyctl] SET_TIMEOUT stub");
            Ok(0)
        }

        KEYCTL_INVALIDATE => {
            let key_id = arg2 as i32;
            let mut store = KEY_STORE.lock();
            match store.get_mut(&key_id) {
                Some(entry) => {
                    entry.invalidated = true;
                    entry.payload.clear();
                    debug!("[keyctl] INVALIDATE key={}", key_id);
                    Ok(0)
                }
                None => {
                    debug!("[keyctl] INVALIDATE key={} -> ENOKEY", key_id);
                    Err(SysErrNo::ENOKEY)
                }
            }
        }

        _ => {
            warn!("[keyctl] unsupported option {} -> EOPNOTSUPP", option);
            Err(SysErrNo::EOPNOTSUPP)
        }
    }
}
