//! Process, exec, vfork, wait, futex, and procfs performance counters.

use core::sync::atomic::AtomicUsize;

use crate::arch::time::get_ticks;

use super::common::{add, record_duration};
use super::syscall::{record_futex_active_duration, record_wait_active_duration};

pub(crate) static CLONE_ADDRESS_SPACE_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static CLONE_ADDRESS_SPACE_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static CLONE_ADDRESS_SPACE_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static CLONE_PROCESS_TOTAL_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static CLONE_PROCESS_TOTAL_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static CLONE_PROCESS_TOTAL_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static CLONE_ACTIVE_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static CLONE_ACTIVE_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static CLONE_ACTIVE_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static CLONE_VFORK_WAIT_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static CLONE_VFORK_WAIT_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static CLONE_VFORK_WAIT_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFORK_CHILD_TO_EXEC_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFORK_CHILD_TO_EXEC_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFORK_CHILD_TO_EXEC_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFORK_EXEC_TO_PARENT_READY_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFORK_EXEC_TO_PARENT_READY_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFORK_EXEC_TO_PARENT_READY_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFORK_CHILD_TO_EXIT_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFORK_CHILD_TO_EXIT_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFORK_CHILD_TO_EXIT_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFORK_PARENT_READY_TO_RESUME_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFORK_PARENT_READY_TO_RESUME_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFORK_PARENT_READY_TO_RESUME_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFORK_RELEASE_EXEC: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFORK_RELEASE_EXIT: AtomicUsize = AtomicUsize::new(0);
pub(crate) static VFORK_RELEASE_SIGNAL: AtomicUsize = AtomicUsize::new(0);
pub(crate) static CLONE_BOOTSTRAP_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static CLONE_BOOTSTRAP_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static CLONE_BOOTSTRAP_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static CLONE_PARENT_STATE_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static CLONE_PARENT_STATE_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static CLONE_PARENT_STATE_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static CLONE_PROCESS_CREATE_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static CLONE_PROCESS_CREATE_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static CLONE_PROCESS_CREATE_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static CLONE_TASK_SETUP_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static CLONE_TASK_SETUP_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static CLONE_TASK_SETUP_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static CLONE_PROCFS_REGISTER_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static CLONE_PROCFS_REGISTER_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static CLONE_PROCFS_REGISTER_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static CLONE_PUBLISH_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static CLONE_PUBLISH_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static CLONE_PUBLISH_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static CLONE_ENQUEUE_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static CLONE_ENQUEUE_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static CLONE_ENQUEUE_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static PROCFS_MATERIALIZE_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static PROCFS_MATERIALIZE_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static PROCFS_MATERIALIZE_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);

pub(crate) static EXEC_IMAGE_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static EXEC_IMAGE_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static EXEC_IMAGE_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static EXEC_FROM_ELF_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static EXEC_FROM_ELF_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static EXEC_FROM_ELF_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static EXEC_STACK_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static EXEC_STACK_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static EXEC_STACK_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static EXEC_COMMIT_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static EXEC_COMMIT_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static EXEC_COMMIT_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static EXEC_KERNEL_SPACE_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static EXEC_KERNEL_SPACE_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static EXEC_KERNEL_SPACE_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static EXEC_INTERP_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static EXEC_INTERP_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static EXEC_INTERP_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static EXEC_INTERP_READ_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static EXEC_INTERP_READ_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static EXEC_INTERP_READ_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static EXEC_INTERP_MAP_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static EXEC_INTERP_MAP_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static EXEC_INTERP_MAP_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static EXEC_MAP_ELF_SAMPLES: AtomicUsize = AtomicUsize::new(0);
pub(crate) static EXEC_MAP_ELF_TICKS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static EXEC_MAP_ELF_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
pub fn record_clone_address_space_duration(elapsed: usize) {
    record_duration(
        &CLONE_ADDRESS_SPACE_SAMPLES,
        &CLONE_ADDRESS_SPACE_TICKS,
        &CLONE_ADDRESS_SPACE_MAX_TICKS,
        elapsed,
    );
}

/// Profile the full `TaskControlBlock::clone_process()` success path.
#[inline]
pub fn record_clone_process_total_duration(elapsed: usize) {
    record_duration(
        &CLONE_PROCESS_TOTAL_SAMPLES,
        &CLONE_PROCESS_TOTAL_TICKS,
        &CLONE_PROCESS_TOTAL_MAX_TICKS,
        elapsed,
    );
}

/// Profile clone work up to the child becoming runnable, excluding a possible
/// `CLONE_VFORK` parent wait after the child has been published.
#[inline]
pub fn record_clone_active_duration(elapsed: usize) {
    record_duration(
        &CLONE_ACTIVE_SAMPLES,
        &CLONE_ACTIVE_TICKS,
        &CLONE_ACTIVE_MAX_TICKS,
        elapsed,
    );
}

/// Profile the semantic parent wait imposed by `CLONE_VFORK`.
#[inline]
pub fn record_clone_vfork_wait_duration(elapsed: usize) {
    record_duration(
        &CLONE_VFORK_WAIT_SAMPLES,
        &CLONE_VFORK_WAIT_TICKS,
        &CLONE_VFORK_WAIT_MAX_TICKS,
        elapsed,
    );
}

/// Profile a vfork child from publication to its first execve entry.
#[inline]
pub fn record_vfork_child_to_exec_duration(elapsed: usize) {
    record_duration(
        &VFORK_CHILD_TO_EXEC_SAMPLES,
        &VFORK_CHILD_TO_EXEC_TICKS,
        &VFORK_CHILD_TO_EXEC_MAX_TICKS,
        elapsed,
    );
}

/// Profile a vfork child from execve entry until it releases its parent.
#[inline]
pub fn record_vfork_exec_to_parent_ready_duration(elapsed: usize) {
    record_duration(
        &VFORK_EXEC_TO_PARENT_READY_SAMPLES,
        &VFORK_EXEC_TO_PARENT_READY_TICKS,
        &VFORK_EXEC_TO_PARENT_READY_MAX_TICKS,
        elapsed,
    );
}

/// Profile a vfork child from publication to an exit-based parent release.
#[inline]
pub fn record_vfork_child_to_exit_duration(elapsed: usize) {
    record_duration(
        &VFORK_CHILD_TO_EXIT_SAMPLES,
        &VFORK_CHILD_TO_EXIT_TICKS,
        &VFORK_CHILD_TO_EXIT_MAX_TICKS,
        elapsed,
    );
}

/// Profile a vfork parent from Ready until it resumes after its forced yield.
#[inline]
pub fn record_vfork_parent_ready_to_resume_duration(elapsed: usize) {
    record_duration(
        &VFORK_PARENT_READY_TO_RESUME_SAMPLES,
        &VFORK_PARENT_READY_TO_RESUME_TICKS,
        &VFORK_PARENT_READY_TO_RESUME_MAX_TICKS,
        elapsed,
    );
}

#[inline]
pub fn record_vfork_release_exec() {
    add(&VFORK_RELEASE_EXEC, 1);
}

#[inline]
pub fn record_vfork_release_exit() {
    add(&VFORK_RELEASE_EXIT, 1);
}

#[inline]
pub fn record_vfork_release_signal() {
    add(&VFORK_RELEASE_SIGNAL, 1);
}

/// Profile TID/kernel-stack allocation and the initial parent metadata snapshot.
#[inline]
pub fn record_clone_bootstrap_duration(elapsed: usize) {
    record_duration(
        &CLONE_BOOTSTRAP_SAMPLES,
        &CLONE_BOOTSTRAP_TICKS,
        &CLONE_BOOTSTRAP_MAX_TICKS,
        elapsed,
    );
}

/// Profile the parent task lock scope. `address_space` is a nested subphase.
#[inline]
pub fn record_clone_parent_state_duration(elapsed: usize) {
    record_duration(
        &CLONE_PARENT_STATE_SAMPLES,
        &CLONE_PARENT_STATE_TICKS,
        &CLONE_PARENT_STATE_MAX_TICKS,
        elapsed,
    );
}

/// Profile creation and registration of the child `Process` object.
#[inline]
pub fn record_clone_process_create_duration(elapsed: usize) {
    record_duration(
        &CLONE_PROCESS_CREATE_SAMPLES,
        &CLONE_PROCESS_CREATE_TICKS,
        &CLONE_PROCESS_CREATE_MAX_TICKS,
        elapsed,
    );
}

/// Profile child task construction, user resources, and child-visible state.
#[inline]
pub fn record_clone_task_setup_duration(elapsed: usize) {
    record_duration(
        &CLONE_TASK_SETUP_SAMPLES,
        &CLONE_TASK_SETUP_TICKS,
        &CLONE_TASK_SETUP_MAX_TICKS,
        elapsed,
    );
}

/// Profile in-memory `/proc` PID registration during a process clone.
#[inline]
pub fn record_clone_procfs_register_duration(elapsed: usize) {
    record_duration(
        &CLONE_PROCFS_REGISTER_SAMPLES,
        &CLONE_PROCFS_REGISTER_TICKS,
        &CLONE_PROCFS_REGISTER_MAX_TICKS,
        elapsed,
    );
}

/// Profile final child publication to task and shared-resource registries.
#[inline]
pub fn record_clone_publish_duration(elapsed: usize) {
    record_duration(
        &CLONE_PUBLISH_SAMPLES,
        &CLONE_PUBLISH_TICKS,
        &CLONE_PUBLISH_MAX_TICKS,
        elapsed,
    );
}

/// Profile placing the child into the scheduler ready queue.
#[inline]
pub fn record_clone_enqueue_duration(elapsed: usize) {
    record_duration(
        &CLONE_ENQUEUE_SAMPLES,
        &CLONE_ENQUEUE_TICKS,
        &CLONE_ENQUEUE_MAX_TICKS,
        elapsed,
    );
}

/// Profile a real deferred `/proc/<pid>` EXT4 directory materialization.
#[inline]
pub fn record_procfs_materialize_duration(elapsed: usize) {
    record_duration(
        &PROCFS_MATERIALIZE_SAMPLES,
        &PROCFS_MATERIALIZE_TICKS,
        &PROCFS_MATERIALIZE_MAX_TICKS,
        elapsed,
    );
}

#[inline]
pub fn record_exec_image_duration(elapsed: usize) {
    record_duration(
        &EXEC_IMAGE_SAMPLES,
        &EXEC_IMAGE_TICKS,
        &EXEC_IMAGE_MAX_TICKS,
        elapsed,
    );
}

#[inline]
pub fn record_exec_from_elf_duration(elapsed: usize) {
    record_duration(
        &EXEC_FROM_ELF_SAMPLES,
        &EXEC_FROM_ELF_TICKS,
        &EXEC_FROM_ELF_MAX_TICKS,
        elapsed,
    );
}

#[inline]
pub fn record_exec_stack_duration(elapsed: usize) {
    record_duration(
        &EXEC_STACK_SAMPLES,
        &EXEC_STACK_TICKS,
        &EXEC_STACK_MAX_TICKS,
        elapsed,
    );
}

#[inline]
pub fn record_exec_commit_duration(elapsed: usize) {
    record_duration(
        &EXEC_COMMIT_SAMPLES,
        &EXEC_COMMIT_TICKS,
        &EXEC_COMMIT_MAX_TICKS,
        elapsed,
    );
}

#[inline]
pub fn record_exec_kernel_space_duration(elapsed: usize) {
    record_duration(
        &EXEC_KERNEL_SPACE_SAMPLES,
        &EXEC_KERNEL_SPACE_TICKS,
        &EXEC_KERNEL_SPACE_MAX_TICKS,
        elapsed,
    );
}

#[inline]
pub fn record_exec_interp_duration(elapsed: usize) {
    record_duration(
        &EXEC_INTERP_SAMPLES,
        &EXEC_INTERP_TICKS,
        &EXEC_INTERP_MAX_TICKS,
        elapsed,
    );
}

#[inline]
pub fn record_exec_interp_read_duration(elapsed: usize) {
    record_duration(
        &EXEC_INTERP_READ_SAMPLES,
        &EXEC_INTERP_READ_TICKS,
        &EXEC_INTERP_READ_MAX_TICKS,
        elapsed,
    );
}

#[inline]
pub fn record_exec_interp_map_duration(elapsed: usize) {
    record_duration(
        &EXEC_INTERP_MAP_SAMPLES,
        &EXEC_INTERP_MAP_TICKS,
        &EXEC_INTERP_MAP_MAX_TICKS,
        elapsed,
    );
}

#[inline]
pub fn record_exec_map_elf_duration(elapsed: usize) {
    record_duration(
        &EXEC_MAP_ELF_SAMPLES,
        &EXEC_MAP_ELF_TICKS,
        &EXEC_MAP_ELF_MAX_TICKS,
        elapsed,
    );
}
pub struct CloneActiveGuard {
    begin: usize,
}

impl CloneActiveGuard {
    #[inline]
    pub fn new() -> Self {
        Self { begin: get_ticks() }
    }
}

impl Drop for CloneActiveGuard {
    #[inline]
    fn drop(&mut self) {
        record_clone_active_duration(get_ticks().saturating_sub(self.begin));
    }
}
pub struct WaitActiveGuard {
    begin: usize,
}

impl WaitActiveGuard {
    #[inline]
    pub fn new() -> Self {
        Self { begin: get_ticks() }
    }
}

impl Drop for WaitActiveGuard {
    #[inline]
    fn drop(&mut self) {
        record_wait_active_duration(get_ticks().saturating_sub(self.begin));
    }
}

/// Scope guard for one actively executing futex interval.
pub struct FutexActiveGuard {
    begin: usize,
}

impl FutexActiveGuard {
    #[inline]
    pub fn new() -> Self {
        Self { begin: get_ticks() }
    }
}

impl Drop for FutexActiveGuard {
    #[inline]
    fn drop(&mut self) {
        record_futex_active_duration(get_ticks().saturating_sub(self.begin));
    }
}
