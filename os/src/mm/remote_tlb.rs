//! Cross-hart translation-cache invalidation for shared user address spaces.
//!
//! A `MemorySet` writer serializes updates through [`lock_updates`], changes
//! its page table while holding the address-space write lock, then calls
//! [`shootdown`].  The target hart acknowledges only after its local TLB and
//! instruction cache have been synchronized.  Serializing all senders is
//! deliberate: some kernel-mode trap paths cannot safely nest another page
//! table writer, so two harts must never wait for each other's software
//! interrupt at once.

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::arch::{cpu::hart_id, hardware::MAX_SUPPORTED_HARTS};
use crate::sync::RemoteTlbMutex;
use spin::MutexGuard;

struct TlbMailbox {
    pending: AtomicBool,
    sequence: AtomicUsize,
    acknowledged: AtomicUsize,
    #[cfg(feature = "perf")]
    requested_at: AtomicUsize,
}

impl TlbMailbox {
    const fn new() -> Self {
        Self {
            pending: AtomicBool::new(false),
            sequence: AtomicUsize::new(0),
            acknowledged: AtomicUsize::new(0),
            #[cfg(feature = "perf")]
            requested_at: AtomicUsize::new(0),
        }
    }
}

static MAILBOXES: [TlbMailbox; MAX_SUPPORTED_HARTS] =
    [const { TlbMailbox::new() }; MAX_SUPPORTED_HARTS];

/// Only one hart may wait for remote shootdown acknowledgements at a time.
///
/// This lock is acquired before the corresponding `MemorySet` write lock.
/// A waiter can itself be a target of the current shootdown, so it must keep
/// servicing its mailbox while another hart owns the lock.
static UPDATE_LOCK: RemoteTlbMutex<()> = RemoteTlbMutex::new(());

/// Logical owner of a page-table update that needs translation invalidation.
/// This is diagnostic-only and does not alter the invalidation protocol.
#[cfg(feature = "perf")]
#[derive(Clone, Copy, Debug)]
#[repr(usize)]
pub(crate) enum ShootdownKind {
    PageFault,
    Cow,
    Munmap,
    Mprotect,
    Mremap,
    ForkExec,
    Other,
}

#[inline]
pub(crate) fn lock_updates() -> MutexGuard<'static, ()> {
    UPDATE_LOCK.lock()
}

/// Flush the local translation and instruction caches, then acknowledge a
/// pending remote request for this hart.
///
/// This is called from the user/kernel IPI paths on architectures which have
/// a resumable inter-processor interrupt entry.  Both QEMU RISC-V and QEMU
/// LoongArch use the same mailbox protocol; the architecture layer only
/// supplies IPI delivery and local TLB/cache invalidation.
#[inline]
pub(crate) fn poll() {
    #[cfg(any(target_arch = "riscv64", target_arch = "loongarch64"))]
    {
        let hart = hart_id();
        let mailbox = &MAILBOXES[hart];
        if mailbox.pending.swap(false, Ordering::AcqRel) {
            crate::arch::tlb::tlb_invalidate();
            crate::arch::tlb::instruction_fence();
            let sequence = mailbox.sequence.load(Ordering::Acquire);
            mailbox.acknowledged.store(sequence, Ordering::Release);
            #[cfg(feature = "perf")]
            crate::utils::perf::record_remote_tlb_acknowledgement(
                crate::arch::time::get_ticks()
                    .saturating_sub(mailbox.requested_at.load(Ordering::Relaxed)),
            );
        }
    }
}

/// Make a completed page-table update visible on every active remote hart.
///
/// The caller holds both [`UPDATE_LOCK`] and the affected `MemorySet` write
/// lock.  The local flush is unconditional: it also handles operations on an
/// address space that has just stopped being current on this hart.  The live
/// active-hart mask is re-read while waiting: a target that has detached from
/// this address space cannot return to user mode until the write lock drops,
/// whereupon its normal activation flushes the local translation cache.
#[inline]
pub(crate) fn shootdown(active_harts: &AtomicUsize, #[cfg(feature = "perf")] kind: ShootdownKind) {
    #[cfg(feature = "perf")]
    let shootdown_begin = crate::arch::time::get_ticks();

    crate::arch::tlb::tlb_invalidate();
    crate::arch::tlb::instruction_fence();

    #[cfg(any(target_arch = "riscv64", target_arch = "loongarch64"))]
    {
        let source = hart_id();
        let remote_harts = active_harts.load(Ordering::Acquire) & !(1usize << source);
        if remote_harts == 0 {
            #[cfg(feature = "perf")]
            crate::utils::perf::record_remote_tlb_shootdown(
                kind as usize,
                0,
                crate::arch::time::get_ticks().saturating_sub(shootdown_begin),
            );
            return;
        }
        // Prepare every mailbox before notifying any target.  Waiting for an
        // acknowledgement inside this loop serializes an N-hart shootdown
        // behind the sum of each target's IPI latency.  UPDATE_LOCK already
        // guarantees that no second sender can reuse a mailbox sequence.
        let mut sequences = [0usize; MAX_SUPPORTED_HARTS];
        #[cfg(feature = "perf")]
        let mut requested_at = [0usize; MAX_SUPPORTED_HARTS];
        let hart_count = crate::arch::hardware::hart_count().min(MAX_SUPPORTED_HARTS);
        for target in 0..hart_count {
            let target_bit = 1usize << target;
            if remote_harts & target_bit == 0 {
                continue;
            }

            let mailbox = &MAILBOXES[target];
            let sequence = mailbox
                .sequence
                .fetch_add(1, Ordering::AcqRel)
                .wrapping_add(1);
            sequences[target] = sequence;
            #[cfg(feature = "perf")]
            {
                let begin = crate::arch::time::get_ticks();
                requested_at[target] = begin;
                mailbox.requested_at.store(begin, Ordering::Relaxed);
            }
            mailbox.pending.store(true, Ordering::Release);
        }

        // Dispatch the complete IPI fan-out before observing any mailbox.
        // Targets can now invalidate concurrently, so the protocol waits for
        // the slowest active Hart instead of summing every target latency.
        for target in 0..hart_count {
            let target_bit = 1usize << target;
            if remote_harts & target_bit == 0 {
                continue;
            }
            if !crate::arch::cpu::wake_hart(target) {
                panic!("remote TLB shootdown could not wake hart {}", target);
            }
        }

        for target in 0..hart_count {
            let target_bit = 1usize << target;
            if remote_harts & target_bit == 0 {
                continue;
            }
            let mailbox = &MAILBOXES[target];
            let sequence = sequences[target];
            while mailbox.acknowledged.load(Ordering::Acquire) != sequence {
                if active_harts.load(Ordering::Acquire) & target_bit == 0 {
                    break;
                }
                core::hint::spin_loop();
            }
            #[cfg(feature = "perf")]
            crate::utils::perf::record_remote_tlb_mailbox_wait(
                crate::arch::time::get_ticks().saturating_sub(requested_at[target]),
            );
        }
        #[cfg(feature = "perf")]
        crate::utils::perf::record_remote_tlb_shootdown(
            kind as usize,
            remote_harts.count_ones() as usize,
            crate::arch::time::get_ticks().saturating_sub(shootdown_begin),
        );
    }

    #[cfg(not(any(target_arch = "riscv64", target_arch = "loongarch64")))]
    {
        let _ = active_harts;
        #[cfg(feature = "perf")]
        crate::utils::perf::record_remote_tlb_shootdown(
            kind as usize,
            0,
            crate::arch::time::get_ticks().saturating_sub(shootdown_begin),
        );
    }
}
