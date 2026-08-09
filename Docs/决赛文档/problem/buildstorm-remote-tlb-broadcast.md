# BuildStorm remote TLB 广播式 ACK 收集

## 背景

BuildStorm 的 Rust/Cargo 工作负载频繁 fork、COW 和更新页表。共享地址空间同时运行于多个
Hart 时，发起页表更新的一方必须在释放可能失效的旧 `FrameTracker` 前，确认所有活跃远端
Hart 已完成本地 TLB 与 instruction cache 失效。

已有 P1 优化已经收敛单次批量 VMA/PTE 更新中的重复本地 TLB flush，并为 remote-TLB
协议加入来源与耗时观测。本轮根据新的 `server.ans` 和 `client.ans` 继续定位跨 Hart
确认的发送侧时序。

## 现象

`server.ans` 是尚未完成的 RISC-V BuildStorm 运行：末尾仍停在
`Building 445/446: tg-xtask(bin)`，没有 `BUILDSTORM_COMPILE`、测试组结束标记或
`shutdown!`，因此不能作为端到端性能样本。

不过在 `t=1633466ms` 的完整 perf 快照中，remote-TLB 指标已经给出稳定的热点证据：

- `shootdowns=1153785`，其中 `remote=788891`，目标 Hart 总数为 `1541228`；
- 发起端 `remote_tlb_remote` 为 `788891` 个样本、累计 `135170056us`，最大
  `1067979us`；
- 按目标 Hart 计的 `remote_tlb_mailbox_wait` 为 `1541228` 个样本、累计
  `131391699us`，最大 `1067921us`；接收侧 `remote_tlb_ack_latency` 为
  `1541227` 个样本、累计 `125197952us`，最大 `1068183us`；
- `Cow=1050895`，说明 COW 是该阶段最主要的页表更新来源。快照时 ACK 少一个是正在
  进行的请求，而非已确认的协议失败。

`client.ans` 是 LoongArch64 GDB 现场，而不是可用于比例比较的 perf 样本。它显示一个
Hart 正在 `remote_tlb::shootdown(... ShootdownKind::Cow)`，另外的 Hart 位于共享 CFS
`READY_QUEUE` 的 `SpinMutex::lock()` / `fetch_task()`，其余 Hart 在
`idle_until_runnable()`。这支持 COW 广播期间跨 Hart 同步会放大调度竞争的调用路径，但
不单独证明 CFS 锁是累计耗时的根因。

## 分析

修改前的发送侧对每个目标依次执行：发布 mailbox、`wake_hart(target)`、自旋等待该
target 的 ACK，再进入下一个目标。若一个 shootdown 有 N 个远端目标，发起侧临界路径近似
为 N 次 IPI/ACK 延迟之和；接收端虽能独立处理 IPI，发送端却没有让它们并发开始。大量
COW 更新因此把本可重叠的远端失效变为串行等待。

perf 的 `mailbox_wait` 与 `ack_latency` 是 per-target 累计值，不能相加推断 wall-clock；
它们仍明确说明大量目标确认落在这条路径上。更可靠的可比主指标是每次请求端到端的
`remote_tlb_remote`，后续 A/B 需使用相同镜像、Hart 数、冷/热缓存和完成的同一
BuildStorm 阶段。

后半段 `tg-xtask` 中 remote-TLB 计数不再增长，运行却仍然缓慢。日志同时显示文件页缓存
达到 `196608/196608` 页，并出现超过
`MAX_PAGE_CACHED_READ_FILE_SIZE = 32 MiB` 文件的 direct-bypass 读取及大量 `read`/
`lseek`。这是文件页缓存容量与顺序读路径的独立 I/O 长尾，不能归因给本次 CFS 或
remote-TLB 问题，也不在本轮改动范围内。

## 根因

`os/src/mm/remote_tlb.rs` 的 sender 将每个远端 mailbox 的 ACK 等待放在 IPI fan-out
循环内。`UPDATE_LOCK` 已经正确地串行化不同发起者，但该全局发送序列又意外把同一次
shootdown 的不同目标串行化，造成 remote TLB 失效等待随活跃目标数累加。

## 修复

将 `shootdown()` 改为三阶段 broadcast-then-collect：

1. 为本次 `remote_harts` 的每个目标分配并保存 sequence，写入 perf 请求 tick，随后以
   `pending.store(true, Release)` 发布 mailbox；
2. 遍历目标并发送全部 `wake_hart()` IPI；
3. 再按各自保存的 sequence 收集 ACK，保持目标已脱离 `active_harts` 时可以退出等待的
   原有规则。

该调整不改变一致性或生命周期协议：`UPDATE_LOCK` 和 `MemorySet` write lock 仍覆盖完整
更新与 ACK 收集，发送端的 `Release` 发布、接收端 `pending.swap(false, AcqRel)`、sequence
读取及 ACK 的 `Release` 写入、发送端 ACK 的 `Acquire` 读取均保持不变。旧帧仍在 ACK
之后才释放，本地 TLB/instruction cache flush 与 IPI 数量也不变。`UPDATE_LOCK` 保证第二个
sender 不会在本轮收集 ACK 前复用同一 mailbox sequence。

新的 `remote_tlb_mailbox_wait` 从每个 mailbox 发布起计到该目标 ACK，其中包含随后的
fan-out 时间，目标样本之间会重叠；其历史累计值不能和旧实现直接逐项相加比较。
`remote_tlb_remote` 仍表示单次 shootdown 的端到端时间，预期由多目标累计等待收敛为最慢
目标 ACK 附近。

## 涉及文件

- `os/src/mm/remote_tlb.rs`
- `Docs/决赛文档/problem/buildstorm-remote-tlb-broadcast.md`
- `Docs/决赛文档/README.md`
- `Docs/决赛文档/开发日志.md`
- `Docs/决赛文档/ai.log`
- `Docs/决赛文档/AI_INTERACTION.md`

## 验证

以下编译与静态检查通过：

```bash
rustfmt --edition 2021 --check os/src/mm/remote_tlb.rs
git diff --check
make TARGET_ARCH=riscv64
make perf TARGET_ARCH=riscv64
make perf TARGET_ARCH=loongarch64
```

`make TARGET_ARCH=riscv64` 完成了 RISC-V 与 LoongArch64 的 release 构建；两条 `make perf`
命令分别完成对应架构的 perf 条件编译。输出仅含既有 user/smoltcp warnings，没有本轮错误。

本轮没有直接执行根目录 `make run`：当前工作区存在维护者保留的未跟踪 `disk.img` 符号链接，
而该目标会无条件删除并重建它。也没有运行完整 BuildStorm A/B；`server.ans` 本身尚未结束，
故没有声称具体端到端加速百分比。后续应使用独立 `/tmp` qcow2 overlay，不触碰该链接，完成
相同配置的基线与修复后 BuildStorm，并对比完成阶段的 wall-clock、`remote_tlb_remote` 和
`TPASS`/`TFAIL`/`TBROK`。
