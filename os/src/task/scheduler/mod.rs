use super::TaskControlBlock;
use alloc::sync::Arc;

#[cfg(all(feature = "scheduler-rr", feature = "scheduler-cfs"))]
compile_error!("scheduler-rr and scheduler-cfs are mutually exclusive");

#[cfg(not(any(feature = "scheduler-rr", feature = "scheduler-cfs")))]
compile_error!("enable exactly one scheduler feature: scheduler-rr or scheduler-cfs");

#[cfg(feature = "scheduler-cfs")]
mod cfs;
#[cfg(feature = "scheduler-rr")]
mod rr;

#[cfg(feature = "scheduler-cfs")]
use cfs as policy;
#[cfg(feature = "scheduler-rr")]
use rr as policy;

#[cfg(feature = "scheduler-cfs")]
pub(crate) use cfs::SchedEntity;

#[cfg(feature = "scheduler-rr")]
pub(crate) struct SchedEntity;

#[cfg(feature = "scheduler-rr")]
impl SchedEntity {
    pub const fn new() -> Self {
        Self
    }
}

pub mod ready_queue {
    use super::*;

    pub fn add_task(task: &Arc<TaskControlBlock>) {
        #[cfg(feature = "scheduler-cfs")]
        if let Some(ready_tasks) = policy::add_task(task) {
            crate::task::processor::notify_harts_of_runnable_task(task.cpu_affinity(), ready_tasks);
        }
        #[cfg(feature = "scheduler-rr")]
        {
            policy::add_task(task);
            crate::task::processor::notify_hart_of_runnable_task(task.scheduled_hart());
        }
    }

    /// Requeue the task that just left the current Hart without waking another
    /// Hart.  The scheduler is already running on the Hart that will compete
    /// for this task, so publishing an IPI for this bookkeeping enqueue only
    /// creates redundant remote wakeups.  External wakeups must continue to
    /// use [`add_task`] so an idle allowed Hart is notified.
    pub(crate) fn requeue_current(task: &Arc<TaskControlBlock>) {
        #[cfg(feature = "scheduler-cfs")]
        {
            let _ = policy::add_task(task);
        }
        #[cfg(feature = "scheduler-rr")]
        policy::add_task(task);
        #[cfg(feature = "perf")]
        crate::utils::perf::record_scheduler_current_requeue(false);
    }

    pub fn fetch_task(hartid: usize) -> Option<Arc<TaskControlBlock>> {
        policy::fetch_task(hartid)
    }

    pub fn ready_procs_num() -> usize {
        policy::ready_procs_num()
    }

    /// Return whether the specified hart has a queued task that can compete
    /// with its currently running task.  The scheduler uses this on timer
    /// preemption to avoid a needless round trip through the idle context
    /// when the run queue is otherwise empty.
    pub(crate) fn has_ready_for_hart(hartid: usize) -> bool {
        policy::has_ready_for_hart(hartid)
    }

    pub(crate) fn mark_running(task: &Arc<TaskControlBlock>) {
        policy::mark_running(task);
    }

    pub(crate) fn account_current(task: &Arc<TaskControlBlock>) {
        policy::account_current(task);
    }
}
