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

use spin::{Mutex, MutexGuard};

use crate::arch::{config::HART_NUM, cpu::hart_id};

struct TlbMailbox {
    pending: AtomicBool,
    sequence: AtomicUsize,
    acknowledged: AtomicUsize,
}

impl TlbMailbox {
    const fn new() -> Self {
        Self {
            pending: AtomicBool::new(false),
            sequence: AtomicUsize::new(0),
            acknowledged: AtomicUsize::new(0),
        }
    }
}

static MAILBOXES: [TlbMailbox; HART_NUM] = [const { TlbMailbox::new() }; HART_NUM];

/// Only one hart may wait for remote shootdown acknowledgements at a time.
///
/// This lock is acquired before the corresponding `MemorySet` write lock.  A
/// second updater therefore stays out of the non-interruptible kernel path
/// instead of becoming a remote target that is itself waiting for an IPI.
static UPDATE_LOCK: Mutex<()> = Mutex::new(());

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
            crate::utils::perf::record_remote_tlb_acknowledgement();
        }
    }
}

/// Make a completed page-table update visible on every active remote hart.
///
/// The caller holds both [`UPDATE_LOCK`] and the affected `MemorySet` write
/// lock.  The local flush is unconditional: it also handles operations on an
/// address space that has just stopped being current on this hart.
#[inline]
pub(crate) fn shootdown(active_harts: usize) {
    crate::arch::tlb::tlb_invalidate();
    crate::arch::tlb::instruction_fence();

    #[cfg(any(target_arch = "riscv64", target_arch = "loongarch64"))]
    {
        let source = hart_id();
        let remote_harts = active_harts & !(1usize << source);
        #[cfg(feature = "perf")]
        crate::utils::perf::record_remote_tlb_shootdown(remote_harts.count_ones() as usize);
        for target in 0..HART_NUM {
            if remote_harts & (1usize << target) == 0 {
                continue;
            }

            let mailbox = &MAILBOXES[target];
            let sequence = mailbox
                .sequence
                .fetch_add(1, Ordering::AcqRel)
                .wrapping_add(1);
            mailbox.pending.store(true, Ordering::Release);
            if !crate::arch::cpu::wake_hart(target) {
                panic!("remote TLB shootdown could not wake hart {}", target);
            }
            while mailbox.acknowledged.load(Ordering::Acquire) != sequence {
                core::hint::spin_loop();
            }
        }
    }

    #[cfg(not(any(target_arch = "riscv64", target_arch = "loongarch64")))]
    let _ = active_harts;
}
