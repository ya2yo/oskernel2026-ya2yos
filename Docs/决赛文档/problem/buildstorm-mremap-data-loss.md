# BuildStorm Rustc `mremap(MREMAP_MAYMOVE)` 扩容丢失数据

## 背景

RISC-V BuildStorm 的 `buildstorm_xtask_prebuild_debug.sh` 会在 `/work/tgoskits`
执行 `cargo build -p tg-xtask`。Cargo 编译依赖 `unicode-ident 1.0.24` 时，Rustc 会把
`tables.rs` 读入可增长的匿名私有映射，并通过 `mremap(MREMAP_MAYMOVE)` 扩容该缓冲区。

## 现象

根目录原始 `log.ans` 在 `unicode-ident-1.0.24/src/tables.rs:164` 输出：

```text
error: expected expression, found `=`
 --> .../unicode-ident-1.0.24/src/tables.rs:164:55
164 | pub(crate) static LEAF: Align64<[u8; 7808]> = Align64([
                                                    ^ expected expression
```

`BUILDSTORM_DEBUG_XTASK_PREBUILD done` 不能作为 Cargo 成功的证据：诊断脚本的
`cargo build` 后有 `|| true`，即使编译器返回失败也会继续输出该标记。

临时诊断首先排除了镜像或依赖文件损坏：guest 镜像、宿主 Cargo 缓存中的 `tables.rs`
SHA-256 一致，原文件和复制到 `/tmp` 的文件都含有合法的第 164 行。两个 guest 直接
`rustc` 调用均复现解析错误，而宿主编译同一份源码正常。

随后按 PID 跟踪 syscall。Rustc 读取 `tables.rs` 得到 63369 字节，建立长度为 `0x21000`
的匿名私有映射，连续两次调用 `mremap` 把它扩至 `0x41000` 和 `0x81000`，随即出现上述
parser 错误。两个独立 Rustc 进程的顺序完全一致，因此问题位于读取后的用户地址空间内容，
不是源码文本本身。

## 根因

旧的 `sys_mremap()` 对 `MREMAP_MAYMOVE` 先调用 `munmap(old_addr, old_len)`，释放旧 VMA
中的 resident PTE 和页帧，再按新长度创建一个空的 `mmap` VMA。它没有迁移或复制旧映射
的任何内容。

Rust allocator 按 Linux 语义假定扩容后的映射保留旧区间内容。该实现却让已经读入的
`tables.rs` 缓冲区在第一次扩容后变为新映射的空页/延迟页，后续 Rust parser 把错误内容
当成源码，才在看似正常的 `=` 位置报语法错误。

## 修复

`MemorySetInner::mremap_maymove()` 现在只迁移完整的 `MapAreaType::Mmap` 私有 VMA，且
整个操作在同一个 `MemorySet` 写锁中完成：

1. 等长请求直接返回原地址；变长请求在旧 VMA 仍存在时选择不相交的目标地址。
2. 以原 VMA 元数据创建目标 VMA，扫描源 VMA 已驻留的 PTE；每个源页先固定对应的
   `FrameTracker`，再分配目标页并复制物理页内容。
3. 只要任一目标页分配或复制失败，就解除刚建立的目标映射并回滚目标 VMA，旧 VMA 和
   原页帧均保持不变。
4. 全部 resident 页准备成功后，才卸载旧 VMA、安装目标 VMA、更新 mmap 计数并刷新 TLB。

私有文件映射会保留原有文件元数据和 lazy fault 语义；已驻留的私有页复制到新 VMA，尚未
驻留的页继续按原映射按需加载。这样既不丢失匿名缓冲区，也不把未访问的私有文件页提前
materialize。

syscall 入口同时收紧了参数与 flag 处理：非法 flag、非规范用户地址、未页对齐地址、零长度
或页对齐后的长度溢出返回 `EINVAL`；成功迁移后才清理旧地址的坏地址记录。

## 当前边界

- 仅支持完整 `MapAreaType::Mmap` 的 `MAP_PRIVATE + MREMAP_MAYMOVE` 迁移。
- `MAP_SHARED` 和 `MAP_SHARED_VALIDATE` VMA 返回 `ENOSYS`。现有共享映射的共享组状态尚未
  具备安全重定位语义，不能把它们错误地当作私有映射移动。
- `MREMAP_FIXED`、`MREMAP_DONTUNMAP` 及没有 `MREMAP_MAYMOVE` 的请求尚未实现，返回
  `ENOSYS`；`MREMAP_FIXED` 缺少 `MREMAP_MAYMOVE` 时按 Linux 参数约束返回 `EINVAL`。
- 本修复不实现 Linux 的原址扩容、部分 VMA 重映射或 shared mapping 迁移。

## 涉及文件

- `os/src/syscall/mm/mmap.rs`
- `os/src/mm/memory_set/mmap_ops.rs`
- `Docs/ya2yos/chapters/04-memory.typ`
- `Docs/决赛文档/problem/buildstorm-mremap-data-loss.md`
- `Docs/决赛文档/problem/README.md`
- `Docs/决赛文档/开发日志.md`
- `Docs/决赛文档/ai.log`
- `Docs/决赛文档/AI_INTERACTION.md`

## 验证

已通过：

```text
make build-arch TARGET_ARCH=riscv64
make build-arch TARGET_ARCH=loongarch64
make log TARGET_ARCH=riscv64
```

RISC-V QEMU 使用临时 Rustc probe 运行：

```text
BUILDSTORM_DEBUG_UNICODE_ORIGIN rc=0
BUILDSTORM_DEBUG_UNICODE_COPY rc=0
```

采集范围内未出现 `expected expression, found '='` 或 `could not compile unicode-ident`，说明
原文件和复制文件的直接 Rustc 编译都已跨过此前的确定性解析失败。外层
`timeout 180s make run TARGET_ARCH=riscv64` 在 Cargo 继续扫描 workspace 时到期，因而
本轮不把完整 `cargo build -p tg-xtask` 或完整 BuildStorm 标记为通过。

上述 probe 仅用于定位与运行时验证，随后已从 initfiles 诊断脚本删除，不属于最终修改。
