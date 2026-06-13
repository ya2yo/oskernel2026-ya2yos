---
name: syscall-implementation
description: >-
  在本 OS 内核中新增或修改系统调用的三步流程。改完后须验证并写文档，见 kernel-change。
  用于实现新 syscall、修复 syscall 语义错误、或 ecall/trap 相关工作时。
---

# 系统调用实现指南

> 总流程：[kernel-change](../kernel-change/SKILL.md)。完成后更新 [doc-writing](../doc-writing/SKILL.md)。**放码位置**见 [oskernel-conventions/placement.md](../oskernel-conventions/placement.md)。

## 概述

系统调用通过 `ecall` 触发，在 `trap_handler` 中捕获，路由到 `sys_*` 函数。

## 注册三步

### 1. Syscall 枚举（`os/src/syscall/mod.rs`）

```rust
#[derive(Debug, PartialEq, FromPrimitive)]
#[repr(usize)]
pub enum Syscall {
    Read = 63,
    Write = 64,
    Socket = 198,
    // 值必须与 Linux 编号一致
}
```

### 2. syscall() 路由

```rust
pub fn syscall(syscall_id: usize, args: [usize; 6]) -> SyscallRet {
    match Syscall::from(syscall_id) {
        Syscall::Read => sys_read(args[0], args[1] as *const u8, args[2]),
        _ => { /* ENOSYS 或 exit */ }
    }
}
```

### 3. 实现 sys_*

按功能放入子模块：`syscall/fs/`、`syscall/net/`、`syscall/task/` 等。

## 标准模式

### 简单 syscall（fd 操作）

```rust
pub fn sys_write(fd: usize, buf: *const u8, len: usize) -> SyscallRet {
    let task = current_task()?;
    let proc_inner = task.process.inner_lock();
    let file = proc_inner.fd_table.get(fd)?.file()?;
    let memory_set = proc_inner.get_locked_memory_set_read();
    let mut kernel_buf = vec![0; len];
    copy_from_user(&memory_set, buf as usize, &mut kernel_buf)?;
    file.write(&kernel_buf)
}
```

### 网络 syscall

见 `os/src/syscall/net/`：`socket.rs`、`opt.rs`、`io.rs`

**陷阱**：`sys_setsockopt` 的 `match` 结果必须用 `?` 传播，不能无条件 `Ok(0)`。

### 带复杂结构（statx 等）

1. 检查 NULL 指针 → `EFAULT`
2. `copy_from_user` 读路径/参数
3. 内核操作
4. `copy_to_user` 写回

## 关键辅助

```rust
current_task()?                                    // 当前任务
task.process.inner_lock().fd_table.get(fd)?       // fd
proc_inner.get_locked_memory_set_read()            // 页表
copy_from_user(&memory_set, ptr, &mut buf)?        // 用户→内核
copy_to_user(&memory_set, ptr, &slice)?            // 内核→用户
```

## 常见陷阱

| 错误 | 正确 |
|------|------|
| 直接使用用户指针 `slice::from_raw_parts(buf, len)` | `copy_from_user` |
| syscall 包装忽略内部 `Err` | `sock.set_option(opt)?` |
| 未检查 NULL | 先判空再解引用 |
| `translated_str().unwrap()` 读用户路径 | `copy_from_user` + UTF-8 校验 |

## 测试

1. `user/src/bin/` 写小程序
2. 或在 `initproc.rs` 启用对应 LTP/busybox 套件
3. `make log && make run`

## 参考资料

- [kernel-change](../kernel-change/SKILL.md)
- 现有实现：`os/src/syscall/`
- [OSKernel 开发规范](../oskernel-conventions/SKILL.md)
- [Linux syscall 表](https://syscalls.win/)
