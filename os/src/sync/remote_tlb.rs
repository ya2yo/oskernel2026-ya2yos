//! Mutex wrapper for kernel paths that must acknowledge remote TLB requests
//! while waiting for a contended lock.

use spin::{Mutex, MutexGuard};

/// A spin mutex that services the local remote-TLB mailbox while contended.
///
/// This is intentionally a separate type from [`spin::Mutex`].  Polling a
/// mailbox on every contended lock would add architecture/MM behavior to
/// unrelated locks, while the locks using this type are known to be reachable
/// from a remote shootdown target with interrupts disabled.
pub struct RemoteTlbMutex<T: ?Sized> {
    inner: Mutex<T>,
}

impl<T> RemoteTlbMutex<T> {
    /// Create an unlocked mailbox-aware mutex.
    pub const fn new(value: T) -> Self {
        Self {
            inner: Mutex::new(value),
        }
    }

    /// Consume the mutex and return the protected value.
    pub fn into_inner(self) -> T {
        self.inner.into_inner()
    }
}

impl<T: ?Sized> RemoteTlbMutex<T> {
    /// Lock the mutex, polling the local remote-TLB mailbox between attempts.
    #[inline]
    pub fn lock(&self) -> MutexGuard<'_, T> {
        loop {
            if let Some(guard) = self.inner.try_lock() {
                return guard;
            }
            crate::mm::remote_tlb::poll();
            core::hint::spin_loop();
        }
    }

    /// Attempt to lock without polling or spinning.
    #[inline]
    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        self.inner.try_lock()
    }

    /// Return a mutable reference when the caller has exclusive access.
    #[inline]
    pub fn get_mut(&mut self) -> &mut T {
        self.inner.get_mut()
    }
}

impl<T: ?Sized + core::fmt::Debug> core::fmt::Debug for RemoteTlbMutex<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.inner.fmt(f)
    }
}

impl<T: Default> Default for RemoteTlbMutex<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T> From<T> for RemoteTlbMutex<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}
