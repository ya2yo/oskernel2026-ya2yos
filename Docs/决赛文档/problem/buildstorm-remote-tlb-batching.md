# BuildStorm P1 页表更新批处理与 remote-TLB 观测

## 背景

《优化方案.md》的 P1 指出，BuildStorm 样本累计出现 911,051 次 remote-TLB
shootdown。页表更新必须继续遵守 `UPDATE_LOCK -> MemorySet write -> ACK -> release old
frame`：不能为了减少 IPI 在远端失效确认前释放旧帧，也不能把 fresh non-present
fault 误改为广播。

现有实现已经具备 range-aware frame retention、fresh fault fast path 和活跃 Hart
mask。P1 的本轮目标是在保持该协议的前提下消除同一 syscall 内的重复本地刷新，并让
分析运行能区分调用来源与 mailbox 等待成本。

## 现象

`MemorySet` handle 层会在整次页表更新完成后调用统一的
`remote_tlb::shootdown()`。但 `MemorySetInner::munmap()` 会对命中的每个 VMA 再执行
一次 `tlb_invalidate()`；`mprotect`、`mremap`、brk 缩小、共享内存解除映射及 fork
COW 父页表更新也留下了内层本地刷新。这样一次连续 VPN 更新可能执行多次本地 flush，
最后仍会进入一次全协议 shootdown。

原 perf 输出只有 `shootdowns`、`target_harts` 和 `acknowledgements` 三个累计值，无法
区分 local-only 更新、哪类 syscall 发起广播，以及耗时主要在发起侧等待还是远端 ACK。

## 根因

页表操作逐步从 `MemorySetInner` 移到 handle 层统一完成帧保留和跨 Hart 同步后，内层
保留的本地 TLB 刷新没有同步收敛到外层批量边界。内层刷新既不能替代远端 ACK，又会和
外层无条件本地 flush 重复。

同时，mailbox 仅保存 sequence/ACK 状态，perf 缺少请求时间和调用来源，无法验证 P1
假设中的 remote-target、mailbox wait 与 ACK latency。

## 修复

- 删除 `MemorySetInner` 中由 handle 层调用路径覆盖的本地 `tlb_invalidate()`/
  `instruction_fence()`。`munmap`、`mprotect`、`mremap`、`MADV_DONTNEED`、brk 缩小、
  MAP_FIXED、共享内存解除和 fork COW 父页表更新在全部 PTE 修改完成后只进入一次统一
  `shootdown()`；该函数继续执行一次本地失效，并只对 active remote Hart 发送 mailbox。
- `MemorySet` 的 retain closure、活跃 Hart 暂时撤销/恢复、`UPDATE_LOCK` 和旧
  `FrameTracker` 的释放顺序不变。操作失败的清理路径同样由外层在释放写锁前完成一次
  同步，因此不会把暂时安装过的 PTE 留给本地或远端 TLB。
- 为 perf 运行加入 `ShootdownKind`：现有 PTE page fault/COW、`munmap`、`mprotect`、
  `mremap`、fork/exec 与其他更新分别计数。fresh non-present fault 继续跳过该协议，
  因而不伪造一次 broadcast。
- 每个 remote mailbox 在 perf 版本保存请求 tick；报告 local-only、remote
  shootdown、per-target mailbox wait 与接收端 ACK latency 的 samples、total_us、max_us。
- `ShootdownKind`、mailbox 请求 tick、`shootdown` 的分类参数和 `MemorySet` 包装器的
  分类参数均使用 `#[cfg(feature = "perf")]`。普通 release 不编译枚举或额外函数参数，
  只保留原有一致性协议。

## 涉及文件

- `os/src/mm/remote_tlb.rs`
- `os/src/mm/memory_set/{handle.rs,mmap_ops.rs,area_ops.rs,fork_clone.rs}`
- `os/src/utils/perf/{scheduler.rs,report.rs}`

## 验证

已执行：

```text
make build-arch TARGET_ARCH=riscv64
make perf TARGET_ARCH=riscv64
make build-arch TARGET_ARCH=loongarch64
make perf TARGET_ARCH=loongarch64
timeout 120s make run TARGET_ARCH=riscv64 > /tmp/p1-riscv-perf-final.log 2>&1
git diff --check
```

四组 release/perf 构建均通过。最终 RISC-V 8 Hart perf 运行完成并输出 `shutdown!`；
`fstat unlink`、`sigaltstack`、`rseq`、`uptime` 回归均为 `PASS`，未出现
`panic`、`TFAIL` 或 `TBROK`。

最终快照中，`remote_tlb` 报告 720 次 shootdown：707 次 local-only，13 次 remote，
13 个 remote target 均已 ACK。调用来源包含 COW 348、`munmap` 7、`mprotect` 161、
`mremap` 1、fork/exec 3 和 other 200；四组新增 duration 均有非零样本。该启动样本不
是完整 BuildStorm，也没有 cold/hot 多轮对照，因此不能据此报告端到端加速百分比；LTP
`mprotect`/`munmap`/`mremap` 专项和长时间 BuildStorm 仍需后续回归。
