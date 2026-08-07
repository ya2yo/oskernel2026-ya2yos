---
name: add-syscall-feature
description: >-
  Ya2yOS 新增功能或实现新的 Linux syscall。用于用户要求添加内核能力、
  增加/修改 sys_*、接入 syscall 号、实现文件/任务/内存/网络等功能，并要求完成验证与文档记录时。
---

# 添加功能 / Syscall

目标：把新功能按本仓库结构放到正确位置，保持 syscall 层薄，完成最小验证，并按 `write-docs` 补记录。

## 先定位

1. 读用户需求、Linux 语义和现有同类实现。
2. 用 `rg` 查现有 syscall 号、`sys_*`、用户态封装和测试入口。
3. 新代码优先扩展已有模块，不为单个小函数新建泛化目录。

## 代码放置

| 需求 | 位置 |
|------|------|
| syscall 号与分发 | `os/src/syscall/mod.rs` |
| 文件类 syscall | `os/src/syscall/fs/`，核心语义在 `os/src/fs/` |
| 任务/进程 syscall | `os/src/syscall/task/`，核心语义在 `os/src/task/` |
| 内存 syscall | `os/src/syscall/mm/`，核心语义在 `os/src/mm/` 或 `arch/` |
| futex/同步 syscall | `os/src/syscall/sync/`，等待队列在 `os/src/task/` |
| poll/epoll/select | `os/src/syscall/io_mpx/`，文件对象语义在 `os/src/fs/files/` |
| socket/网络 syscall | `os/src/syscall/net/`，协议语义在 `os/src/net/` |
| 架构相关页表/trap | `os/src/arch/<arch>/` |
| 用户态封装或测试 | `user/src/` |

原则：`syscall/` 只做参数解析、fd 查找、用户内存拷贝和调用领域 API；状态机、缓存、队列、协议语义下沉到 `fs` / `task` / `mm` / `net`。

## 实现步骤

1. 在 `Syscall` 枚举使用 Linux syscall 号。
2. 在 `syscall()` match 中接入 `sys_*`，参数保持 `args[0..6]` 映射清楚。
3. 在对应子模块实现 `sys_*`；需要新文件时同步更新父 `mod.rs`。
4. 读取用户指针必须用 `copy_from_user` / `copy_to_user`；先检查 NULL 和长度。
5. 内部错误用 `?` 传播，不要吞掉 `Err` 后固定 `Ok(0)`。
6. 保持锁顺序简单：先当前任务，再进程内部锁；避免跨阻塞点持锁。

## 验证

至少运行：

```bash
make
```

涉及运行语义时再跑：

```bash
make log
make run
```

需要单测时在 `user/src/bin/` 加临时或正式测试，或调整 `user/src/bin/initproc.rs` 跑对应 LTP/busybox。看 LTP 内部 `TPASS`/`TFAIL`，不要只看 `FAIL LTP CASE xxx : 0` 字样。

## 收尾

- 非平凡功能、新 syscall、修通测例：使用 `write-docs`。
- 双架构相关改动：至少说明 RISC-V / LoongArch64 哪些验证跑过，哪些没跑。
- 最终回复列出改动文件和验证结果。
