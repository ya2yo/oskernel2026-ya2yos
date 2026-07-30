# LoongArch 36GiB CMA 与 12 核启动栈配置同步

## 背景

2026-07-27 将 LoongArch QEMU 配置调整为 `-m 36G -smp 12`，但共享 buddy allocator 仍按此前
RISC-V 8GiB 配置保留 34 个阶，入口汇编的启动栈数量也仍为 8。

## 现象

第一份 `log.ans` 在 `init_cma` 加入第二段高端内存时 panic：

```text
[kernel] Panicked at .../buddy_system_allocator/src/lib.rs:94
index out of bounds: the len is 34 but the index is 34
```

扩展 allocator 后，内核可以启动全部 12 个 hart，但 CAgent 日志只到：

```text
#### OS COMP TEST GROUP START cagent ####
```

没有出现 `Simple LLM Server`、测试项结果或 `shutdown!`。

## 分析

龙芯 CMA 高端区间为 `[0x80000000, 0x970000000)`，长度为 `0x8f0000000`。伙伴算法按地址对齐拆分
时会产生 `0x400000000`（16GiB）块，其阶为 34；原 `free_list` 长度 34 只能访问 0..33。

入口汇编用 `CPUID + 1` 乘以 `BOOT_STACK_SIZE` 计算每个 hart 的启动栈。`config.rs` 和 QEMU 已启动
hart 0..11，但 `entry.asm` 的 `MAX_HARTS` 仍为 8，因此 hart 8..11 会在预留栈数组之外写入，破坏
启动后的 `.bss` 或调度状态。CAgent 标记之后的第一步是后台 fork/exec `simple_llm_server`，栈破坏会
使该路径无输出地卡住；问题不在 Bash 脚本的组标记。

## 修复

- 将 `crates/buddy_system_allocator` 的 `MAX_ORDER` 扩展为 35，覆盖 order 34，并同步 `FrameAllocator`
  的注释。
- 将 LoongArch `entry.asm` 的 `MAX_HARTS` 改为 12，与 `config::HART_NUM` 和 `scripts/loongarch64.mk`
  的 `SMP` 保持一致。

## 验证

- `make build-arch TARGET_ARCH=loongarch64`：通过，仅有既有未使用代码警告。
- `timeout 120s make run TARGET_ARCH=loongarch64`：通过；最新日志启动 12 个 hart，CAgent 十项均为
  `pass`，出现 `#### OS COMP TEST GROUP END cagent ####` 和 `shutdown!`。
- 运行中有一条 PID 57 的用户态 `PagePrivilegeIllegal` 被按 `SIGSEGV` 隔离，未导致测试项失败或内核
  panic；该条属于独立的用户进程非法访问，不是本次启动栈越界的根因。
