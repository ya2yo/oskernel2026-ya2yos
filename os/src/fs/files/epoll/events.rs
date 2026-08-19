//! epoll 事件掩码与内部 [`PollEvents`] 互转。
//!
//! epoll 面向用户态使用 Linux 定义的位掩码，而内核文件对象统一通过
//! [`PollEvents`] 描述可读、可写、挂断和错误状态。本模块负责两个方向的
//! 映射，并保留 epoll 的特殊语义：错误和挂断即使没有出现在注册掩码中，
//! 也必须能够被报告；`PRI`、`RDNORM` 和 `WRNORM` 则分别折算为普通读写。

use linux_raw_sys::general::{EPOLLERR, EPOLLHUP, EPOLLIN, EPOLLOUT, EPOLLRDHUP};

use crate::syscall::PollEvents;

/// 将用户态注册的 Linux `epoll` 事件掩码转换为内核轮询事件集合。
///
/// 转换结果供被监视文件的 [`crate::fs::vfs::File::poll`] 和注册等待者使用；
/// 错误与挂起事件始终加入轮询集合，以便遵循 `epoll` 的无条件通知语义。
pub(crate) fn epoll_events_to_poll(ep_events: u32) -> PollEvents {
    let mut pe = PollEvents::empty();
    if ep_events & EPOLLIN != 0 {
        pe |= PollEvents::IN;
    }
    if ep_events & EPOLLOUT != 0 {
        pe |= PollEvents::OUT;
    }
    if ep_events & EPOLLHUP != 0 {
        pe |= PollEvents::HUP;
    }
    pe |= PollEvents::ERR | PollEvents::HUP;
    if ep_events & EPOLLRDHUP != 0 {
        pe |= PollEvents::RDHUP;
    }
    pe
}

/// 将内核轮询结果转换为用户态可见的 Linux `epoll` 事件掩码。
///
/// `PRI`、`RDNORM` 和 `WRNORM` 等价的轮询状态会映射到相应的
/// `EPOLLIN` 或 `EPOLLOUT` 标志。
pub(crate) fn poll_to_epoll_events(pe: PollEvents) -> u32 {
    let mut events: u32 = 0;
    if pe.contains(PollEvents::IN) {
        events |= EPOLLIN;
    }
    if pe.contains(PollEvents::PRI) {
        events |= EPOLLIN;
    }
    if pe.contains(PollEvents::OUT) {
        events |= EPOLLOUT;
    }
    if pe.contains(PollEvents::ERR) {
        events |= EPOLLERR;
    }
    if pe.contains(PollEvents::HUP) {
        events |= EPOLLHUP;
    }
    if pe.contains(PollEvents::RDHUP) {
        events |= EPOLLRDHUP;
    }
    if pe.contains(PollEvents::RDNORM) {
        events |= EPOLLIN;
    }
    if pe.contains(PollEvents::WRNORM) {
        events |= EPOLLOUT;
    }
    events
}
