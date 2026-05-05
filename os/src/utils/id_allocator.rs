// 本文件实现一个通用的id分配器类
// 其他地方可以引用这里
// 比如tid分配器
// id分配器的功能是分配唯一的id，确保每次分配的id都和以前分配的不同
// 只有在一个id被dealloc了，它以后才可能重新分配出去这个id
// 如需上锁，由调用者负责
// 该类型无法在编译期构造，必须在运行时构造

use alloc::vec::Vec;

pub struct IdAllocator {
    next_id: usize,
    recycled: Vec<usize>,
}

impl IdAllocator {
    pub const fn new(start_id: usize) -> Self {
        Self {
            next_id: start_id,
            recycled: Vec::new(),
        }
    }

    pub fn alloc(&mut self) -> Option<usize> {
        if let Some(id) = self.recycled.pop() {
            Some(id)
        } else {
            let id = self.next_id;
            if let Some(next) = self.next_id.checked_add(1) {
                self.next_id = next;
                Some(id)
            } else {
                None // 溢出处理
            }
        }
    }

    pub fn dealloc(&mut self, id: usize) {
        // 使用了 TidHandle 保证只在 Drop 时释放，这里只需简单的 push
        self.recycled.push(id);
    }
}
