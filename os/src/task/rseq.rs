//! Per-thread restartable-sequence state and user-return fixups.
//!
//! The rseq ABI is thread-local.  The syscall layer only decodes its four
//! arguments; registration state, CPU publication, and critical-section abort
//! handling live here with the task lifecycle.

use core::mem::size_of;

use crate::{
    arch::cpu::hart_id,
    mm::{copy_from_user_val, copy_to_user_val, probe_user_write, MemorySet},
    utils::{SysErrNo, SysResult},
};

use super::TaskControlBlock;

pub(crate) const RSEQ_LEN: u32 = 32;
const RSEQ_ALIGN: usize = 32;
const RSEQ_FLAG_UNREGISTER: u32 = 1;
const RSEQ_CPU_ID_UNINITIALIZED: u32 = u32::MAX;

const RSEQ_CPU_ID_START_OFFSET: usize = 0;
const RSEQ_CS_OFFSET: usize = 8;
const RSEQ_NODE_ID_OFFSET: usize = 20;
const RSEQ_MM_CID_OFFSET: usize = 24;

/// Classic 32-byte Linux rseq ABI area.  Newer ABI extensions are deliberately
/// not advertised through auxv yet, so the kernel accepts only this layout.
#[repr(C, align(32))]
#[derive(Clone, Copy)]
struct RseqAbi {
    cpu_id_start: u32,
    cpu_id: u32,
    rseq_cs: u64,
    flags: u32,
    node_id: u32,
    mm_cid: u32,
    slice_ctrl: u32,
}

/// User-owned critical-section descriptor referenced by `RseqAbi::rseq_cs`.
#[repr(C, align(32))]
#[derive(Clone, Copy)]
struct RseqCs {
    _version: u32,
    _flags: u32,
    start_ip: u64,
    post_commit_offset: u64,
    abort_ip: u64,
}

const _: [(); RSEQ_LEN as usize] = [(); size_of::<RseqAbi>()];
const _: [(); RSEQ_LEN as usize] = [(); size_of::<RseqCs>()];

/// Kernel-owned portion of one thread's rseq registration.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct RseqState {
    abi_addr: usize,
    len: u32,
    sig: u32,
}

impl RseqState {
    fn is_registered(self) -> bool {
        self.abi_addr != 0
    }
}

fn user_addr(base: usize, offset: usize) -> Result<usize, SysErrNo> {
    base.checked_add(offset).ok_or(SysErrNo::EFAULT)
}

fn write_ids(memory_set: &MemorySet, abi_addr: usize, cpu_id: u32) -> SysResult {
    // The CPU fields are adjacent in the ABI.  Keep them in one copy so the
    // hot pending path performs one range check/page-table walk for both
    // stores.  The node/mm fields are likewise a single short segment.
    let cpu_ids = [cpu_id; 2];
    copy_to_user_val(
        memory_set,
        user_addr(abi_addr, RSEQ_CPU_ID_START_OFFSET)? as *mut [u32; 2],
        &cpu_ids,
    )?;

    // Ya2yOS has no NUMA topology or per-mm concurrency ID yet.  Publishing
    // zero matches the single-node/non-extended ABI contract.
    let node_mm = [0u32; 2];
    copy_to_user_val(
        memory_set,
        user_addr(abi_addr, RSEQ_NODE_ID_OFFSET)? as *mut [u32; 2],
        &node_mm,
    )?;
    Ok(())
}

fn clear_rseq_cs(memory_set: &MemorySet, abi_addr: usize) -> SysResult {
    copy_to_user_val(
        memory_set,
        user_addr(abi_addr, RSEQ_CS_OFFSET)? as *mut u64,
        &0u64,
    )
}

impl TaskControlBlock {
    /// Implement the classic Linux `rseq(2)` registration and unregistration
    /// ABI for the current thread.
    pub(crate) fn rseq(&self, abi_addr: usize, len: u32, flags: u32, sig: u32) -> SysResult {
        if flags & RSEQ_FLAG_UNREGISTER != 0 {
            if flags != RSEQ_FLAG_UNREGISTER {
                return Err(SysErrNo::EINVAL);
            }

            let state = self.inner_lock().rseq;
            if !state.is_registered() || state.abi_addr != abi_addr {
                return Err(SysErrNo::EINVAL);
            }
            if state.len != len {
                return Err(SysErrNo::EINVAL);
            }
            if state.sig != sig {
                return Err(SysErrNo::EPERM);
            }

            let memory_set = self.process.memory_set_arc();
            write_ids(&memory_set, state.abi_addr, RSEQ_CPU_ID_UNINITIALIZED)?;

            let mut inner = self.inner_lock();
            if inner.rseq == state {
                inner.rseq = RseqState::default();
                inner.rseq_pending = false;
            }
            return Ok(());
        }

        if flags != 0 {
            return Err(SysErrNo::EINVAL);
        }

        let state = self.inner_lock().rseq;
        if state.is_registered() {
            if state.abi_addr != abi_addr || state.len != len {
                return Err(SysErrNo::EINVAL);
            }
            if state.sig != sig {
                return Err(SysErrNo::EPERM);
            }
            return Err(SysErrNo::EBUSY);
        }

        if len != RSEQ_LEN || abi_addr % RSEQ_ALIGN != 0 {
            return Err(SysErrNo::EINVAL);
        }

        let memory_set = self.process.memory_set_arc();
        probe_user_write(&memory_set, abi_addr, RSEQ_LEN as usize)?;
        let abi = RseqAbi {
            cpu_id_start: hart_id() as u32,
            cpu_id: hart_id() as u32,
            rseq_cs: 0,
            flags: 0,
            node_id: 0,
            mm_cid: 0,
            slice_ctrl: 0,
        };
        copy_to_user_val(&memory_set, abi_addr as *mut RseqAbi, &abi)?;

        let mut inner = self.inner_lock();
        if inner.rseq.is_registered() {
            return Err(SysErrNo::EBUSY);
        }
        inner.rseq = RseqState { abi_addr, len, sig };
        inner.rseq_pending = true;
        Ok(())
    }

    /// Publish the current CPU and abort a user rseq critical section before
    /// returning to user mode.  Callers convert a user-memory/descriptor error
    /// into SIGSEGV, matching Linux's fatal handling for broken rseq state.
    pub(crate) fn rseq_prepare_user_return(&self, force: bool) -> SysResult {
        let (state, instruction_pointer, pending) = {
            let inner = self.inner_lock();
            (inner.rseq, inner.trap_cx().get_sepc(), inner.rseq_pending)
        };
        if !state.is_registered() || (!force && !pending) {
            return Ok(());
        }

        let memory_set = self.process.memory_set_arc();
        write_ids(&memory_set, state.abi_addr, hart_id() as u32)?;

        let cs_addr = copy_from_user_val::<u64>(
            &memory_set,
            user_addr(state.abi_addr, RSEQ_CS_OFFSET)? as *const u64,
        )? as usize;
        if cs_addr == 0 {
            let mut inner = self.inner_lock();
            if inner.rseq == state {
                inner.rseq_pending = false;
            }
            return Ok(());
        }

        let cs = copy_from_user_val::<RseqCs>(&memory_set, cs_addr as *const RseqCs)?;
        if instruction_pointer.wrapping_sub(cs.start_ip as usize) >= cs.post_commit_offset as usize
        {
            clear_rseq_cs(&memory_set, state.abi_addr)?;
            let mut inner = self.inner_lock();
            if inner.rseq == state {
                inner.rseq_pending = false;
            }
            return Ok(());
        }

        let abort_ip = cs.abort_ip as usize;
        let signature_addr = abort_ip
            .checked_sub(size_of::<u32>())
            .ok_or(SysErrNo::EFAULT)?;
        let signature = copy_from_user_val::<u32>(&memory_set, signature_addr as *const u32)?;
        if signature != state.sig {
            return Err(SysErrNo::EFAULT);
        }

        clear_rseq_cs(&memory_set, state.abi_addr)?;
        let mut inner = self.inner_lock();
        if inner.rseq == state {
            #[cfg(feature = "fault-diagnostics")]
            log::warn!(
                "[fault-diagnostics] rseq_abort pid={} tid={} hart={} interrupted_pc={:#x} abi={:#x} cs={:#x} start_ip={:#x} post_commit_offset={:#x} abort_ip={:#x} signature={:#x}",
                self.pid(),
                self.tid(),
                hart_id(),
                instruction_pointer,
                state.abi_addr,
                cs_addr,
                cs.start_ip,
                cs.post_commit_offset,
                abort_ip,
                signature,
            );
            inner.trap_cx().set_sepc(abort_ip);
            inner.rseq_pending = false;
        }
        Ok(())
    }

    /// Stop retrying user-memory access after the rseq area became invalid.
    /// The trap return path will deliver SIGSEGV to the affected thread.
    pub(crate) fn disable_rseq(&self) {
        let mut inner = self.inner_lock();
        inner.rseq = RseqState::default();
        inner.rseq_pending = false;
    }
}
