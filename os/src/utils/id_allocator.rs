// 本文件实现一个通用的id分配器类
// 其他地方可以引用这里
// 比如tid分配器
// id分配器的功能是分配唯一的id，确保每次分配的id都和以前分配的不同
// 只有在一个id被dealloc了，它以后才可能重新分配出去这个id
// 如需上锁，由调用者负责
// 该类型无法在编译期构造，必须在运行时构造

use alloc::vec::Vec;
use hashbrown::HashSet;

#[derive(Debug)]
pub enum IdAllocError {
    Overflow,
    InvalidId(usize),
    AlreadyDeallocated(usize),
}

#[derive(Debug)]
pub struct IdAllocator {
    next_id: usize,
    recycled: Vec<usize>,
    recycled_set: HashSet<usize>,
}

impl IdAllocator {
    pub fn new() -> Self {
        Self {
            next_id: 1,
            recycled: Vec::new(),
            recycled_set: HashSet::new(),
        }
    }

    /// 分配一个新ID，返回Result
    pub fn alloc(&mut self) -> Result<usize, IdAllocError> {
        if let Some(id) = self.recycled.pop() {
            self.recycled_set.remove(&id);
            Ok(id)
        } else {
            let id = self.next_id;
            self.next_id = self.next_id.checked_add(1).ok_or(IdAllocError::Overflow)?;
            Ok(id)
        }
    }

    /// 回收ID，返回Result
    pub fn dealloc(&mut self, id: usize) -> Result<(), IdAllocError> {
        if id >= self.next_id {
            return Err(IdAllocError::InvalidId(id));
        }
        if self.recycled_set.contains(&id) {
            return Err(IdAllocError::AlreadyDeallocated(id));
        }

        self.recycled.push(id);
        self.recycled_set.insert(id);
        Ok(())
    }

    /// 检查ID是否有效
    pub fn is_valid(&self, id: usize) -> bool {
        id < self.next_id && !self.recycled_set.contains(&id)
    }
}
