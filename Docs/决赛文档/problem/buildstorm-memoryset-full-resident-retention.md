# BuildStorm MemorySet 全驻留帧保留导致局部 MM 更新放大

## 背景

共享地址空间支持跨 Hart 运行后，修改页表必须向所有仍在运行该地址空间的 Hart
发送 remote TLB shootdown。远端确认失效前，旧 TLB 项仍可能引用刚被解除映射或
COW 替换的物理帧，因此 `MemorySet` 更新路径会临时持有旧 `FrameTracker` 的
`Arc`，直到全部 ACK 返回后再释放。

## 现象

`server.ans` 已输出 `BUILDSTORM_TOOLCHAIN ok` 和 `BUILDSTORM_MINIBUILD ok`，随后在
非计时的 `tg-xtask` 预构建中从 Cargo `440/446` 推进到 `444/446: axbuild`，长时间
没有新的串口输出。维护者给出的旧基线中，单独编译 `axbuild` 约耗时 12 分钟。

`client.ans` 的 GDB 现场显示只有执行 rustc 的 Hart 活跃，调用栈反复落在：

```text
sys_brk -> TaskControlBlock::growproc -> MemorySet::with_mut
sys_munmap -> MemorySet::munmap -> MemorySet::with_mut
Vec<Arc<FrameTracker>>::collect -> BTreeMap::values
```

其余 Hart 处于 idle/WFI，与预构建尾部只剩一个 Cargo 编译单元相符。

## 分析

原 `MemorySet::with_mut()` 在每次页表更新前遍历所有 `MapArea`，将进程全部驻留页的
`Arc<FrameTracker>` 克隆到临时 `Vec`。这对任意未知范围的页表更新是保守且安全的，
但 rustc 已有很大的驻留集，而分配器会频繁调用 `brk`、`munmap`、`mremap` 和
`MADV_DONTNEED`。一次只改变少量页的操作因此被放大成对整个进程驻留集的扫描和
原子引用计数更新。

GDB 源码单步会在 `BTreeMap::values()` 和 `Vec::collect()` 的同一组源码行中停留很久，
看起来像迭代器死循环；但两次 `continue` 后调用栈能从 `brk` 前进到后续 `munmap`，
说明迭代器确实在推进。现场没有相互等待的锁链，也没有损坏的 BTree 节点证据，
因此这不是结构性无限循环，而是全量工作量造成的近似卡死。

## 根因

remote TLB 的帧生命周期保护粒度过大：所有局部 MM 更新都通过同一个全量
`with_mut()` 入口，在持有更新锁和 `MemorySet` 写锁期间复制整个驻留集。安全需求
实际只要求保留本次可能被解除映射或替换的旧帧；纯新增映射或权限更新不释放旧帧，
无需复制任何帧。

## 修复

- 新增 `with_retained_frames_mut()`，由调用者选择更新前需要保留的旧帧；原有
  deactivate、更新锁、MM 写锁、remote shootdown、ACK 后释放和重新 activate 的
  协议保持不变。
- 新增范围化入口，只从目标 VPN 范围的 `BTreeMap` 子区间克隆旧帧。`munmap`、
  `MADV_DONTNEED`、堆收缩、`MAP_FIXED`、`mremap` 和共享内存解除映射改用该入口。
- present-page fault 只保留故障 VPN 的旧帧；非固定 mmap、堆增长、页内堆收缩、
  `mprotect`、fork 的 COW 权限处理和纯新增内核映射使用零帧保留入口，但仍执行
  remote TLB shootdown。
- 保留全量 `with_mut()` 作为明确的整地址空间回收入口，目前只用于
  `recycle_data_pages()`。
- 将 `mremap` 的保留范围选择收进 `MemorySet` handle 层，syscall 层只负责参数与
  结果处理；`TaskControlBlock::growproc()` 同样委托给范围感知的 `MemorySet::grow()`。

涉及文件：

- `os/src/mm/memory_set/handle.rs`
- `os/src/mm/memory_set/fork_clone.rs`
- `os/src/syscall/mm/mmap.rs`
- `os/src/task/task/task.rs`

## 验证

- 目标 Rust 文件 `rustfmt` 通过。
- `git diff --check` 通过。
- `make TARGET_ARCH=riscv64` 完成 RISC-V 与 LoongArch64 release 构建。
- 最终代码再次通过 `make build-arch TARGET_ARCH=riscv64` 和
  `make build-arch TARGET_ARCH=loongarch64`。
- 使用全新 qcow2 overlay 运行 RISC-V 15 分钟：toolchain/minibuild 均通过，Cargo
  从 `440/446` 推进到 `445/446: tg-xtask(bin)`；`axbuild` 单次观测约 8 分钟，较维护者
  提供的旧版约 12 分钟基线缩短。运行期间没有 panic、`TFAIL`、`TBROK`、`EIO`、
  Cargo fatal/error 或 remote-TLB 异常。

15 分钟 timeout 时仍停在 `445/446`，没有完整 BuildStorm 结束标记，因此本次结果只能
证明已越过原 `axbuild` 慢点并降低耗时，不能报告完整 `446/446` 或端到端 BuildStorm
通过。日志中的 `extra TCB refs` 是既有的独立告警，不属于本次 MM 范围化修复。
