---
name: debug-playbook
description: >-
  Ya2yOS 常见 bug 模式与修复方向。排查时对照 Docs/初赛文档/problem/；
  修完后按 kernel-change 写 problem/ 与开发日志。
---

# 竞赛内核调试手册

来源：`Docs/初赛文档/problem/` 及开发经验。修通后把复盘写入 problem/ 新文件，见 [kernel-change](../kernel-change/SKILL.md) + [doc-writing](../doc-writing/SKILL.md)。

先对症状分类，再跳到对应章节；详细案例在 `problem/*.md`。

## 通用原则

1. **用户指针**：用 `copy_from_user` / `copy_to_user`，禁止对用户地址 `translated_str().unwrap()`
2. **锁顺序**：先 task 锁，后 process 锁（fd_table 在 process 内）
3. **错误传播**：syscall 包装函数中对内部 `Result` 使用 `?`，不要无条件 `Ok(0)`
4. **日志**：`make log` 开 debug；`rg -a` 搜 log.ans

---

## futex

**症状**：`task already in queue` panic；信号打断后 futex 行为异常

**原因**：`EINTR` 时 Waiter 未从 `FUTEX_QUEUE_BITMAP` 清理；`futex_wake` 与 `add_signal` 竞态

**查**：`os/src/syscall/sync/futex.rs`，`os/src/task/` 信号投递路径

**修**：wait 被打断时移除 Waiter；wake 前检查任务状态

---

## block_on / accept 死循环

**症状**：accept 或阻塞 syscall 无限 `Pending`，`strong_count` 异常偏高

**原因**：`block_on` Pending 分支持有多余 `Arc<Task>` 强引用

**查**：`os/src/task/future/mod.rs`

**修**：用 `WeakTaskRef`，Pending 分支不增加强引用

---

## COW（写时复制）

**症状**：`LoadPageFault`；`valid pte without COW flag` panic；busybox/lmbench 随机失败

**原因**：fork 后子进程页表缺 `DIRTY`/`WRITABLE` 处理；写时未正确分裂页

**查**：
- RISC-V：`arch/riscv64/qemu/page_table.rs`
- LoongArch：`arch/loongarch64/qemu/page_table.rs`

**修**：COW fault 时分配新页、更新 PTE 标志、刷新 TLB

---

## pipe

**症状**：`lat_pipe` 等测试挂起或 CPU 空转

**原因**：无数据时 `yield` 而非真正阻塞

**查**：`os/src/fs/` pipe 实现

**修**：无数据/满缓冲时用 waiter 队列 + `Blocked` 状态，有数据时 wake

---

## 信号 + 阻塞任务

**症状**：向阻塞任务发信号后无响应或状态错乱

**修**：投递信号时将任务标为 `Ready` 并重新入队

---

## FsIndex OOM

**症状**：长跑多个测试后分配失败（如 16MB）

**原因**：`FsIndex` inode 缓存只增不减

**查**：`os/src/fs/kernel_fs_ops/fsidx.rs`，`sys_close`

**修**：`close` 时若 `Arc::strong_count <= 2` 则驱逐缓存项

---

## 动态链接 / glibc

**症状**：glibc 程序 `__isoc23_*` 未定义；iozone 等失败

**查**：`os/src/fs/map_dynamic_link.rs`

**修**：确认 ld-linux 路径与架构匹配（musl vs glibc 目录不同）

---

## ext4 /tmp cleanup

**症状**：测试 TPASS 后 `ext4_fopen: /tmp/LTP_*/..., rc = 2` (ENOENT)

**性质**：多为 LTP 框架 cleanup 时序问题，**通常不影响测试结果**

**若需修**：检查 `mkdir`/`unlink`/`rmdir` 语义与 `/tmp` 目录创建

---

## cgroup_fj 卡死

**症状**：直接运行 `cgroup_fj_proc` 无退出

**原因**：helper 脚本失败未通知子进程

**修**：通过 `cgroup_fj_function.sh <子命令>` 运行，或保持黑名单跳过

---

## 网络（简表）

| 症状 | 方向 |
|------|------|
| setsockopt 语义错 | `opt.rs` 错误传播 |
| 组播/IGMP | `ethernet.rs` 组播 MAC、`mod.rs` IP 顺序 |
| virtio token | `virtio/net.rs` TX 回收 |
| LA 无网络 | `main.rs` DeviceContainer |

详见 [network-debug](../network-debug/SKILL.md)

---

## 文档合规（竞赛）

- 显著修复记入 `Docs/初赛文档/开发日志.md`
- AI 辅助开发记入 `Docs/初赛文档/AI_INTERACTION.md`
- 代码规范见 [oskernel-conventions](../oskernel-conventions/SKILL.md)
