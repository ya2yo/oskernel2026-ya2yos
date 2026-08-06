//! System V message queues.
//!
//! The queue table is intentionally kept in the IPC syscall module for now:
//! message queues are kernel-global objects and are not file descriptors.
//! User pointers are copied before entering a potentially blocking wait, and
//! the queue locks are never held while touching user memory or scheduling.

use alloc::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
    vec,
    vec::Vec,
};
use core::{future::poll_fn, task::Poll};

use spin::{Lazy, Mutex};

use crate::{
    mm::{copy_from_user, copy_from_user_val, copy_to_user, copy_to_user_val},
    task::{block_on, current_task, interruptible},
    timer::realtime,
    utils::{PollSet, SysErrNo, SysResult, SyscallRet},
};

/// `msgget` 的 key 值：0 表示私有队列，必然新建
const IPC_PRIVATE: i32 = 0;
/// 队列不存在时创建（与 IPC_EXCL 结合使用）
const IPC_CREAT: i32 = 0o1000;
/// 队列已存在时报 EEXIST（须与 IPC_CREAT 同时使用）
const IPC_EXCL: i32 = 0o2000;
/// 非阻塞标志：队列满/空时立即返回 EAGAIN/ENOMSG
const IPC_NOWAIT: i32 = 0o4000;

/// `msgctl` 命令：删除队列
const IPC_RMID: i32 = 0;
/// `msgctl` 命令：更新队列权限、uid/gid 与 msg_qbytes
const IPC_SET: i32 = 1;
/// `msgctl` 命令：读取队列状态到 struct msqid64_ds
const IPC_STAT: i32 = 2;
/// `msgctl` 命令：读取全局 msginfo，返回值是最大已用队列 id
const IPC_INFO: i32 = 3;
/// `msgctl` 命令：按 id 读取队列状态，返回值是该队列 id（不校验读权限）
const MSG_STAT: i32 = 11;
/// `msgctl` 命令：读取全局 msginfo，返回值是最大已用队列 id
const MSG_INFO: i32 = 12;
/// `msgctl` 命令：按 id 读取队列状态，返回队列 id，且不校验权限
const MSG_STAT_ANY: i32 = 13;

/// `msgrcv` 标志：消息长度超过 msgsz 时截断而不是报 E2BIG
const MSG_NOERROR: i32 = 0o10000;
/// `msgrcv` 标志：msgtyp > 0 时读取第一条类型不同的消息
const MSG_EXCEPT: i32 = 0o20000;
/// `msgrcv` 标志：按数组下标拷贝消息而不从队列移除（须与 IPC_NOWAIT 同用）
const MSG_COPY: i32 = 0o40000;

/// Linux 默认值。`msg_qbytes` 可通过 IPC_SET 调低，非特权调用者受 MSGMNB 限制。
const MSGMNI: usize = 32_000;
const MSGMAX: usize = 8192;
const MSGMNB: usize = 16_384;

/// 内核侧的队列权限元数据，对应 Linux `struct ipc_perm` 的内核表示。
#[derive(Clone, Copy)]
struct IpcPerm {
    key: i32,
    uid: u32,
    gid: u32,
    cuid: u32,
    cgid: u32,
    mode: u32,
    seq: i32,
}

/// 队列中的一条消息：`kind` 为消息类型（mtype），`text` 为消息正文（mtext）。
#[derive(Clone)]
struct Message {
    kind: i64,
    text: Vec<u8>,
}

/// 单条消息队列的可变状态，由 [`MsgQueue::state`] 的互斥锁保护。
struct QueueState {
    perm: IpcPerm,
    /// 消息本体，按入队顺序排列
    messages: VecDeque<Message>,
    /// 队列中消息正文的总字节数
    bytes: usize,
    /// 队列字节上限（默认 MSGMNB，可用 IPC_SET 调整）
    qbytes: usize,
    /// 最后一次 msgsnd 的时间（秒），0 表示尚未发生过
    stime: usize,
    /// 最后一次 msgrcv 的时间（秒），0 表示尚未发生过
    rtime: usize,
    /// 队列最近一次创建或 IPC_SET 的时间（秒）
    ctime: usize,
    /// 最后一个发送消息的进程 pid
    lspid: u32,
    /// 最后一个接收消息的进程 pid
    lrpid: u32,
    /// IPC_RMID 后置位；置位后阻塞中的收发方会以 EIDRM 唤醒
    removed: bool,
}

/// 一条消息队列：状态加等待集合。
struct MsgQueue {
    state: Mutex<QueueState>,
    /// 因队列为空而阻塞的接收者
    recv_wait: PollSet,
    /// 因队列满而阻塞的发送者
    send_wait: PollSet,
}

/// 全局消息队列管理器，负责 id 分配、id → 队列、key → id 三张表。
struct MsgManager {
    next_id: i32,
    queues: BTreeMap<i32, Arc<MsgQueue>>,
    keys: BTreeMap<i32, i32>,
}

impl MsgManager {
    fn new() -> Self {
        Self {
            next_id: 1,
            queues: BTreeMap::new(),
            keys: BTreeMap::new(),
        }
    }
}

/// 全局唯一的管理器实例（kernel-global，非 fd 对象）。
static MSG_MANAGER: Lazy<Mutex<MsgManager>> = Lazy::new(|| Mutex::new(MsgManager::new()));

/// 用户态 `struct ipc_perm::mode` 的宽度与架构相关：riscv64 为 u16，loongarch64 为 u32。
#[cfg(target_arch = "riscv64")]
type UserMode = u16;
#[cfg(target_arch = "loongarch64")]
type UserMode = u32;

/// 用户态 `struct ipc_perm` 的镜像，按架构保留 C ABI 布局与 padding。
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct UserIpcPerm {
    key: i32,
    uid: u32,
    gid: u32,
    cuid: u32,
    cgid: u32,
    #[cfg(target_arch = "riscv64")]
    mode: UserMode,
    #[cfg(target_arch = "riscv64")]
    pad1: u16,
    #[cfg(target_arch = "riscv64")]
    seq: u16,
    #[cfg(target_arch = "riscv64")]
    pad2: u16,
    #[cfg(target_arch = "loongarch64")]
    mode: UserMode,
    #[cfg(target_arch = "loongarch64")]
    seq: i32,
    unused1: usize,
    unused2: usize,
}

/// 用户态 `struct msqid64_ds` 的镜像（IPC_STAT/IPC_SET 的缓冲布局）。
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct UserMsqidDs {
    msg_perm: UserIpcPerm,
    msg_stime: usize,
    msg_rtime: usize,
    msg_ctime: usize,
    msg_cbytes: usize,
    msg_qnum: usize,
    msg_qbytes: usize,
    msg_lspid: u32,
    msg_lrpid: u32,
    pad1: usize,
    pad2: usize,
}

/// 用户态 `struct msginfo` 的镜像（IPC_INFO/MSG_INFO 的输出）。
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct UserMsgInfo {
    msgpool: i32,
    msgmap: i32,
    msgmax: i32,
    msgmnb: i32,
    msgmni: i32,
    msgssz: i32,
    msgtql: i32,
    msgseg: u16,
}

/// 当前实时时钟的秒数，用作队列时间戳。
fn now() -> usize {
    realtime().tv_sec
}

/// 读取当前任务的 (effective uid, effective gid, pid)，用于权限检查与记账。
fn current_credentials() -> SysResult<(u32, u32, u32)> {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let inner = task.inner_lock();
    Ok((inner.effective_uid, inner.effective_gid, task.pid() as u32))
}

/// 是否拥有特权：目前等价于 root（uid == 0）。
fn has_cap_or_root(uid: u32) -> bool {
    uid == 0
}

/// 按 IPC 权限位（owner/group/other 三组 rwx 位）检查读/写访问。
///
/// 特权用户直接放行；否则按 uid/gid 选取对应权限组，
/// 同时要求 `!read || 有读位` 且 `!write || 有写位`。
fn access_allowed(perm: IpcPerm, uid: u32, gid: u32, read: bool, write: bool) -> bool {
    if has_cap_or_root(uid) {
        return true;
    }
    let bits = if uid == perm.uid {
        (perm.mode >> 6) & 7
    } else if gid == perm.gid {
        (perm.mode >> 3) & 7
    } else {
        perm.mode & 7
    };
    (!read || bits & 4 != 0) && (!write || bits & 2 != 0)
}

/// 是否对该队列拥有管理权：root、当前 owner 或创建者。
fn owner_allowed(perm: IpcPerm, uid: u32) -> bool {
    has_cap_or_root(uid) || uid == perm.uid || uid == perm.cuid
}

/// 按 msqid 取出队列；id 非法返回 EINVAL。
fn get_queue(msqid: i32) -> SysResult<Arc<MsgQueue>> {
    if msqid < 0 {
        return Err(SysErrNo::EINVAL);
    }
    MSG_MANAGER
        .lock()
        .queues
        .get(&msqid)
        .cloned()
        .ok_or(SysErrNo::EINVAL)
}

/// 新建一条队列并返回其 id。
///
/// 队列总数达到 MSGMNI 时报 ENOSPC；非私有 key 会登记到 key → id 表，
/// 权限模式取自 `msgflg` 的低 9 位。持 MSG_MANAGER 锁期间不触碰用户内存。
fn create_queue(key: i32, msgflg: i32, uid: u32, gid: u32) -> SyscallRet {
    let mut manager = MSG_MANAGER.lock();
    if manager.queues.len() >= MSGMNI {
        return Err(SysErrNo::ENOSPC);
    }
    let id = manager.next_id;
    manager.next_id = manager.next_id.checked_add(1).ok_or(SysErrNo::ENOSPC)?;
    let perm = IpcPerm {
        key,
        uid,
        gid,
        cuid: uid,
        cgid: gid,
        mode: (msgflg as u32) & 0o777,
        seq: 0,
    };
    let queue = Arc::new(MsgQueue {
        state: Mutex::new(QueueState {
            perm,
            messages: VecDeque::new(),
            bytes: 0,
            qbytes: MSGMNB,
            stime: 0,
            rtime: 0,
            ctime: now(),
            lspid: 0,
            lrpid: 0,
            removed: false,
        }),
        recv_wait: PollSet::new(),
        send_wait: PollSet::new(),
    });
    manager.queues.insert(id, queue);
    if key != IPC_PRIVATE {
        manager.keys.insert(key, id);
    }
    Ok(id as usize)
}

/// 向用户地址写入字节串（供 msgrcv 回填 mtype + mtext）。
fn write_user_bytes(ptr: *mut u8, data: &[u8]) -> SyscallRet {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let memory_set = task.process.memory_set_arc();
    copy_to_user(&memory_set, ptr as usize, data)
}

/// 从用户地址读取一个任意大小的值。
fn read_user_value<T: Sized>(ptr: *const u8) -> SysResult<T> {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let memory_set = task.process.memory_set_arc();
    copy_from_user_val(&memory_set, ptr as *const T)
}

/// 向用户地址写入一个任意大小的值。
fn write_user_value<T: Sized>(ptr: *mut u8, value: &T) -> SysResult {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let memory_set = task.process.memory_set_arc();
    copy_to_user_val(&memory_set, ptr as *mut T, value)
}

/// 由队列状态生成用户态 `msqid64_ds` 快照（IPC_STAT 输出）。
fn queue_snapshot(state: &QueueState) -> UserMsqidDs {
    let p = state.perm;
    let result = UserMsqidDs {
        msg_perm: UserIpcPerm {
            key: p.key,
            uid: p.uid,
            gid: p.gid,
            cuid: p.cuid,
            cgid: p.cgid,
            mode: p.mode as UserMode,
            unused1: 0,
            unused2: 0,
            ..UserIpcPerm::default()
        },
        msg_stime: state.stime,
        msg_rtime: state.rtime,
        msg_ctime: state.ctime,
        msg_cbytes: state.bytes,
        msg_qnum: state.messages.len(),
        msg_qbytes: state.qbytes,
        msg_lspid: state.lspid,
        msg_lrpid: state.lrpid,
        pad1: 0,
        pad2: 0,
    };
    result
}

/// 汇总全部队列生成 `msginfo`（IPC_INFO/MSG_INFO 输出），并返回最大已用队列 id。
fn queue_info() -> UserMsgInfo {
    let manager = MSG_MANAGER.lock();
    let mut messages = 0usize;
    let mut bytes = 0usize;
    for queue in manager.queues.values() {
        let state = queue.state.lock();
        messages += state.messages.len();
        bytes += state.bytes;
    }
    UserMsgInfo {
        msgpool: manager.queues.len() as i32,
        msgmap: messages as i32,
        msgmax: MSGMAX as i32,
        msgmnb: MSGMNB as i32,
        msgmni: MSGMNI as i32,
        msgssz: 1,
        msgtql: bytes as i32,
        msgseg: 0xffff,
    }
}

/// 在队列中查找符合 msgtyp 规则的消息下标，规则与 Linux 一致：
///
/// - `MSG_COPY`：按数组下标取，下标非负才有效；
/// - `msgtyp == 0`：取队首第一条；
/// - `msgtyp > 0`：取第一条 `kind == msgtyp` 的消息（`MSG_EXCEPT` 时取第一条
///   `kind != msgtyp` 的）；
/// - `msgtyp < 0`：取第一条 `kind <= -msgtyp` 中 kind 最小的消息。
fn find_message(state: &QueueState, msgtyp: i64, flags: i32) -> Option<usize> {
    if flags & MSG_COPY != 0 {
        return if msgtyp < 0 {
            None
        } else {
            state.messages.get(msgtyp as usize).map(|_| msgtyp as usize)
        };
    }
    if msgtyp == 0 {
        return (!state.messages.is_empty()).then_some(0);
    }
    if msgtyp > 0 {
        return state
            .messages
            .iter()
            .enumerate()
            .find(|(_, message)| {
                if flags & MSG_EXCEPT != 0 {
                    message.kind != msgtyp
                } else {
                    message.kind == msgtyp
                }
            })
            .map(|(index, _)| index);
    }

    let limit = msgtyp.saturating_neg();
    state
        .messages
        .iter()
        .enumerate()
        .filter(|(_, message)| message.kind <= limit)
        .min_by_key(|(index, message)| (message.kind, *index))
        .map(|(index, _)| index)
}

/// 非阻塞尝试入队一条消息。
///
/// 队列已被删除返回 `Err(EIDRM)`；空间不足返回 `Ok(false)`（调用方决定
/// 立即报错还是阻塞等待）；成功入队时更新 `stime`/`lspid`。
fn try_send(state: &mut QueueState, message: &Message, pid: u32) -> Result<bool, SysErrNo> {
    if state.removed {
        return Err(SysErrNo::EIDRM);
    }
    let size = message.text.len();
    if size > state.qbytes.saturating_sub(state.bytes) || state.messages.len() >= state.qbytes {
        return Ok(false);
    }
    state.bytes += size;
    state.messages.push_back(message.clone());
    state.stime = now();
    state.lspid = pid;
    Ok(true)
}

/// 非阻塞尝试取出一条符合 msgtyp 规则的消息。
///
/// 队列已被删除返回 `Err(EIDRM)`；没有匹配消息返回 `Ok(None)`；消息过长且
/// 未设 `MSG_NOERROR` 返回 `Err(E2BIG)`；`MSG_COPY` 模式只拷贝不移除，
/// 其余模式移除消息并更新 `rtime`/`lrpid`。
fn try_receive(
    state: &mut QueueState,
    msgtyp: i64,
    msgflg: i32,
    msgsz: usize,
    pid: u32,
) -> Result<Option<Message>, SysErrNo> {
    if state.removed {
        return Err(SysErrNo::EIDRM);
    }
    let Some(index) = find_message(state, msgtyp, msgflg) else {
        return Ok(None);
    };
    let message = &state.messages[index];
    if msgflg & MSG_COPY == 0 && message.text.len() > msgsz && msgflg & MSG_NOERROR == 0 {
        return Err(SysErrNo::E2BIG);
    }
    if msgflg & MSG_COPY != 0 {
        return Ok(Some(Message {
            kind: message.kind,
            text: message.text.clone(),
        }));
    }
    let message = state.messages.remove(index).ok_or(SysErrNo::EIDRM)?;
    state.bytes = state.bytes.saturating_sub(message.text.len());
    state.rtime = now();
    state.lrpid = pid;
    Ok(Some(message))
}

/// 阻塞式发送：队列满时挂起当前任务等待空间。
///
/// 等待期间不持锁；挂起前先注册 waker，唤醒后二次尝试，避免丢失通知。
/// 被信号打断返回 EINTR。
fn wait_send(queue: Arc<MsgQueue>, message: Message, pid: u32) -> SyscallRet {
    let result = block_on(interruptible(poll_fn(move |cx| {
        let mut state = queue.state.lock();
        match try_send(&mut state, &message, pid) {
            Ok(true) => Poll::Ready(Ok(())),
            Ok(false) => {
                drop(state);
                queue.send_wait.register(cx.waker());
                let mut state = queue.state.lock();
                match try_send(&mut state, &message, pid) {
                    Ok(true) => Poll::Ready(Ok(())),
                    Ok(false) => Poll::Pending,
                    Err(error) => Poll::Ready(Err(error)),
                }
            }
            Err(error) => Poll::Ready(Err(error)),
        }
    })));
    match result {
        Ok(value) => value.map(|_| 0),
        Err(_) => Err(SysErrNo::EINTR),
    }
}

/// 阻塞式接收：队列无匹配消息时挂起当前任务等待。
///
/// 等待期间不持锁；挂起前先注册 waker，唤醒后二次尝试，避免丢失通知。
/// 被信号打断返回 EINTR。
fn wait_receive(
    queue: Arc<MsgQueue>,
    msgtyp: i64,
    msgflg: i32,
    msgsz: usize,
    pid: u32,
) -> SysResult<Message> {
    let result = block_on(interruptible(poll_fn(move |cx| {
        let mut state = queue.state.lock();
        match try_receive(&mut state, msgtyp, msgflg, msgsz, pid) {
            Ok(Some(message)) => Poll::Ready(Ok(message)),
            Ok(None) => {
                drop(state);
                queue.recv_wait.register(cx.waker());
                let mut state = queue.state.lock();
                match try_receive(&mut state, msgtyp, msgflg, msgsz, pid) {
                    Ok(Some(message)) => Poll::Ready(Ok(message)),
                    Ok(None) => Poll::Pending,
                    Err(error) => Poll::Ready(Err(error)),
                }
            }
            Err(error) => Poll::Ready(Err(error)),
        }
    })));
    match result {
        Ok(value) => value,
        Err(_) => Err(SysErrNo::EINTR),
    }
}

/// 参考 https://www.man7.org/linux/man-pages/man2/msgget.2.html
/// 获取或创建 System V 消息队列。
///
/// - key 为 IPC_PRIVATE 时总是新建；
/// - 队列已存在：IPC_CREAT|IPC_EXCL 报 EEXIST，无读/写权限报 EACCES，
///   否则直接返回既有 id；
/// - 队列不存在：未带 IPC_CREAT 报 ENOENT，否则新建（总数达到 MSGMNI 报 ENOSPC）。
pub fn sys_msgget(key: i32, msgflg: i32) -> SyscallRet {
    let (uid, gid, _) = current_credentials()?;
    if key != IPC_PRIVATE {
        let existing = MSG_MANAGER.lock().keys.get(&key).copied();
        if let Some(msqid) = existing {
            let queue = get_queue(msqid)?;
            let state = queue.state.lock();
            if msgflg & IPC_CREAT != 0 && msgflg & IPC_EXCL != 0 {
                return Err(SysErrNo::EEXIST);
            }
            let read = (msgflg & 0o444) != 0;
            let write = (msgflg & 0o222) != 0;
            if !access_allowed(state.perm, uid, gid, read, write) {
                return Err(SysErrNo::EACCES);
            }
            return Ok(msqid as usize);
        }
        if msgflg & IPC_CREAT == 0 {
            return Err(SysErrNo::ENOENT);
        }
    }
    create_queue(key, msgflg, uid, gid)
}

/// 参考 https://www.man7.org/linux/man-pages/man2/msgsnd.2.html
/// 向消息队列发送消息。
///
/// `msgp` 指向 `long mtype + char mtext[msgsz]`；mtype 必须为正（否则 EINVAL），
/// 正文长度超过 MSGMAX 报 EINVAL。队列满时：IPC_NOWAIT 报 EAGAIN，
/// 否则阻塞等待空间；队列被删除时报 EIDRM。
pub fn sys_msgsnd(msqid: i32, msgp: *const u8, msgsz: usize, msgflg: i32) -> SyscallRet {
    if msgsz > MSGMAX {
        return Err(SysErrNo::EINVAL);
    }
    let queue = get_queue(msqid)?;
    let (uid, gid, pid) = current_credentials()?;
    {
        let state = queue.state.lock();
        if !access_allowed(state.perm, uid, gid, false, true) {
            return Err(SysErrNo::EACCES);
        }
    }
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let memory_set = task.process.memory_set_arc();
    let kind = copy_from_user_val::<usize>(&memory_set, msgp as *const usize)? as i64;
    if kind <= 0 {
        return Err(SysErrNo::EINVAL);
    }
    let text_addr = (msgp as usize)
        .checked_add(core::mem::size_of::<usize>())
        .ok_or(SysErrNo::EFAULT)?;
    let mut text = vec![0; msgsz];
    copy_from_user(&memory_set, text_addr, &mut text)?;
    let message = Message { kind, text };
    let mut state = queue.state.lock();
    match try_send(&mut state, &message, pid)? {
        true => {
            drop(state);
            queue.recv_wait.wake();
            Ok(0)
        }
        false if msgflg & IPC_NOWAIT != 0 => Err(SysErrNo::EAGAIN),
        false => {
            drop(state);
            wait_send(queue, message, pid)
        }
    }
}

/// 参考 https://www.man7.org/linux/man-pages/man2/msgrcv.2.html
/// 从消息队列接收消息。
///
/// 向用户缓冲区写 `long mtype + mtext`，返回正文长度；消息过长且未设
/// MSG_NOERROR 报 E2BIG；MSG_COPY 须与 IPC_NOWAIT 同用且不得与 MSG_EXCEPT
/// 同用（否则 EINVAL）；无匹配消息时 IPC_NOWAIT 报 ENOMSG，否则阻塞等待。
pub fn sys_msgrcv(msqid: i32, msgp: *mut u8, msgsz: usize, msgtyp: i64, msgflg: i32) -> SyscallRet {
    if msgsz > MSGMAX
        || (msgflg & MSG_COPY != 0 && msgflg & IPC_NOWAIT == 0)
        || (msgflg & MSG_COPY != 0 && msgflg & MSG_EXCEPT != 0)
    {
        return Err(SysErrNo::EINVAL);
    }
    let queue = get_queue(msqid)?;
    let (uid, gid, pid) = current_credentials()?;
    {
        let state = queue.state.lock();
        if !access_allowed(state.perm, uid, gid, true, false) {
            return Err(SysErrNo::EACCES);
        }
    }
    let message = if msgflg & IPC_NOWAIT != 0 {
        let mut state = queue.state.lock();
        try_receive(&mut state, msgtyp, msgflg, msgsz, pid)?.ok_or(SysErrNo::ENOMSG)?
    } else {
        wait_receive(queue.clone(), msgtyp, msgflg, msgsz, pid)?
    };
    let copy_len = message.text.len().min(msgsz);
    let mut result = Vec::with_capacity(core::mem::size_of::<usize>() + copy_len);
    result.extend_from_slice(&(message.kind as usize).to_ne_bytes());
    result.extend_from_slice(&message.text[..copy_len]);
    write_user_bytes(msgp, &result)?;
    if msgflg & MSG_COPY == 0 {
        queue.send_wait.wake();
    }
    Ok(copy_len)
}

/// 消息队列控制操作，`cmd` 支持：
///
/// - `IPC_INFO` / `MSG_INFO`：写 `msginfo`，返回最大已用队列 id；
/// - `IPC_STAT`：写 `msqid64_ds` 快照，无读权限报 EACCES；
/// - `MSG_STAT` / `MSG_STAT_ANY`：按 id 写快照，返回队列 id（前者查权限）；
/// - `IPC_SET`：非 owner/创建者/root 报 EPERM，改 uid/gid 或把 qbytes 调超
///   MSGMNB 需要特权；若原队列已满则唤醒等待中的发送者；
/// - `IPC_RMID`：删除队列并唤醒所有阻塞的收发方（后续访问报 EIDRM）。
pub fn sys_msgctl(msqid: i32, cmd: i32, buf: *mut u8) -> SyscallRet {
    if cmd == IPC_INFO || cmd == MSG_INFO {
        let info = queue_info();
        write_user_value(buf, &info)?;
        return Ok(MSG_MANAGER
            .lock()
            .queues
            .keys()
            .next_back()
            .copied()
            .unwrap_or(0) as usize);
    }

    let queue = get_queue(msqid)?;
    let (uid, gid, _) = current_credentials()?;
    match cmd {
        IPC_STAT | MSG_STAT | MSG_STAT_ANY => {
            let state = queue.state.lock();
            if cmd != MSG_STAT_ANY && !access_allowed(state.perm, uid, gid, true, false) {
                return Err(SysErrNo::EACCES);
            }
            let snapshot = queue_snapshot(&state);
            drop(state);
            write_user_value(buf, &snapshot)?;
            if cmd == MSG_STAT || cmd == MSG_STAT_ANY {
                Ok(msqid as usize)
            } else {
                Ok(0)
            }
        }
        IPC_SET => {
            let requested: UserMsqidDs = read_user_value(buf as *const u8)?;
            let (new_uid, new_gid) = (requested.msg_perm.uid, requested.msg_perm.gid);
            let new_mode = requested.msg_perm.mode as u32 & 0o777;
            let queue_was_full;
            {
                let mut state = queue.state.lock();
                if !owner_allowed(state.perm, uid) {
                    return Err(SysErrNo::EPERM);
                }
                if new_uid != state.perm.uid && !has_cap_or_root(uid) {
                    return Err(SysErrNo::EPERM);
                }
                if new_gid != state.perm.gid && !has_cap_or_root(uid) {
                    return Err(SysErrNo::EPERM);
                }
                if requested.msg_qbytes > MSGMNB && !has_cap_or_root(uid) {
                    return Err(SysErrNo::EPERM);
                }
                queue_was_full =
                    state.bytes >= state.qbytes || state.messages.len() >= state.qbytes;
                state.perm.uid = new_uid;
                state.perm.gid = new_gid;
                state.perm.mode = new_mode;
                state.qbytes = requested.msg_qbytes;
                state.ctime = now();
            }
            if queue_was_full {
                queue.send_wait.wake();
            }
            Ok(0)
        }
        IPC_RMID => {
            if !owner_allowed(queue.state.lock().perm, uid) {
                return Err(SysErrNo::EPERM);
            }
            let mut manager = MSG_MANAGER.lock();
            let Some(existing) = manager.queues.remove(&msqid) else {
                return Err(SysErrNo::EINVAL);
            };
            if existing.state.lock().perm.key != IPC_PRIVATE {
                manager.keys.remove(&existing.state.lock().perm.key);
            }
            {
                let mut state = existing.state.lock();
                state.removed = true;
            }
            existing.recv_wait.wake();
            existing.send_wait.wake();
            Ok(0)
        }
        _ => Err(SysErrNo::EINVAL),
    }
}
