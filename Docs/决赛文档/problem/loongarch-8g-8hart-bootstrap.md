# LoongArch QEMU 8GiB/8 核启动与分段内存

## 背景

评测机的 LoongArch QEMU 使用 `-m 8G -smp 8`。仅把
`make_scripts/loongarch64.mk` 的启动参数改为该值不足以启用内核侧的内存和多核能力：内核原先仍按
2GiB、单 hart 建模。

## 现象

原有龙芯实现存在以下单核/小内存假设：

- `HART_NUM = 1`，`hart_id()` 固定返回 0；
- QEMU 直接启动时只让 CPU0 进入内核，其他 CPU 在 flash slave boot ROM 中等待 mailbox/IPI，
  内核没有发送启动请求；
- `PHYSICAL_MEMORY_RANGES` 只描述低端 256MiB 和高端 1792MiB，因此 CMA 即使面对 `-m 8G`
  也只会纳管 2GiB；
- 新建 LoongArch 进程总是固定到 hart 0，`getcpu()`、`sched_getaffinity()` 和
  `/proc/cpuinfo` 也继续向用户态暴露单核信息。

## 分析

QEMU 9.2 LoongArch `virt` 的 RAM 被 PCI/MMIO hole 分割：低端范围固定为
`[0x0000_0000, 0x1000_0000)`，其余内存从 `0x8000_0000` 开始。8GiB 配置的高端范围因此为
`[0x8000_0000, 0x2_7000_0000)`，长度 `0x1_f000_0000`。

QEMU direct boot 将 CPU0 置于高半 ELF entry；次核则在 flash slave boot ROM 中等待 mailbox 0
的 entry 地址和 IPI vector 0。将 `_start` 的高半 ELF 地址写入 mailbox 后，QEMU 的直接地址转换会
与 CPU0 一样落到物理内核镜像。

入口汇编虽然早期把 CPUID 放入 `$tp`，但 trap 返回会从用户 TrapContext 恢复 `$tp`。长期使用
`$tp` 作为 hart ID 会把用户 TLS 误认成 CPU 编号，因此运行期必须直接读取 LoongArch `CPUID` CSR。

## 修复

- 将 QEMU 默认参数同步为 `-m 8G -smp 8`，保留 `-snapshot`。
- 将 LoongArch `HART_NUM` 和 bootstrap stack 数量设为 8；物理 RAM 描述为低端 256MiB 加高端
  7936MiB。CMA 已有的分段遍历逻辑会将两段加入同一个 allocator，不会分配 PCI/MMIO hole。
- `hart_id()` 改读 `CPUID` CSR。bootstrap hart 完成全局初始化后，通过
  `csr_mail_send(entry, hart, 0)` 和 IPI vector 0 启动 7 个 AP。
- 新建独立进程按 `(pid - 1) % HART_NUM` 分配 home hart；同一地址空间内的线程继续固定在同一
  hart。`getcpu()`、`sched_getaffinity()` 和 `/proc/cpuinfo` 同步反映实际拓扑。

## 边界

当前调度器尚未实现运行中任务迁移、远程 TLB shootdown 或通用 reschedule IPI。因此
`sched_getaffinity()` 诚实返回目标进程的单一 home-hart bit，`sched_setaffinity()` 只接受包含该 bit
的请求而不迁移任务。`/proc/cpuinfo` 展示已安装的 8 个 hart；不应据此推断任一进程可迁移到全部
hart。

## 验证

执行：

```text
make loongarch64-build
timeout 60s make run TARGET_ARCH=loongarch64
git diff --check
```

LoongArch64 release 构建通过。真实 QEMU 启动日志确认 CMA 范围为：

```text
from: 0x9000000080000000
size: 0x1f0000000
to:   0x9000000270000000
```

并打印 hart 1 至 hart 7 的启动消息。网络基础回归为 `Summary: netdev passed 4 failed 0`，
`basic-musl` 和 `basic-glibc` 均输出 `GROUP END` 及 `shutdown!`；未见 `panic`、`TFAIL` 或
`TBROK`。展开的 QEMU 命令包含 `-m 8G -smp 8 ... -snapshot`。
