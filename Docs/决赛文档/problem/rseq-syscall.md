# rseq(293) 系统调用接入

## 背景

RISC-V 和 LoongArch64 使用 Linux asm-generic syscall ABI，`293` 对应
`rseq(struct rseq *rseq, uint32_t rseq_len, int flags, uint32_t sig)`。原仓库
`Syscall` 枚举在 `Statx = 291` 后直接跳到高号 syscall，`293` 落入默认分支并返回
`ENOSYS`。

本次需求是新增 293 号 syscall 接入，不要求完成或验证 Linux rseq 的完整行为语义。

## 实现范围

### syscall 与用户态入口

- `os/src/syscall/mod.rs` 增加 `Syscall::Rseq = 293`，分发到 `sys_rseq()`。
- `os/src/syscall/task/rseq.rs` 只负责按 ABI 解码四个参数并委托 task 层。
- `user/src/syscall/mod.rs` 增加 raw syscall 包装；`user/src/lib.rs` 公开经典
  32-byte、32-byte 对齐的 `RseqAbi` 和 `rseq()` 包装。

### 线程状态与生命周期

- `os/src/task/rseq.rs` 保存每线程的 ABI 地址、长度和 signature；`TaskControlBlockInner`
  增加对应字段，避免将 TLS 指针错误放到线程组共享的 `Process`。
- 当前仅接受旧 ABI 的 `len == 32`、32-byte 对齐、注册 `flags == 0` 和注销
  `RSEQ_FLAG_UNREGISTER == 1`。注册初始化 `cpu_id_start`、`cpu_id`、`rseq_cs` 和
  扩展保留字段；重复注册、错误 signature 注销等返回 Linux 可见 errno。
- 非 `CLONE_VM` 的 fork 复制 rseq 注册，`CLONE_VM` 子线程清空注册，成功 `execve`
  清空指向旧地址空间的注册状态。
- 返回用户态时更新 CPU ID，并在发现活跃 `rseq_cs` 时清理或按 descriptor 的
  `abort_ip`/signature 做防御性 fixup；信号 frame 创建前经过同一出口，避免 frame
  保存已中断的 rseq critical section PC。

### 回归探针

`user/src/bin/initproc/rseq_regression.rs` 已接入 `initproc`，覆盖基础 ABI 闭环：

- 错误 flags、未对齐地址、短结构和坏地址；
- 注册、重复注册、错误 signature 注销、正确注销和重复注销；
- 返回用户态时对范围外 `rseq_cs` 的清理。

最新 RISC-V QEMU `log.ans` 已输出 `rseq regression: PASS`，说明上述基础 ABI
闭环在 initproc 中执行成功。

## 验证结果

- RISC-V QEMU：`log.ans` 包含 `rseq regression: PASS`。
- 该结论覆盖本探针的参数校验、注册/重复注册、signature 注销、重复注销和范围外
  `rseq_cs` 清理；不覆盖真实 rseq critical section 的 abort 指令路径。
- 本轮日志中没有 rseq 探针失败、内核 panic 或未正常关机的迹象。

## 未验证边界

本次维护者明确不要求验证 rseq 语义正确性。虽然已通过 RISC-V QEMU 运行基础探针，仍未
运行 LTP 或 Linux rseq selftests，也没有使用架构专用汇编构造真实 critical section 来验证
抢占、信号和跨 hart abort；LoongArch64 也尚未进行对应运行时回归。上述代码不能据此声明为
Linux rseq 全量兼容实现。

已执行的静态检查是 `cargo fmt --manifest-path os/Cargo.toml -- --check`、
`cargo fmt --manifest-path user/Cargo.toml -- --check`、`git diff --check`，以及复用
`os/src/task/rseq.rs` 的临时 Rust 类型检查壳。

## 涉及文件

| 路径 | 修改内容 |
| --- | --- |
| `os/src/syscall/mod.rs` | 293 号枚举和分发 |
| `os/src/syscall/task/{mod.rs,rseq.rs}` | syscall 入口 |
| `os/src/task/{mod.rs,rseq.rs,task/task.rs}` | rseq 状态、clone/exec 生命周期 |
| `os/src/trap/mod.rs` | 返回用户态和信号 frame 前的 fixup 入口 |
| `user/src/{lib.rs,syscall/mod.rs}` | 用户 ABI 和 raw syscall 包装 |
| `user/src/bin/initproc/rseq_regression.rs` | RISC-V `log.ans` 已通过的基础回归探针 |
| `user/src/bin/initproc.rs` | 将探针接入 initproc 测试入口 |

## 后续建议

在具备双架构交叉 C 工具链后，补跑 LoongArch64 的 `rseq_regression`，再引入 Linux
`tools/testing/selftests/rseq` 的基础注册、CPU ID 和抢占/信号 abort 用例。只有这些行为
测例通过后，才能把 rseq 临界区语义标记为已验证。
