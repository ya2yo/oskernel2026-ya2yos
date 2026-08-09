# BuildStorm 独占 COW 页被误判为共享复制

## 背景

BuildStorm 的 `axbuild`/rustc 阶段频繁 fork、写时复制和分配大块临时内存。共享地址空间的
真实 COW split 会替换 PTE 的物理页号，必须在旧 `FrameTracker` 释放前完成 active remote
Hart 的 TLB ACK；但独占 COW 页只需将同一 PPN 的 PTE 恢复可写，不能按真实 split 处理。

## 现象

最新 LoongArch64 `server.ans` 停在 `Building 444/446: axbuild`，尚未出现
`BUILDSTORM_COMPILE`、测试组结束或 `shutdown!`。末尾 perf 快照
`t=1148846ms` 已显示：

- `cow=815133`，远端 shootdown 为 `598852`，目标 Hart 合计 `1249659`；
- `remote_tlb_remote` 累计 `72876665us`；
- per-target `remote_tlb_mailbox_wait` 累计 `125105607us`，ACK latency 为
  `95474756us`。

前一轮的 broadcast-then-collect 已消除同一次请求对目标 ACK 的串行等待，但该样本仍持续
产生大量 COW 相关远端同步，说明问题不只是 IPI 发射顺序。

## 根因

`MemorySet::handle_page_fault()` 的 present-PTE 慢路径先克隆故障 VPN 的
`Arc<FrameTracker>`，以便在远端 TLB ACK 前保活可能被替换的旧帧。随后 RISC-V 和
LoongArch64 页表的 COW handler 以 `Arc::strong_count(frame)` 判断源页是否已经独占。

该临时保活引用在判断之前已经把原本 `strong_count == 1` 的独占页提高到至少 2。页表 handler
因此永远走“分配新 frame、复制整页、替换 PPN”的分支，而不是仅清除 COW 位并恢复 writable/
dirty 位。外层随后又将所有 present fault 当作需要 shootdown，连仅更新 dirty 位的异常也会
进入 `UPDATE_LOCK` 和远端 ACK 路径。

这既额外复制 4 KiB 页，也人为制造了需要远端失效的 PPN 替换，是当前 `axbuild` 慢阶段 COW
计数与 mailbox 等待被放大的直接原因。

## 修复

- `MemorySetInner::cow_fault_requires_frame_copy()` 在持有 address-space write lock、且尚未
  克隆旧 frame 前检查 COW PTE、对应 VMA 和原始 `Arc::strong_count`。
- 独占 COW 页、已有 writable PTE 的 dirty-bit 恢复及其他不替换 PPN 的 present fault 只持有
  `MemorySet::inner` write lock。远端 stale TLB 对同一 PPN 仍是更严格的旧权限；远端 store
  会本地 fault、观察新 PTE 并本地失效后重试，不能访问错误 PPN。
- 仅当 COW 源页实际共享，或兼容历史 forked-brk 无 frame tracker 的保守 copy 路径时，按原
  `UPDATE_LOCK -> MemorySet write -> retain old frame -> shootdown ACK -> release` 顺序执行。
  获取 `UPDATE_LOCK` 后会重新判断，覆盖其他 writer 已先完成该 fault 的竞争。
- LoongArch `PageTable` 补齐与 RISC-V 一致的 `is_cow_page()` 查询；perf 新增
  `cow_fault_resolution exclusive_upgrade` 与 `shared_frame_copy` 聚合计数。
- `memory_set/mod.rs` 的锁序说明同步限定为“替换或回收 PPN”的更新。仅放宽同 PPN 的本地
  permission/dirty-bit 更新，不允许持 `MemorySet` guard 再获取 `UPDATE_LOCK`。

## 涉及文件

- `os/src/mm/memory_set/{mod.rs,handle.rs,pagefault.rs}`
- `os/src/arch/loongarch64/qemu/page_table.rs`
- `os/src/utils/perf/{scheduler.rs,report.rs}`
- `Docs/决赛文档/problem/buildstorm-cow-exclusive-frame-promotion.md`

## 验证

以下检查通过：

```text
rustfmt --edition 2021 --check os/src/mm/memory_set/mod.rs os/src/mm/memory_set/handle.rs os/src/mm/memory_set/pagefault.rs os/src/arch/loongarch64/qemu/page_table.rs os/src/utils/perf/scheduler.rs os/src/utils/perf/report.rs
git diff --check
make perf TARGET_ARCH=loongarch64
make perf TARGET_ARCH=riscv64
```

使用 final LoongArch64 原始镜像的独立 `/tmp/cow-exclusive-promotion-smoke.qcow2` 覆盖层运行
12 Hart perf 内核：十项 CAgent 全部 `pass`，随后输出 `BUILDSTORM_TOOLCHAIN ok` 与
`BUILDSTORM_MINIBUILD ok`。首个 perf 快照为：

```text
[perf] remote_tlb shootdowns=134 local_only=134 remote=0 target_harts=0 acknowledgements=0 ... cow=72 ...
[perf] cow_fault_resolution exclusive_upgrade=60 shared_frame_copy=72
```

这直接证明此前会被临时保活误判的独占 COW 已走本地权限恢复，同时真实共享页仍保留 copy
路径。该短样本在 `pre-build tg-xtask` 的 dependency graph 阶段由宿主运行环境结束，未获得
446/446 完成标记或可比 wall-clock；不得以此声明完整 BuildStorm 的端到端加速百分比。后续
应在固定 overlay、相同 12 Hart、无外部短 timeout 的条件下完成修复前后两轮，并比较完整阶段
wall-clock、`cow_fault_resolution`、`remote_tlb_remote` 和最终 `TFAIL`/`TBROK`/`shutdown!`。
