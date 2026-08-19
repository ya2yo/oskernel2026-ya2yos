//! POSIX 消息队列 (mqueue) 文件类型实现
//!
//! 实现 File trait，支持通过 fd 表管理。

use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use spin::{Lazy, Mutex};

use super::super::File;
use crate::mm::UserBuffer;
use crate::syscall::PollEvents;
use crate::utils::{SysErrNo, SysResult, SyscallRet};

/// 队列中保存的一条消息。
///
/// 消息数据按发送时的字节序列保存，优先级目前记录在对象中，具体的
/// 排序/调度由上层消息队列系统调用负责。
struct MqMessage {
    data: Vec<u8>,
    prio: u32,
}

/// POSIX 消息队列文件对象。
///
/// 名称用于注册表查找，队列内容及属性由 `inner` 统一保护。
pub struct Mqueue {
    /// 队列名称（例如 `/myqueue`）。
    name: Mutex<String>,
    /// 内部可变状态
    inner: Mutex<MqueueInner>,
}

struct MqueueInner {
    /// 消息队列
    queue: VecDeque<MqMessage>,
    /// 最大消息数
    maxmsg: usize,
    /// 单条消息最大字节数
    msgsize: usize,
    /// 是否已标记删除 (mq_unlink 后新 mq_open 不可见，但已有 fd 仍可用)
    unlinked: bool,
    /// 是否为非阻塞模式
    nonblocking: bool,
}

// 全局注册表
/// fd → Mqueue 映射（用于 mq_send/mq_receive 查找）
static MQUEUE_TABLE: Lazy<Mutex<BTreeMap<usize, Weak<Mqueue>>>> =
    Lazy::new(|| Mutex::new(BTreeMap::new()));

/// 名称 → Mqueue 映射（用于 mq_open 查找已存在的队列）
pub static NAME_REGISTRY: Lazy<Mutex<BTreeMap<String, Arc<Mqueue>>>> =
    Lazy::new(|| Mutex::new(BTreeMap::new()));

// Mqueue 实现
impl Mqueue {
    /// 创建一个新的消息队列
    pub fn new(name: String, maxmsg: usize, msgsize: usize) -> Arc<Self> {
        Arc::new(Self {
            name: Mutex::new(name),
            inner: Mutex::new(MqueueInner {
                queue: VecDeque::new(),
                maxmsg: if maxmsg == 0 { 10 } else { maxmsg },
                msgsize: if msgsize == 0 { 8192 } else { msgsize },
                unlinked: false,
                nonblocking: false,
            }),
        })
    }

    /// 注册 fd → Mqueue 映射
    pub fn register_fd(fd: usize, mq: &Arc<Mqueue>) {
        MQUEUE_TABLE.lock().insert(fd, Arc::downgrade(mq));
    }

    /// 根据 fd 查找 Mqueue
    pub fn lookup(fd: usize) -> Result<Arc<Mqueue>, SysErrNo> {
        MQUEUE_TABLE
            .lock()
            .get(&fd)
            .and_then(|w| w.upgrade())
            .ok_or(SysErrNo::EBADF)
    }

    /// 清理失效的弱引用
    pub fn cleanup_fd(fd: usize) {
        MQUEUE_TABLE.lock().remove(&fd);
    }

    /// 获取队列属性
    pub fn get_attr(&self) -> MqAttr {
        let inner = self.inner.lock();
        MqAttr {
            mq_flags: if inner.nonblocking { O_NONBLOCK } else { 0 },
            mq_maxmsg: inner.maxmsg as u64,
            mq_msgsize: inner.msgsize as u64,
            mq_curmsgs: inner.queue.len() as u64,
        }
    }

    /// 标记为已删除
    pub fn mark_unlinked(&self) {
        self.inner.lock().unlinked = true;
    }

    /// 从名称注册表中移除
    pub fn unlink_name(name: &str) -> bool {
        if let Some(mq) = NAME_REGISTRY.lock().remove(name) {
            mq.mark_unlinked();
            true
        } else {
            false
        }
    }

    /// 发送消息 (非阻塞)
    /// 返回 Ok(()) 成功，Err(ETIMEDOUT) 表示队列满（非阻塞模式下）
    pub fn try_send(&self, msg_data: &[u8], prio: u32) -> SyscallRet {
        let mut inner = self.inner.lock();
        if inner.queue.len() >= inner.maxmsg {
            return Err(SysErrNo::EAGAIN);
        }
        if msg_data.len() > inner.msgsize {
            return Err(SysErrNo::EMSGSIZE);
        }
        inner.queue.push_back(MqMessage {
            data: msg_data.to_vec(),
            prio,
        });
        Ok(0)
    }

    /// 接收消息 (非阻塞)
    /// 返回 Ok(实际长度)，Err(EAGAIN) 表示队列空
    pub fn try_receive(&self, buf: &mut [u8]) -> SyscallRet {
        let mut inner = self.inner.lock();
        if inner.queue.is_empty() {
            return Err(SysErrNo::EAGAIN);
        }
        let msg = inner.queue.pop_front().unwrap();
        let to_copy = core::cmp::min(buf.len(), msg.data.len());
        buf[..to_copy].copy_from_slice(&msg.data[..to_copy]);
        Ok(msg.data.len())
    }

    /// 设置非阻塞模式
    pub fn set_nonblocking(&self, nb: bool) {
        self.inner.lock().nonblocking = nb;
    }
}

/// 消息队列属性 (对应 struct mq_attr)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MqAttr {
    pub mq_flags: i64,   // 标志: 0 或 O_NONBLOCK
    pub mq_maxmsg: u64,  // 最大消息数
    pub mq_msgsize: u64, // 每条消息最大字节数
    pub mq_curmsgs: u64, // 当前队列中消息数
}

pub const O_NONBLOCK: i64 = 2048;
impl File for Mqueue {
    fn readable(&self) -> bool {
        // 队列非空时可读
        !self.inner.lock().queue.is_empty()
    }

    fn writable(&self) -> bool {
        true
    }

    fn read(&self, _buf: UserBuffer) -> SyscallRet {
        // mqueue 不使用标准 read，应使用 mq_receive
        Err(SysErrNo::EINVAL)
    }

    fn write(&self, _buf: UserBuffer) -> SyscallRet {
        // mqueue 不使用标准 write，应使用 mq_send
        Err(SysErrNo::EINVAL)
    }

    fn poll(&self, events: PollEvents) -> PollEvents {
        let mut revents = PollEvents::empty();
        let inner = self.inner.lock();
        if events.contains(PollEvents::IN) && !inner.queue.is_empty() {
            revents |= PollEvents::IN;
        }
        if events.contains(PollEvents::OUT) && inner.queue.len() < inner.maxmsg {
            revents |= PollEvents::OUT;
        }
        revents
    }

    fn nonblocking(&self) -> bool {
        self.inner.lock().nonblocking
    }

    fn set_nonblocking(&self, nb: bool) -> SysResult {
        self.inner.lock().nonblocking = nb;
        Ok(())
    }
}
