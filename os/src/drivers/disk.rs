use alloc::{
    collections::VecDeque,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::{
    future::Future,
    ops::{Deref, DerefMut},
    pin::Pin,
    task::{Context, Poll, Waker},
};

use crate::drivers::BlockDriver;
#[cfg(feature = "perf")]
use crate::utils::perf::{
    record_ext4_block_request_queued, BlockRequestPerf, Ext4BlockRequestKind,
};
use spin::{Lazy, Mutex, MutexGuard};

use super::{BlockDeviceImpl, DevError, DevResult};

const BLOCK_SIZE: usize = 512;

struct DeviceSubmissionWaiter {
    ticket: usize,
    tid: usize,
    waker: Waker,
}

struct DeviceSubmissionState {
    owner: Option<usize>,
    next_ticket: usize,
    waiters: VecDeque<DeviceSubmissionWaiter>,
}

impl DeviceSubmissionState {
    const fn new() -> Self {
        Self {
            owner: None,
            next_ticket: 1,
            waiters: VecDeque::new(),
        }
    }

    fn can_acquire(&self, ticket: Option<usize>) -> bool {
        self.owner.is_none()
            && match ticket {
                Some(ticket) => self
                    .waiters
                    .front()
                    .is_some_and(|waiter| waiter.ticket == ticket),
                None => self.waiters.is_empty(),
            }
    }

    fn wake_front(&self) -> Option<Waker> {
        if self.owner.is_none() {
            self.waiters.front().map(|waiter| waiter.waker.clone())
        } else {
            None
        }
    }
}

/// Serializes physical requests without making task-context contenders spin.
///
/// `BlockDeviceImpl` currently has one synchronous virtio queue, so a request
/// must remain exclusive through a complete aligned request or unaligned RMW.
/// The queue only changes how waiters sleep; it does not claim device-level
/// parallelism.
struct DeviceSubmission {
    dev: Mutex<BlockDeviceImpl>,
    state: Mutex<DeviceSubmissionState>,
}

struct DeviceSubmissionGuard<'a> {
    submission: &'a DeviceSubmission,
    tid: usize,
    contended: bool,
    dev: Option<MutexGuard<'a, BlockDeviceImpl>>,
}

struct DeviceSubmissionFuture<'a> {
    submission: &'a DeviceSubmission,
    tid: usize,
    ticket: Option<usize>,
}

impl DeviceSubmission {
    fn new(dev: BlockDeviceImpl) -> Self {
        Self {
            dev: Mutex::new(dev),
            state: Mutex::new(DeviceSubmissionState::new()),
        }
    }

    fn try_acquire(&self, tid: usize, ticket: Option<usize>) -> bool {
        let mut state = self.state.lock();
        if !state.can_acquire(ticket) {
            return false;
        }
        if ticket.is_some() {
            state.waiters.pop_front();
        }
        state.owner = Some(tid);
        true
    }

    fn guard(&self, tid: usize, contended: bool) -> DeviceSubmissionGuard<'_> {
        let dev = self
            .dev
            .try_lock()
            .expect("device submission token lost its block-device guard");
        DeviceSubmissionGuard {
            submission: self,
            tid,
            contended,
            dev: Some(dev),
        }
    }

    fn lock(&self) -> DeviceSubmissionGuard<'_> {
        let task = crate::task::current_task();
        let tid = task.as_ref().map_or(0, |task| task.tid());
        if self.try_acquire(tid, None) {
            return self.guard(tid, false);
        }

        let Some(task) = task else {
            loop {
                if self.try_acquire(0, None) {
                    return self.guard(0, true);
                }
                core::hint::spin_loop();
            }
        };

        let tid = task.tid();
        drop(task);
        crate::task::block_on(DeviceSubmissionFuture {
            submission: self,
            tid,
            ticket: None,
        });
        self.guard(tid, true)
    }

    fn release(&self, tid: usize) {
        let next = {
            let mut state = self.state.lock();
            assert_eq!(
                state.owner,
                Some(tid),
                "block-device submission released by non-owner"
            );
            state.owner = None;
            state.wake_front()
        };
        if let Some(waker) = next {
            waker.wake();
        }
    }

    fn cancel_ticket(&self, ticket: usize) {
        let next = {
            let mut state = self.state.lock();
            let Some(index) = state
                .waiters
                .iter()
                .position(|waiter| waiter.ticket == ticket)
            else {
                return;
            };
            state.waiters.remove(index);
            state.wake_front()
        };
        if let Some(waker) = next {
            waker.wake();
        }
    }

    fn cancel_tid(&self, tid: usize) {
        let next = {
            let mut state = self.state.lock();
            state.waiters.retain(|waiter| waiter.tid != tid);
            // A synchronous block request does not reach a task cancellation
            // point while its guard exists, so only pending tickets may be
            // removed here. Releasing `owner` without dropping `dev` would
            // reintroduce a spinning waiter on the driver mutex.
            state.wake_front()
        };
        if let Some(waker) = next {
            waker.wake();
        }
    }
}

impl Deref for DeviceSubmissionGuard<'_> {
    type Target = BlockDeviceImpl;

    fn deref(&self) -> &Self::Target {
        self.dev
            .as_ref()
            .expect("device submission guard already released")
    }
}

impl DerefMut for DeviceSubmissionGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.dev
            .as_mut()
            .expect("device submission guard already released")
    }
}

impl DeviceSubmissionGuard<'_> {
    fn contended(&self) -> bool {
        self.contended
    }
}

impl Drop for DeviceSubmissionGuard<'_> {
    fn drop(&mut self) {
        drop(self.dev.take());
        self.submission.release(self.tid);
    }
}

impl Future for DeviceSubmissionFuture<'_> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        #[cfg(feature = "perf")]
        let mut queued_depth = None;
        let acquired = {
            let mut state = this.submission.state.lock();
            if state.can_acquire(this.ticket) {
                if this.ticket.is_some() {
                    state.waiters.pop_front();
                    this.ticket = None;
                }
                state.owner = Some(this.tid);
                true
            } else if let Some(ticket) = this.ticket {
                if let Some(waiter) = state
                    .waiters
                    .iter_mut()
                    .find(|waiter| waiter.ticket == ticket)
                {
                    if !waiter.waker.will_wake(cx.waker()) {
                        waiter.waker = cx.waker().clone();
                    }
                }
                false
            } else {
                let ticket = state.next_ticket;
                state.next_ticket = state.next_ticket.wrapping_add(1).max(1);
                state.waiters.push_back(DeviceSubmissionWaiter {
                    ticket,
                    tid: this.tid,
                    waker: cx.waker().clone(),
                });
                this.ticket = Some(ticket);
                #[cfg(feature = "perf")]
                {
                    queued_depth = Some(state.waiters.len());
                }
                false
            }
        };

        #[cfg(feature = "perf")]
        if let Some(queue_depth) = queued_depth {
            record_ext4_block_request_queued(queue_depth);
        }

        if acquired {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

impl Drop for DeviceSubmissionFuture<'_> {
    fn drop(&mut self) {
        if let Some(ticket) = self.ticket.take() {
            self.submission.cancel_ticket(ticket);
        }
    }
}

static DISK_SUBMISSIONS: Lazy<Mutex<Vec<Weak<DeviceSubmission>>>> =
    Lazy::new(|| Mutex::new(Vec::new()));

/// Remove stale device-submission tickets before a task abandons its stack.
pub(crate) fn cancel_disk_waiter(tid: usize) {
    let submissions = {
        let mut registry = DISK_SUBMISSIONS.lock();
        registry.retain(|submission| submission.strong_count() != 0);
        registry
            .iter()
            .filter_map(Weak::upgrade)
            .collect::<Vec<_>>()
    };
    for submission in submissions {
        submission.cancel_tid(tid);
    }
}

/// A position-independent disk adapter.
///
/// The task-aware submission queue covers a complete request, including an
/// unaligned read-modify-write, but it never protects filesystem metadata,
/// block-cache state, or inode state.
pub struct Disk {
    submission: Arc<DeviceSubmission>,
    size: usize,
}

impl Disk {
    /// Create a new disk.
    pub fn new(dev: BlockDeviceImpl) -> Self {
        assert_eq!(BLOCK_SIZE, dev.block_size());
        let submission = Arc::new(DeviceSubmission::new(dev));
        let size = submission.dev.lock().num_blocks() * BLOCK_SIZE;
        DISK_SUBMISSIONS.lock().push(Arc::downgrade(&submission));
        Self { size, submission }
    }

    /// Get the size of the disk.
    pub fn size(&self) -> usize {
        self.size
    }

    #[inline]
    fn check_range(&self, offset: usize, len: usize) -> DevResult {
        match offset.checked_add(len) {
            Some(end) if end <= self.size => Ok(()),
            _ => Err(DevError::InvalidParam),
        }
    }

    /// Read an exact byte range without changing shared request state.
    pub fn read_at(&self, offset: usize, buf: &mut [u8]) -> DevResult<usize> {
        self.check_range(offset, buf.len())?;
        if buf.is_empty() {
            return Ok(0);
        }

        #[cfg(feature = "perf")]
        let mut perf = BlockRequestPerf::new(Ext4BlockRequestKind::Read, offset, buf.len());
        #[cfg(feature = "perf")]
        let mut dev = self.submission.lock();
        #[cfg(feature = "perf")]
        perf.acquired(dev.contended());
        #[cfg(not(feature = "perf"))]
        let mut dev = self.submission.lock();

        let result = (|| {
            let mut block_id = offset / BLOCK_SIZE;
            let mut in_block = offset % BLOCK_SIZE;
            let mut done = 0;

            while done < buf.len() {
                let remaining = buf.len() - done;
                if in_block == 0 && remaining >= BLOCK_SIZE {
                    let bulk_len = remaining / BLOCK_SIZE * BLOCK_SIZE;
                    dev.read_block(block_id, &mut buf[done..done + bulk_len])?;
                    done += bulk_len;
                    block_id += bulk_len / BLOCK_SIZE;
                    continue;
                }

                let mut block = [0u8; BLOCK_SIZE];
                dev.read_block(block_id, &mut block)?;
                let count = remaining.min(BLOCK_SIZE - in_block);
                buf[done..done + count].copy_from_slice(&block[in_block..in_block + count]);
                done += count;
                block_id += 1;
                in_block = 0;
            }

            Ok(done)
        })();
        #[cfg(feature = "perf")]
        perf.finish(result.is_ok());
        result
    }

    /// Write an exact byte range without changing shared request state.
    pub fn write_at(&self, offset: usize, buf: &[u8]) -> DevResult<usize> {
        self.check_range(offset, buf.len())?;
        if buf.is_empty() {
            return Ok(0);
        }

        #[cfg(feature = "perf")]
        let mut perf = BlockRequestPerf::new(Ext4BlockRequestKind::Write, offset, buf.len());
        #[cfg(feature = "perf")]
        let mut dev = self.submission.lock();
        #[cfg(feature = "perf")]
        perf.acquired(dev.contended());
        #[cfg(not(feature = "perf"))]
        let mut dev = self.submission.lock();

        let result = (|| {
            let mut block_id = offset / BLOCK_SIZE;
            let mut in_block = offset % BLOCK_SIZE;
            let mut done = 0;

            while done < buf.len() {
                let remaining = buf.len() - done;
                if in_block == 0 && remaining >= BLOCK_SIZE {
                    let bulk_len = remaining / BLOCK_SIZE * BLOCK_SIZE;
                    dev.write_block(block_id, &buf[done..done + bulk_len])?;
                    done += bulk_len;
                    block_id += bulk_len / BLOCK_SIZE;
                    continue;
                }

                let mut block = [0u8; BLOCK_SIZE];
                dev.read_block(block_id, &mut block)?;
                let count = remaining.min(BLOCK_SIZE - in_block);
                block[in_block..in_block + count].copy_from_slice(&buf[done..done + count]);
                dev.write_block(block_id, &block)?;
                done += count;
                block_id += 1;
                in_block = 0;
            }

            Ok(done)
        })();
        #[cfg(feature = "perf")]
        perf.finish(result.is_ok());
        result
    }

    /// Complete writes already submitted to the device.
    pub fn flush(&self) -> DevResult {
        #[cfg(feature = "perf")]
        let mut perf = BlockRequestPerf::new(Ext4BlockRequestKind::Flush, 0, 0);
        #[cfg(feature = "perf")]
        let mut dev = self.submission.lock();
        #[cfg(feature = "perf")]
        perf.acquired(dev.contended());
        #[cfg(not(feature = "perf"))]
        let mut dev = self.submission.lock();

        let result = dev.flush();
        #[cfg(feature = "perf")]
        perf.finish(result.is_ok());
        result
    }
}
