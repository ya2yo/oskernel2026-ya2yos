# BuildStorm uaccess 缺页路径递归获取任务锁

## 背景

P3 为当前地址空间的短小用户缓冲区增加了 fault-safe uaccess。直接 load/store 发生
S-mode page fault 时，内核需要修复缺页或跳转到 copy helper 的 fixup 标签。

## 现象

旧 `client.ans` 的 CPU#1 停在：

```text
SpinMutex::lock -> TaskControlBlock::inner_lock
  -> os::mm::uaccess::handle_kernel_fault
  -> os::trap::trap_from_kernel_frame
```

其余 Hart 均在 idle，日志没有 panic；该自旋不会自行结束。修复后的 `server.ans` 已通过
`sigaltstack`、`rseq` 回归并进入 `BUILDSTORM_BEGIN mode=multi`。修复后的新 `client.ans`
只显示 VirtIO 磁盘读取和 `sys_read` 大块缺页分配，没有再出现 uaccess/任务锁栈。

## 分析

fault handler 在 `MemorySet::handle_page_fault()` 返回后，为了复用用户态 fault 的
“同一 VPN 只重试一次”状态，再次调用当前任务的 `inner_lock()`。旧回溯在 trap frame
处停止，不能从日志确定具体 syscall；但这是从当前内核路径同步陷入的 fault，RISC-V
trap 入口不会切换任务，而其余 Hart 都 idle，因此该获取发生在同一 Hart 的嵌套路径中，
形成自锁，而不是正常的跨 Hart 锁竞争。

## 根因

`os/src/mm/uaccess.rs` 将用户态 fault handler 的 `TaskControlBlockInner` 状态复用到内核
uaccess fault 路径，违反 `os/src/task/mod.rs` 规定的锁序：uaccess 不能在资源/缺页处理
过程中反向获取任务锁，也不能假设调用者已经释放任务锁。

## 修复

- 在每个 Hart 的 `UaccessState` 中增加原子 `retry_vpn`，以 `compare_exchange` 实现同一
  VPN 的一次重试，不再访问 `TaskControlBlockInner`。
- `Scope::enter` 为每次复制清空重试状态，`Drop` 恢复嵌套 scope 的旧状态；成功修复缺页
  后清空状态，保持下一次 fault 的语义。
- 保留现有 `MemorySet` active-hart 检查、缺页/COW 修复、TLB 失效以及 fixup 返回 `EFAULT`
  的行为；用户态 trap 的任务级重试状态不变。

## 涉及文件

- `os/src/mm/uaccess.rs`

## 验证

- `make TARGET_ARCH=riscv64`：RISC-V 与 LoongArch64 release 构建均通过。
- `git diff --check`：通过。
- 新 `server.ans`：`sigaltstack regression: PASS`、`rseq regression: PASS`，进入
  `BUILDSTORM_TOOLCHAIN ok`、`BUILDSTORM_MINIBUILD ok` 和 `BUILDSTORM_BEGIN mode=multi`，
  未出现 `panic`、`TFAIL` 或 `deadlock`。
- 新 `client.ans`：活跃 Hart 位于 VirtIO/EXT4 读取和 `sys_read -> handle_page_fault`，
  其余 Hart idle；这属于 I/O/大块缺页耗时，不能作为死锁结论。日志尚未包含完整
  BuildStorm END。
