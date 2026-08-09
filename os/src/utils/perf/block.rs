//! Block-device request timing helpers.

use super::fs::{
    record_ext4_block_request_acquired, record_ext4_block_request_complete,
    record_ext4_block_request_submit, Ext4BlockRequestKind,
};
use crate::arch::time::get_ticks;

/// Perf-only timing state for one complete `Disk` request.
///
/// The helper lives beside the counters so the device driver only contains
/// request submission and I/O semantics. It deliberately does not own a
/// device lock or perform any reporting.
pub(crate) struct BlockRequestPerf {
    kind: Ext4BlockRequestKind,
    offset: usize,
    len: usize,
    started_at: usize,
    service_started: usize,
    lock_context: usize,
}

impl BlockRequestPerf {
    #[inline]
    pub(crate) fn new(kind: Ext4BlockRequestKind, offset: usize, len: usize) -> Self {
        Self {
            kind,
            offset,
            len,
            started_at: get_ticks(),
            service_started: 0,
            lock_context: 0,
        }
    }

    #[inline]
    pub(crate) fn acquired(&mut self, contended: bool) {
        let acquired = get_ticks();
        self.lock_context = record_ext4_block_request_submit(self.kind, self.offset, self.len);
        record_ext4_block_request_acquired(
            self.kind,
            acquired.saturating_sub(self.started_at),
            contended,
            self.lock_context,
        );
        self.service_started = acquired;
    }

    #[inline]
    pub(crate) fn finish(self, success: bool) {
        record_ext4_block_request_complete(
            self.kind,
            get_ticks().saturating_sub(self.service_started),
            success,
            self.lock_context,
        );
    }
}
