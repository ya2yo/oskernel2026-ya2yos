# BuildStorm journal 回调与 cache flush 并发串行化

## 背景

在 checkpoint 引用和 journal 回调生命周期修复之后，RISC-V final-2026 BuildStorm 已经能够越过旧的
journal 满断言和 bcache index-lock 卡死点，但新的运行现场仍在 Cargo `431/446` 附近出现长时间无输出。
该阶段同时存在普通 transaction commit、journal checkpoint 和文件关闭触发的全局 bcache flush。

## 现象

GDB 现场曾观察到以下两条路径同时运行：

```text
jbd_journal_prepare
  -> jbd_block_set
  -> __ext4_trans_stop
  -> ext4_frename

ext4_block_cache_flush
  -> ext4_cache_flush
  -> Ext4File::file_close
  -> Ext4Inode::drop
  -> sys_unlinkat
```

并发现场最终触发：

```text
Cause: Exception(LoadPageFault)
stval: 0x75622f6563636589
sepc : 0xffffffc0803616b8
```

符号化后的 C 栈落在 `ext4_journal.c:jbd_journal_prepare()` 的 `jbd_block_set()` 附近。此前日志中的
`rmeta.../full.rmeta, rc = 2` 和 `libfutures_util...rlib: No such file` 是文件系统元数据损坏后的表现，不能按普通
编译器缺文件处理。

另一次用户中断 GDB 的唯一活跃栈在
`FilePageCache::publish_reserved_page()` 的 Rust `BTreeMap::entry` 内部，其余 hart 在 idle。重新连接并继续
执行后，BuildStorm 从 431 继续推进到 444/446；因此该次页缓存栈只是采样点，不足以证明 BTreeMap 自身死循环。

## 分析

transaction commit 和 close-time/global cache flush 都可以调用 `ext4_block_flush_buf()`，而该函数会同步进入
`jbd_trans_end_write()`。原有代码只修复了部分单线程 callback 遍历的悬空 successor，没有禁止两个入口同时修改同一
journal 的 transaction、`jbd_buf`、`jbd_block_rec` 和 checkpoint queue。callback 还可能在清理当前 descriptor 时触发
嵌套完成，导致 transaction 或 block record 在外层遍历尚未结束时被回收。

因此，431 附近的 panic 根因是 journal callback 生命周期与 cache flush 并发缺少共同的串行化边界，而不是 Cargo 的
`aws-lc-sys` 编译逻辑或 Rust 页缓存索引本身。其他 hart idle 也不能单独说明死锁：BuildStorm 当前可能只有一个用户态
构建线程可运行，必须结合连续 GDB 采样和日志进度判断。

## 修复

- `jbd_buf` 增加 checkpoint 引用释放和同步 flush 尝试标记；checkpoint/flush 遍历每次从 live queue 重新扫描，
  不跨越 callback 保存 TAILQ successor。
- transaction 增加 `checkpoint_submit_active`，journal 增加 `callback_depth`。checkpoint callback 期间禁止
  transaction/block-record 提前 purge，嵌套 callback 完成后再统一回收。
- `jbd_trans_end_write()` 只有在 bcache callback slot 仍精确指向当前 `jbd_buf` 时才清空函数和参数，避免覆盖
  已经交接给后继 transaction 的 callback。
- `ext4_block_cache_write_back()` 和 `ext4_cache_flush()` 使用统一锁序：
  `journal_lock -> cache_flush_lock -> cache_lock`。这样 close-time flush 的 callback 不会与 transaction prepare
  并发改写 journal 链表；所有返回路径都释放已取得的锁。

## 涉及文件

- `crates/lwext4_rust/c/lwext4/include/ext4_journal.h`
- `crates/lwext4_rust/c/lwext4/src/ext4_journal.c`
- `crates/lwext4_rust/c/lwext4/src/ext4_blockdev.c`
- `crates/lwext4_rust/c/lwext4/src/ext4.c`

维护者已有的 `user/src/bin/initproc.rs` 改动与未跟踪的 `disk.img` 不属于本问题。

## 验证

- `cmake --build /tmp/lwext4-debug --target lwext4-bcache-lifecycle -j2`：通过。
- `/tmp/lwext4-debug/fs_test/lwext4-bcache-lifecycle`：`lwext4-bcache-lifecycle: PASS`。
- `git diff --check`：通过。
- 已有 `make build-arch TARGET_ARCH=riscv64` 构建通过；本轮锁修改后的完整构建尚需重新执行。
- 当前 RISC-V `server.ans` 在 431 后继续到 444/446，暂未观察到本轮的 journal panic、`rmeta` 缺失或 compiler fatal
  error；日志没有 BuildStorm 完成标记，因此不能宣称 446/446、`e2fsck`、LTP 或 LoongArch64 runtime 通过。

## 后续边界

需要重新构建当前工作区并从干净 QEMU 现场完整跑完 BuildStorm，连续采样每个 hart 的内核/用户态栈，确认锁序没有
反向获取；随后补 `e2fsck -fn`、文件系统 LTP 和 LoongArch64 回归。若再次停滞，应先用两次间隔采样确认 PC 是否
保持不变，再把现场归类为死循环、锁等待或正常单线程编译耗时。
