use core::{
    sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    task::Waker,
    time::Duration,
};

use crate::syscall::PollEvents;
use crate::task::schedule;
use crate::utils::SysErrNo;
use crate::{
    fs::File,
    task::{block_on, poll_io, timeout},
    utils::SysResult,
};

use super::{
    get_service,
    options::{Configurable, GetSocketOption, SetSocketOption},
};

/// 通用套接字配置选项
/// 存储了影响套接字行为的所有基础参数，如非阻塞模式、超时时间等
/// 使用原子类型确保在多核环境下，无需大锁也能安全地读取和修改配置
pub(crate) struct GeneralOptions {
    /// 是否为非阻塞模式
    nonblock: AtomicBool,
    /// 地址重用标志
    reuse_address: AtomicBool,
    /// 发送超时时间
    send_timeout_nanos: AtomicU64,
    /// 接收超时时间
    recv_timeout_nanos: AtomicU64,
    /// 设备掩码，通常用于标识该套接字关联的网络设备
    device_mask: AtomicU32,
}
impl Default for GeneralOptions {
    fn default() -> Self {
        Self::new()
    }
}
impl GeneralOptions {
    /// 创建默认配置，阻塞模式、不重用地址、无超时
    pub fn new() -> Self {
        Self {
            nonblock: AtomicBool::new(false),
            reuse_address: AtomicBool::new(false),

            send_timeout_nanos: AtomicU64::new(0),
            recv_timeout_nanos: AtomicU64::new(0),

            device_mask: AtomicU32::new(0),
        }
    }
    /// 获取当前是否是非阻塞状态
    pub fn nonblocking(&self) -> bool {
        self.nonblock.load(Ordering::Relaxed)
    }
    /// 获取当前是否是地址重用状态
    pub fn reuse_address(&self) -> bool {
        self.reuse_address.load(Ordering::Relaxed)
    }
    /// 获取发送超时时长，如果为 0 则返回 None
    pub fn send_timeout(&self) -> Option<Duration> {
        let nanos = self.send_timeout_nanos.load(Ordering::Relaxed);
        (nanos > 0).then(|| Duration::from_nanos(nanos))
    }
    /// 获取接收超时时长，如果为 0 则返回 None
    pub fn recv_timeout(&self) -> Option<Duration> {
        let nanos = self.recv_timeout_nanos.load(Ordering::Relaxed);
        (nanos > 0).then(|| Duration::from_nanos(nanos))
    }

    pub fn set_device_mask(&self, mask: u32) {
        self.device_mask.store(mask, Ordering::Release);
    }

    pub fn device_mask(&self) -> u32 {
        self.device_mask.load(Ordering::Acquire)
    }
    /// 向底层网络服务注册当前任务的 Waker，以便在有网络包到达时唤醒任务
    pub fn register_waker(&self, waker: &Waker) {
        get_service().register_waker(self.device_mask(), waker);
    }
    /// 发送操作的通用轮询处理器
    ///
    /// 1. `poll_io`: 创建一个 Future，当 IO 就绪（OUT 事件）或符合非阻塞规则时返回结果。
    /// 2. `timeout`: 为上述 Future 包装一层超时逻辑。
    /// 3. `block_on`: 阻塞当前内核任务，直到 Future 完成、超时或被信号中断。
    pub fn send_poller<P: File, F: FnMut() -> SysResult<T>, T>(
        &self,
        pollable: &P,
        f: F,
    ) -> SysResult<T> {
        block_on(timeout(
            self.send_timeout(),
            poll_io(pollable, PollEvents::OUT, self.nonblocking(), f),
        ))?
    }
    /// 接收操作的通用轮询处理器
    /// 逻辑与 send_poller 类似，但关注的是 IN 事件
    pub fn recv_poller<P: File, F: FnMut() -> SysResult<T>, T>(
        &self,
        pollable: &P,
        f: F,
    ) -> SysResult<T> {
        block_on(timeout(
            self.recv_timeout(),
            poll_io(pollable, PollEvents::IN, self.nonblocking(), f),
        ))?
    }
}
/// 实现 Configurable 特性，对接系统调用 getsockopt / setsockopt
impl Configurable for GeneralOptions {
    /// 获取套接字选项的具体实现
    fn get_option_inner(&self, option: &mut GetSocketOption) -> SysResult<bool> {
        use GetSocketOption as O;
        match option {
            O::Error(error) => {
                // TODO(mivik): actual logic
                **error = 0;
            }
            O::NonBlocking(nonblock) => {
                **nonblock = self.nonblocking();
            }
            O::ReuseAddress(reuse) => {
                **reuse = self.reuse_address();
            }
            O::SendTimeout(timeout) => {
                **timeout = Duration::from_nanos(self.send_timeout_nanos.load(Ordering::Relaxed));
            }
            O::ReceiveTimeout(timeout) => {
                **timeout = Duration::from_nanos(self.recv_timeout_nanos.load(Ordering::Relaxed));
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
    /// 设置套接字选项的具体实现
    fn set_option_inner(&self, option: SetSocketOption) -> SysResult<bool> {
        use SetSocketOption as O;

        match option {
            O::NonBlocking(nonblock) => {
                self.nonblock.store(*nonblock, Ordering::Relaxed);
            }
            O::ReuseAddress(reuse) => {
                self.reuse_address.store(*reuse, Ordering::Relaxed);
            }
            O::SendTimeout(timeout) => {
                self.send_timeout_nanos
                    .store(timeout.as_nanos() as u64, Ordering::Relaxed);
            }
            O::ReceiveTimeout(timeout) => {
                self.recv_timeout_nanos
                    .store(timeout.as_nanos() as u64, Ordering::Relaxed);
            }
            O::SendBuffer(_) | O::ReceiveBuffer(_) => {}
            _ => return Ok(false),
        }
        Ok(true)
    }
}
