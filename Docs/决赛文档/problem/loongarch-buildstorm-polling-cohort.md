# LoongArch BuildStorm 12 Hart polling cohort 缺核

## 背景

LoongArch64 final 配置声明 `HART_NUM = 12`，BuildStorm 评分脚本也以 12 个 CPU 为期望。Cargo 的
`tg-xtask` 预构建会同时运行大量 Rust 编译/链接子进程，必须让 CFS 共享就绪队列能够被全部已启动
Hart 消费。

QEMU LoongArch 的架构 `idle` 指令不能可靠响应本内核用于调度唤醒的 IPI。因此架构层此前保留了一小组
polling Hart，避免所有 CPU 都因 idle 而失去唤醒机会。

## 现象

新的 `server.ans` 没有 `BUILDSTORM_COMPILE`、测试组结束、`shutdown!`、`TFAIL`、`TBROK` 或 panic。
它已经通过 toolchain/minibuild，停在未计分的：

```text
----- pre-build tg-xtask (untimed) -----
Building 444/446: axbuild
```

日志确认 12 个 Hart 均完成启动，但 `t=223544ms` 的 perf 快照为：

```text
scheduler_harts selections_by_hart=[3508,4644,4679,4665,4487,4271,4520,4538,0,0,0,1]
```

也就是说 Hart 8--11 已启动却几乎从不选择就绪任务，实际有效并行度仅约 8 核。该样本已启用独占 COW
提升，`exclusive_upgrade=210513`、`shared_frame_copy=3101`，说明此前 COW 误复制已被大量消除，不能再将
这个缺核现象归因于 COW 或 remote-TLB ACK。

## 根因

`os/src/arch/loongarch64/qemu/cpu.rs` 把 `POLLING_HARTS` 限制为 8。Hart 0--7 在 `idle()` 中短暂
spin 后返回 scheduler；Hart 8--11 则执行 `idle 0`。同一文件已经记录 QEMU LoongArch `idle` 不能可靠
地因 scheduler IPI 返回，因此后四个 Hart 可能永久停在架构 idle，无法再次从共享 CFS queue 取任务。

这与 perf 中前 8 个 Hart 有数千次 selection、后 4 个为零的分布一一对应。它也解释了为何启动信息有
12 个 CPU、BuildStorm 内的 `nproc` 预期为 12，但 `tg-xtask` 编译阶段始终无法利用全部核。

## 修复

将 LoongArch QEMU `POLLING_HARTS` 设为 `HART_NUM`。所有配置 Hart 在无任务时做原有的有限 spin，随后
回到 scheduler 并重新检查共享队列；不再让任何 CFS 可调度 CPU 落入不可靠的 `idle 0` 唤醒路径。

这个选择会增加完全空闲系统的宿主 CPU 占用。对当前 BuildStorm，高并发的编译负载远长于空闲阶段，且
评分配置明确使用 12 个 CPU，因此保证可用并行度优先。该改动只影响 LoongArch QEMU，不改变 RISC-V 或
用户态可见的调度 ABI。

## 涉及文件

- `os/src/arch/loongarch64/qemu/cpu.rs`
- `Docs/决赛文档/problem/loongarch-buildstorm-polling-cohort.md`
- `Docs/决赛文档/README.md`
- `Docs/决赛文档/开发日志.md`
- `Docs/决赛文档/ai.log`
- `Docs/决赛文档/AI_INTERACTION.md`

## 验证

已通过：

```text
rustfmt --edition 2021 --check os/src/arch/loongarch64/qemu/cpu.rs
git diff --check
make perf TARGET_ARCH=loongarch64
make perf TARGET_ARCH=riscv64
```

使用 final LoongArch64 原始镜像的独立 `/tmp/buildstorm-all-harts-smoke.qcow2` 覆盖层启动 12-Hart
perf 内核。十项 CAgent 均通过，首个 perf 快照显示：

```text
scheduler_harts selections_by_hart=[261,175,483,238,221,365,396,479,464,427,337,250]
```

全部 12 项均为正，直接验证 idle 后的 Hart 8--11 已重新参与 CFS 选择；日志也显示 CAgent 请求被安排到
Hart 8、9、10、11。运行通过 `BUILDSTORM_TOOLCHAIN ok` 与 `BUILDSTORM_MINIBUILD ok`；在 180 秒 timeout
前推进至 `Building 442/446`。第二个 `t=116892ms` 快照的 12 个 selection 值仍全部为正，说明预构建期间
没有再次退化为 8 核。该样本没有完整 `BUILDSTORM_COMPILE` 结果，也没有固定基线的完成样本，不能据此报告
BuildStorm 端到端加速比例。后续需要在相同 36 GiB、12 Hart、fresh overlay 条件下完成完整一轮，并记录
`elapsed_s`、最终结果、`scheduler_harts`、COW 和 remote-TLB 快照。
