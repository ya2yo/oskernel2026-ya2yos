//!Wrap `switch.S` as a function
use log::debug;

use super::TaskContext;
use crate::task::tid_to_task;

extern "C" {
    /// 该汇编函数的两个参数a0, a1是两个struct TaskContext*，
    /// 该函数是将当前CPU上正在运行的控制流上下文
    /// （此处上下文指的是栈、pc（对pc的保存实际上通过保存ra实现），s系列寄存器）
    /// 保存到a0指向的位置，并从a1处获取新的上下文并运行之。
    /// 因此，该函数不会正常地返回到调用者手中
    /// 如果return 0，说明next_task_cx_ptr是从另一个switch中返回的
    /// 如果return >0，则return的是tid，需要释放tid指向的内核栈
    fn __switch(
        current_task_cx_ptr: *mut TaskContext,
        next_task_cx_ptr: *const TaskContext,
    ) -> usize;

    pub fn __abandon(tid: usize, next_task_cx_ptr: *const TaskContext) -> usize;
}

/// 对汇编函数__switch的包装
/// 会在__switch之后检查返回值，如果不为0则释放那个页
pub fn switch(current_task_cx_ptr: *mut TaskContext, next_task_cx_ptr: *const TaskContext) {
    // debug!("[switch] happen!");
    let tid = unsafe { __switch(current_task_cx_ptr, next_task_cx_ptr) };
    if tid != 0 {
        tid_to_task::remove(tid);
    }
}
