//! Fault-safe access to the current task's user address space.
//!
//! Architecture-specific copy helpers enter a short uaccess scope before
//! touching user virtual addresses directly. A synchronous kernel page fault
//! can then either repair the current user mapping and retry the faulting
//! instruction, or redirect execution to the helper's fixup return path.

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::{
    arch::{hardware::MAX_SUPPORTED_HARTS, memory_layout::USER_SPACE_SIZE},
    task::current_task,
    trap::trap_types::{Exception, Trap},
};

use super::{MemorySet, VirtAddr};

struct UaccessState {
    active: AtomicBool,
    memory_set: AtomicUsize,
    fixup_pc: AtomicUsize,
    retry_vpn: AtomicUsize,
}

const NO_RETRY_VPN: usize = usize::MAX;

impl UaccessState {
    const fn new() -> Self {
        Self {
            active: AtomicBool::new(false),
            memory_set: AtomicUsize::new(0),
            fixup_pc: AtomicUsize::new(0),
            retry_vpn: AtomicUsize::new(NO_RETRY_VPN),
        }
    }
}

static UACCESS_STATE: [UaccessState; MAX_SUPPORTED_HARTS] =
    [const { UaccessState::new() }; MAX_SUPPORTED_HARTS];

#[derive(Clone, Copy)]
struct UaccessSnapshot {
    active: bool,
    memory_set: usize,
    fixup_pc: usize,
    retry_vpn: usize,
}

#[inline]
fn current_state() -> &'static UaccessState {
    let hart = crate::arch::cpu::hart_id();
    assert!(hart < MAX_SUPPORTED_HARTS, "invalid hart id {}", hart);
    &UACCESS_STATE[hart]
}

#[inline]
fn snapshot(state: &UaccessState) -> UaccessSnapshot {
    UaccessSnapshot {
        active: state.active.load(Ordering::Acquire),
        memory_set: state.memory_set.load(Ordering::Relaxed),
        fixup_pc: state.fixup_pc.load(Ordering::Relaxed),
        retry_vpn: state.retry_vpn.load(Ordering::Relaxed),
    }
}

/// State guard for one architecture-specific direct user copy.
pub(crate) struct Scope {
    previous: UaccessSnapshot,
}

impl Scope {
    #[inline]
    pub(crate) fn enter(memory_set: &MemorySet, fixup_pc: usize) -> Self {
        assert_ne!(fixup_pc, 0, "uaccess fixup address must be non-zero");
        let state = current_state();
        let previous = snapshot(state);
        state
            .memory_set
            .store(memory_set as *const MemorySet as usize, Ordering::Relaxed);
        state.fixup_pc.store(fixup_pc, Ordering::Relaxed);
        state.retry_vpn.store(NO_RETRY_VPN, Ordering::Relaxed);
        state.active.store(true, Ordering::Release);
        Self { previous }
    }
}

impl Drop for Scope {
    #[inline]
    fn drop(&mut self) {
        let state = current_state();
        state.active.store(false, Ordering::Release);
        state
            .memory_set
            .store(self.previous.memory_set, Ordering::Relaxed);
        state
            .fixup_pc
            .store(self.previous.fixup_pc, Ordering::Relaxed);
        state
            .retry_vpn
            .store(self.previous.retry_vpn, Ordering::Relaxed);
        if self.previous.active {
            state.active.store(true, Ordering::Release);
        }
    }
}

/// Outcome for a synchronous kernel fault taken while a direct user copy is
/// active.
pub(crate) enum KernelFaultAction {
    /// A stale translation was flushed; retry the instruction at the current
    /// `sepc`/`era`.
    Retry,
    /// The copy helper cannot make progress; redirect to its fixup label.
    Fixup(usize),
    /// This is not a recoverable user-access fault. Preserve the old kernel
    /// panic path for unrelated kernel bugs and interrupts.
    Unhandled,
}

#[inline]
fn is_user_access_fault(cause: Trap) -> bool {
    matches!(
        cause,
        Trap::Exception(
            Exception::LoadPageFault
                | Exception::StorePageFault
                | Exception::PageModifyFault
                | Exception::PagePrivilegeIllegal
        )
    )
}

/// Handle a page fault raised by an architecture-specific direct user copy.
///
/// The fast path never enters a potentially blocking page-fault resolver from
/// the architecture trap frame. Missing/COW/file-backed pages return through
/// the copy helper fixup and are handled by the software fallback. Only a
/// present mapping with a stale local translation is retried in place.
pub(crate) fn handle_kernel_fault(cause: Trap, stval: usize) -> KernelFaultAction {
    let state = current_state();
    if !state.active.load(Ordering::Acquire) || !is_user_access_fault(cause) {
        return KernelFaultAction::Unhandled;
    }

    // A fault from the kernel source/destination buffer is a kernel bug, not a
    // user-pointer error. User mappings occupy the low canonical range on
    // both supported architectures.
    if stval >= USER_SPACE_SIZE {
        return KernelFaultAction::Unhandled;
    }

    let fixup_pc = state.fixup_pc.load(Ordering::Relaxed);
    if fixup_pc == 0 {
        return KernelFaultAction::Unhandled;
    }
    let Some(fault_va) = VirtAddr::try_from(stval) else {
        return KernelFaultAction::Fixup(fixup_pc);
    };
    let Some(task) = current_task() else {
        return KernelFaultAction::Fixup(fixup_pc);
    };
    let memory_set = task.process.memory_set_arc();
    let expected_memory_set = state.memory_set.load(Ordering::Relaxed);
    if expected_memory_set != memory_set.as_ref() as *const MemorySet as usize
        || !memory_set.is_current_hart_active()
    {
        return KernelFaultAction::Unhandled;
    }

    let vpn = fault_va.floor();

    // Do not enter the normal page-fault resolver from this synchronous trap.
    // File-backed demand faults may block in the filesystem, and switching a
    // task while sepc/sstatus are live only in the current Hart's CSRs would
    // resume the trap frame with another Hart's architectural state.  Return
    // through the copy helper's fixup instead; the caller then retries through
    // the software path, where a blocking fault is a regular syscall event.
    //
    // A present PTE can still fault once while a remote update or local TLB
    // refill catches up. Keep this retry state per Hart: this handler is
    // nested in a syscall and must not reacquire TaskControlBlockInner.
    let present_user_access = match cause {
        Trap::Exception(Exception::LoadPageFault) => memory_set.is_kernel_user_readable(vpn),
        Trap::Exception(Exception::StorePageFault | Exception::PageModifyFault) => {
            memory_set.is_kernel_user_writable(vpn)
        }
        Trap::Exception(Exception::PagePrivilegeIllegal) => false,
        _ => false,
    };
    if present_user_access {
        let retry = state
            .retry_vpn
            .compare_exchange(NO_RETRY_VPN, vpn.0, Ordering::AcqRel, Ordering::Acquire)
            .is_ok();
        if retry {
            crate::arch::tlb::tlb_invalidate();
            return KernelFaultAction::Retry;
        }
    }

    #[cfg(target_arch = "loongarch64")]
    if cause == Trap::Exception(Exception::PageModifyFault)
        && memory_set.is_kernel_user_writable(vpn)
    {
        crate::arch::trap_interface::tlb_page_modify_handler();
        return KernelFaultAction::Retry;
    }

    KernelFaultAction::Fixup(fixup_pc)
}
