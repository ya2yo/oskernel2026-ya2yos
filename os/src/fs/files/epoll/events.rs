//! epoll 事件掩码与内部 [`PollEvents`] 互转。

use linux_raw_sys::general::{EPOLLERR, EPOLLHUP, EPOLLIN, EPOLLOUT, EPOLLRDHUP};

use crate::syscall::PollEvents;

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
