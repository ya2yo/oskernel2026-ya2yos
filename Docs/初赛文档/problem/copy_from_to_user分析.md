# copy_from_user / copy_to_user 分析文档

## 1. 概述

`copy_from_user` 和 `copy_to_user` 是内核中用于**内核空间与用户空间之间安全传输数据**的两个核心函数。它们封装了对用户空间虚拟地址的翻译、缺页处理、跨页拆分等复杂逻辑，使调用者无需关心底层页表细节。

定义位置：`os/src/mm/translate.rs`

```rust
pub fn copy_from_user(memory_set: &MemorySet, src: usize, dst: &mut [u8]) -> SyscallRet
pub fn copy_to_user(memory_set: &MemorySet, dst: usize, src: &[u8]) -> SyscallRet
```

- `copy_from_user`：从用户空间 `src` 复制数据到内核空间 `dst`
- `copy_to_user`：从内核空间 `src` 复制数据到用户空间 `dst`

---

## 2. 核心设计

### 2.1 调用链

```c
copy_from_user / copy_to_user
  └── checked_user_range()          ← 地址合法性检查
  └── PageTable::from_token()       ← 构建页表快照
  └── translated_user_page()        ← 逐页翻译 + 缺页处理
        └── page_table.translate()  ← 先尝试直接翻译
        └── memory_set.lazy_page_fault()  ← 未命中时触发缺页处理
              └── MemorySetInner::lazy_page_fault()
                    ├── mmap:  mmap_write_page_fault / mmap_read_page_fault
                    └── brk/stack: lazy_page_fault (仅映射)
        └── page_table.translate()  ← 缺页处理后重试翻译
  └── copy_from_slice()             ← 逐页复制数据
```

### 2.2 地址合法性检查 — `checked_user_range`

```rust
fn checked_user_range(start: usize, len: usize) -> Result<usize, SysErrNo> {
    if len == 0 { return Ok(start); }
    if start == 0 { return Err(SysErrNo::EFAULT); }  // NULL 指针
    start.checked_add(len).ok_or(SysErrNo::EFAULT)    // 溢出检查
}
```

验证三项：

1. 长度为 0 直接返回（空操作合法）
2. 起始地址为 `NULL`（0x0）返回 `EFAULT`
3. 起始 + 长度不溢出 `usize` 范围

### 2.3 逐页翻译 + 缺页处理 — `translated_user_page`

```rust
fn translated_user_page(
    memory_set: &MemorySet,
    page_table: &PageTable,
    vpn: VirtPageNum,
    fault: Trap,
) -> Option<PhysPageNum>
```

核心逻辑：

1. **先尝试直接翻译**：`page_table.translate(vpn)`，如果页表项已存在且有效，直接返回物理页号
2. **缺页处理**：如果未命中，调用 `memory_set.lazy_page_fault(vpn, fault)` 尝试按需映射
3. **重试翻译**：再次 `translate(vpn)`，若仍失败返回 `None`（地址不在任何合法区域）

`fault` 参数区分读写语义：

- `copy_from_user` 传入 `LoadPageFault` — 读用户空间数据
- `copy_to_user` 传入 `StorePageFault` — 写用户空间数据
- mmap 区域会根据 fault 类型区分读/写处理，brk/stack 不区分

---

## 3. copy_from_user 详细流程

```c
copy_from_user(memory_set, src, dst)
```

1. `len = dst.len()` — 复制长度由内核缓冲区大小决定
2. 若 `len == 0`，直接返回 `Ok(0)`
3. `checked_user_range(src, len)` — 验证 `[src, src+len)` 在用户空间合法
4. 从 `memory_set` 获取页表 token，构建 `PageTable` 快照
5. 逐页循环：
   - 取当前虚拟地址 `cur_src` 的页号 `vpn`
   - 调用 `translated_user_page(memory_set, &page_table, vpn, LoadPageFault)`
   - 若返回 `None`，说明地址不在任何合法区域，返回 `Err(EFAULT)`
   - 计算本页内可复制字节数：`min(剩余长度, 到页末距离)`
   - 从 `ppn.bytes_array()` 切片复制到 `dst[cur_dst..]`
   - 推进 `cur_src` 和 `cur_dst`
6. 返回 `Ok(len)`

### 关键特征

- **方向**：用户空间 → 内核空间
- **读取语义**：fault 类型为 `LoadPageFault`
- **mmap 行为**：对 mmap 区域触发读缺页时，优先查找共享页组（`GROUP_SHARE`），若无共享页则分配新页并从文件读取数据
- **长度由内核缓冲区决定**：调用者准备多大的 `dst`，就复制多少字节

---

## 4. copy_to_user 详细流程

```c
copy_to_user(memory_set, dst, src)
```

1. `len = src.len()` — 复制长度由内核数据大小决定
2. 若 `len == 0`，直接返回 `Ok(0)`
3. `checked_user_range(dst, len)` — 验证 `[dst, dst+len)` 在用户空间合法
4. 从 `memory_set` 获取页表 token，构建 `PageTable` 快照
5. 逐页循环：
   - 取当前虚拟地址 `cur_dst` 的页号 `vpn`
   - 调用 `translated_user_page(memory_set, &page_table, vpn, StorePageFault)`
   - 若返回 `None`，返回 `Err(EFAULT)`
   - 计算本页内可复制字节数
   - 从 `src[cur_src..]` 复制到 `ppn.bytes_array_mut()` 切片
   - 推进 `cur_dst` 和 `cur_src`
6. 返回 `Ok(len)`

### 关键特征

- **方向**：内核空间 → 用户空间
- **写入语义**：fault 类型为 `StorePageFault`
- **mmap 行为**：对 mmap 区域触发写缺页时，分配新页并从文件读取数据，同时设置 COW
- **长度由内核数据决定**：调用者准备多大的 `src`，就复制多少字节

---

## 5. 缺页处理链

### 5.1 MemorySet::lazy_page_fault

```rust
// memory_set.rs:124
pub fn lazy_page_fault(&self, vpn: VirtPageNum, scause: Trap) -> bool {
    self.inner.get_unchecked_mut().lazy_page_fault(vpn, scause)
}
```

`MemorySet` 对外暴露 `&self` 接口，内部通过 `SyncUnsafeCell::get_unchecked_mut()` 绕过 Rust 借用检查获取可变引用。这意味着在持有 `RwLockReadGuard` 的情况下仍可修改内存集。

### 5.2 MemorySetInner::lazy_page_fault

按优先级查找 VPN 所属的 area：

1. **mmap 区域**（`MapAreaType::Mmap`）
   - 读缺页（`LoadPageFault` / `FetchInstructionPageFault`）→ `mmap_read_page_fault`：查找共享页组，无共享则分配新页并从文件读数据
   - 写缺页 → `mmap_write_page_fault`：分配新页，从文件读数据，设置 COW
2. **堆/栈区域**（`MapAreaType::Brk` / `MapAreaType::Stack`）
   - 直接调用 `lazy_page_fault`：仅 `vma.map_one(page_table, va)` 建立映射，延迟分配物理页
3. 若 VPN 不在任何 area → 返回 `false`

### 5.3 返回值含义

- `true`：找到对应 area 并建立了映射
- `false`：VPN 不在任何合法 area 内，`translated_user_page` 随后返回 `None`，`copy_from/to_user` 返回 `EFAULT`

---

## 6. 内存安全性分析

### 6.1 SyncUnsafeCell 模式

`MemorySet` 使用 `SyncUnsafeCell<MemorySetInner>` 封装数据：

```rust
pub struct MemorySet {
    pub inner: SyncUnsafeCell<MemorySetInner>,
}
```

所有方法通过 `get_unchecked_mut()` 获取 `&mut MemorySetInner`，**即使在共享引用下**。这允许在 `RwLockReadGuard` 保护下修改页表（添加映射、分配物理页）。

### 6.2 RwLockReadGuard 下的写操作

通常的调用模式：

```rust
let proc_inner = task.process.inner_lock();
let memory_set = proc_inner.get_locked_memory_set_read();  // RwLockReadGuard
copy_from_user(&memory_set, user_ptr, &mut kernel_buf);
```

`RwLockReadGuard` 允许多个读者同时访问。通过 `SyncUnsafeCell`，在读者上下文中仍可修改 `MemorySetInner` 的页表和 area 数据帧。这在单核环境下是安全的（内核不会被抢占），多核环境需要小心。

### 6.3 与不安全版本的对比

| 特性 | `copy_from/to_user` | `translated_str` / `translated_ref` |
| ------ | --------------------- | -------------------------------------- |
| 缺页处理 | 支持（`lazy_page_fault`） | **不支持**（直接 `.unwrap()`） |
| 溢出检查 | `checked_user_range` | 无 |
| 跨页处理 | 逐页循环 | 仅单页内 |
| NULL 检查 | 是 | 无 |
| 失败时行为 | 返回 `Err(EFAULT)` | **panic** |
| 适用场景 | 用户空间数据读写 | 已确保映射存在的场景 |

不安全的函数（`translated_str`, `translated_ref`, `translated_refmut`）在 `translate.rs:118` 行直接 `unwrap()` 地址翻译结果：

```rust
*(KernelAddr::from(page_table.translate_va(VirtAddr::from(va)).unwrap()).as_mut())
```

**只在确认地址已映射的上下文中使用**。系统调用处理中应优先使用 `copy_from/to_user`。

---

## 7. 典型调用场景

### 7.1 系统调用中读取用户路径（如 `sys_statx`）

```rust
let mut dst_str = [0u8; MAX_PATH_LEN];
copy_from_user(&memory_set, path as usize, &mut dst_str);
let len = dst_str.iter().position(|&b| b == 0).unwrap_or(MAX_PATH_LEN);
let path_str = core::str::from_utf8(&dst_str[..len]).unwrap_or("");
```

### 7.2 系统调用中写入用户缓冲区（如写 statx 结果）

```rust
let bytes = unsafe {
    core::slice::from_raw_parts(
        &statx as *const statx as *const u8,
        core::mem::size_of::<statx>(),
    )
};
copy_to_user(&memory_set, statxbuf as usize, bytes).map(|_| ())?;
```

### 7.3 进程退出时清除 clear_child_tid

```rust
copy_to_user(&memory_set, curr_task_inner.clear_child_tid as usize, &[0u8; 4]);
```

---

## 8. 错误处理总结

| 错误条件 | 检测位置 | 返回值 |
| ---------- | ---------- | -------- |
| 长度为 0 | `copy_from/to_user` 入口 | `Ok(0)` |
| 用户地址为 NULL (0x0) | `checked_user_range` 第 26 行 | `Err(EFAULT)` |
| 地址范围溢出 usize | `checked_user_range` 第 29 行 | `Err(EFAULT)` |
| VPN 不在任何合法 area | `translated_user_page` 返回 `None` | `Err(EFAULT)` |
| 跨页在任意页失败 | 循环内 `?` 提前返回 | `Err(EFAULT)` |

调用者应**始终检查返回值**，忽略返回值可能导致后续 `translate_va().unwrap()` panic。

---

## 9. 关键不变量

1. **`translated_user_page` 最多调用一次 `lazy_page_fault`**：如果 `lazy_page_fault` 返回 `true` 但重试 `translate` 仍为 `None`，说明 area 存在但映射失败（理论不应发生），返回 `None` → `EFAULT`
2. **`copy_from/to_user` 不修改 `memory_set` 的 areas 结构**：仅通过 `lazy_page_fault` 修改页表和 area 数据帧
3. **跨页数据复制保证原子性**：任一页失败都回退（通过返回 `EFAULT` 表示整体失败），不会出现部分复制
4. **内核缓冲区总是已经映射的**：`dst` for `copy_from_user` 和 `src` for `copy_to_user` 都在内核空间，不需要缺页处理
