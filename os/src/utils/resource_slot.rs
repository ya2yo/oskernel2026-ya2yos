use alloc::sync::Arc;
use spin::Mutex;

/// 可替换的进程级共享资源槽。
///
/// 资源对象自身负责保护内部状态；`ResourceSlot` 只保护当前 `Arc<T>` 指针
/// 的获取和整体替换，例如 `execve()` 切换地址空间或重置信号动作表。
pub struct ResourceSlot<T> {
    current: Mutex<Arc<T>>,
}

impl<T> ResourceSlot<T> {
    pub fn new(resource: Arc<T>) -> Self {
        Self {
            current: Mutex::new(resource),
        }
    }

    pub fn from_resource(resource: T) -> Self {
        Self::new(Arc::new(resource))
    }

    pub fn get(&self) -> Arc<T> {
        self.current.lock().clone()
    }

    pub fn replace(&self, resource: Arc<T>) -> Arc<T> {
        let mut current = self.current.lock();
        core::mem::replace(&mut *current, resource)
    }

    pub fn replace_with(&self, resource: T) -> Arc<T> {
        self.replace(Arc::new(resource))
    }

    pub fn strong_count(&self) -> usize {
        let current = self.current.lock();
        Arc::strong_count(&current)
    }
}
