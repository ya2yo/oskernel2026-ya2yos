//! A library for polling I/O events and waking up tasks.

#![deny(missing_docs)]

extern crate alloc;

use crate::syscall::PollEvents;
use alloc::{sync::Arc, task::Wake, vec::Vec};
use bitflags::bitflags;
use core::task::{Context, Waker};
use spin::{Lazy, Mutex};

// /// Trait for types that can be polled for I/O events.
// pub trait Pollable {
//     /// Polls for I/O events.
//     fn poll(&self) -> PollEvents;

//     /// Registers wakers for I/O events.
//     fn register(&self, context: &mut Context<'_>, events: PollEvents);
// }

const POLL_SET_CAPACITY: usize = 64;

struct Inner {
    entries: Vec<Waker>,
}

impl Inner {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Add one task at most once and return a displaced task when full.
    fn register(&mut self, waker: &Waker) -> Option<Waker> {
        if self.entries.iter().any(|entry| entry.will_wake(waker)) {
            return None;
        }
        let displaced = if self.entries.len() == POLL_SET_CAPACITY {
            Some(self.entries.remove(0))
        } else {
            None
        };
        self.entries.push(waker.clone());
        displaced
    }

    fn unregister(&mut self, waker: &Waker) {
        let mut index = 0;
        while index < self.entries.len() {
            if self.entries[index].will_wake(waker) {
                self.entries.remove(index);
            } else {
                index += 1;
            }
        }
    }

    fn take_one(&mut self) -> Option<Waker> {
        (!self.entries.is_empty()).then(|| self.entries.remove(0))
    }
}

/// A data structure for waking up tasks that are waiting for I/O events.
pub struct PollSet(Lazy<Mutex<Inner>>);

impl Default for PollSet {
    fn default() -> Self {
        Self::new()
    }
}

impl PollSet {
    /// Creates a new empty [`PollSet`].
    pub const fn new() -> Self {
        Self(Lazy::new(|| Mutex::new(Inner::new())))
    }

    /// Registers a waker.
    pub fn register(&self, waker: &Waker) {
        let displaced = self.0.lock().register(waker);
        if let Some(displaced) = displaced {
            displaced.wake();
        }
    }

    /// Removes a previously registered waker without waking it.
    pub fn unregister(&self, waker: &Waker) {
        self.0.lock().unregister(waker);
    }

    /// Wakes one registered task in FIFO order.
    pub fn wake_one(&self) -> usize {
        let waker = self.0.lock().take_one();
        if let Some(waker) = waker {
            waker.wake();
            1
        } else {
            0
        }
    }

    /// Wakes up all registered wakers.
    pub fn wake(&self) -> usize {
        let wakers = {
            let mut guard = self.0.lock();
            core::mem::take(&mut guard.entries)
        };
        let count = wakers.len();
        for waker in wakers {
            waker.wake();
        }
        count
    }
}

impl Drop for PollSet {
    fn drop(&mut self) {
        // Ensure all entries are dropped
        self.wake();
    }
}

impl Wake for PollSet {
    fn wake(self: Arc<Self>) {
        self.as_ref().wake();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.as_ref().wake();
    }
}
