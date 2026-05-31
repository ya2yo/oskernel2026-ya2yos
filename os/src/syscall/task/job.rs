use log::{debug, warn};

use crate::utils::{SysErrNo, SyscallRet};
use crate::mm::translated_str;
use crate::task::current_task;
use spin::{Lazy, Mutex};

/// 参考 https://man7.org/linux/man-pages/man2/setsid.2.html
pub fn sys_setsid() -> SyscallRet {
    warn!("[sys_setsid] We do not really support process group!");
    Ok(0)
}

pub fn sys_getpgid() -> SyscallRet {
    warn!("[sys_getpgid] We do not really support process group!");
    Ok(0)
}

/// https://www.man7.org/linux/man-pages/man2/setpgid.2.html
pub fn sys_setpgid(_pid: u32, _pgid: u32) -> SyscallRet {
    // debug!("[sys_setpgid] start!");
    warn!("[sys_setpgid] unimplement!");
    Ok(0)
}

/// 伪密钥序列号计数器
static NEXT_KEY_SERIAL: Lazy<Mutex<i32>> = Lazy::new(|| Mutex::new(1));

/// 参考 https://man7.org/linux/man-pages/man2/add_key.2.html
pub fn sys_add_key(
    key_type: *const u8,
    desc: *const u8,
    payload: *const u8,
    plen: usize,
    keyring: isize,
) -> SyscallRet {
    let task = current_task().unwrap();
    let proc_inner = task.process.inner_lock();
    let token = proc_inner.get_locked_memory_set_read().token();

    // 翻译 type 和 description 字符串用于检查
    let type_str = if !key_type.is_null() {
        translated_str(token, key_type)
    } else {
        return Err(SysErrNo::EFAULT);
    };

    debug!(
        "[sys_add_key] type={}, desc_addr={:#x}, payload_addr={:#x}, plen={}, keyring={}",
        type_str, desc as usize, payload as usize, plen, keyring
    );

    // add_key02 回归测试: NULL payload 配合非零长度应返回 EFAULT
    if payload.is_null() && plen > 0 {
        debug!("[sys_add_key] NULL payload with non-zero length -> EFAULT");
        return Err(SysErrNo::EFAULT);
    }

    // 根据密钥类型进行基本校验
    match type_str.as_str() {
        "keyring" => {
            // keyring 类型的 payload 长度必须为 0
            if plen > 0 {
                debug!("[sys_add_key] keyring with non-zero plen -> EINVAL");
                return Err(SysErrNo::EINVAL);
            }
        }
        "user" => {
            // user 类型的 payload 最大长度为 32767
            if plen > 32767 {
                debug!("[sys_add_key] user key plen {} > 32767 -> EINVAL", plen);
                return Err(SysErrNo::EINVAL);
            }
        }
        "logon" | "big_key" | "asymmetric" | "cifs.idmap" | "cifs.spnego"
        | "pkcs7_test" | "rxrpc" | "rxrpc_s" => {
            // 这些类型在我们的内核中不受支持，返回 ENODEV
            debug!("[sys_add_key] unsupported key type '{}' -> ENODEV", type_str);
            return Err(SysErrNo::ENODEV);
        }
        _ => {
            // 未知类型也返回 ENODEV
            debug!("[sys_add_key] unknown key type '{}' -> ENODEV", type_str);
            return Err(SysErrNo::ENODEV);
        }
    }

    // 分配一个伪密钥序列号
    let serial = {
        let mut s = NEXT_KEY_SERIAL.lock();
        let ret = *s;
        *s += 1;
        ret
    };

    debug!("[sys_add_key] success, assigned serial {}", serial);
    Ok(serial as usize)
}
