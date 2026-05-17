//! A library for polling I/O events and waking up tasks.

#![deny(missing_docs)]

extern crate alloc;

use crate::syscall::PollEvents;
use alloc::{boxed::Box, sync::Arc, task::Wake};
use bitflags::bitflags;
use core::{
    mem::MaybeUninit,
    task::{Context, Waker},
};
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
    entries: Box<[MaybeUninit<Waker>]>,
    cursor: usize,
}

impl Inner {
    fn new() -> Self {
        Self {
            entries: Box::new_uninit_slice(POLL_SET_CAPACITY),
            cursor: 0,
        }
    }

    fn len(&self) -> usize {
        self.cursor.min(POLL_SET_CAPACITY)
    }

    fn is_empty(&self) -> bool {
        self.cursor == 0
    }

    fn register(&mut self, waker: &Waker) {
        let slot = self.cursor % POLL_SET_CAPACITY;
        if self.cursor >= POLL_SET_CAPACITY {
            let old = unsafe { self.entries[slot].assume_init_read() };
            if !old.will_wake(waker) {
                old.wake();
            }
            self.cursor = ((slot + 1) % POLL_SET_CAPACITY) + POLL_SET_CAPACITY;
        } else {
            self.cursor += 1;
        }
        self.entries[slot].write(waker.clone());
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        for i in 0..self.len() {
            unsafe { self.entries[i].assume_init_read() }.wake();
        }
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
        self.0.lock().register(waker);
    }

    /// Wakes up all registered wakers.
    pub fn wake(&self) -> usize {
        let mut guard = self.0.lock();
        if guard.is_empty() {
            return 0;
        }
        let inner = core::mem::replace(&mut *guard, Inner::new());
        drop(guard);
        inner.len()
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
