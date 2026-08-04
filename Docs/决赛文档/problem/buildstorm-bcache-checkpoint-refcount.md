# BuildStorm bcache checkpoint 引用与 journal 回调生命周期

## 背景

RISC-V final-2026 BuildStorm 在 `server.ans` 中推进到 Cargo `Building 117/446` 后不再产生新输出。
同时，动态链接兼容层对 `/usr/lib/bfd-plugins/liblto_plugin.so` 输出
`map_dynamic_link_file cannot find DL path` warning。维护者要求消除该 warning，并使内核稳定跨过
117 crate。

## 现象

`client.ans` 中多个 hart 忙等在 `ext4_bcache_index_lock()`，持锁 hart 的调用链为：

```text
ext4_bcache_release_locked
ext4_bcache_free
ext4_block_set
jbd_journal_cp_trans
__jbd_journal_commit_trans
```

持锁路径在 `ext4_bcache_release_locked()` 的 `ext4_assert(buf->refctr)` 失败后永久自旋；其它 hart
因等待同一 index lock 同时消耗 CPU。随后用于验证的并行 BuildStorm 还暴露了两类独立 UAF panic：

- `jbd_journal_prepare()` 读取已经释放并被路径字符串覆盖的 `jbd_buf->block_rec`；
- `jbd_trans_end_write()` 经已释放的 `end_write_arg` 回调，`block_rec` 为 NULL 并在偏移 `0x48` 写入。

两次 panic 都由 RISC-V trap 记录为 kernel page fault，不能把 panic 后其它 hart 缓冲输出的
`Compiling` 当作测试继续成功。

## 分析

`jbd_trans_set_block_dirty()` 原本用裸 `ext4_bcache_inc_ref(block->buf)` 为 deferred checkpoint
额外保活。普通 owner 通过 `ext4_bcache_free()` 在 bcache index lock 内递减引用；SMP 下的裸加引用可与
递减交错并丢失更新。checkpoint 随后释放一个已经为零的引用，触发上述断言并永久持有 index lock。

journal 的清理路径还存在两个生命周期问题：

1. `jbd_journal_prepare()` 的 `TAILQ_FOREACH_*_SAFE` 预先缓存相邻节点；清理 clean buffer 所触发的
   rollback/writeback callback 可以同时释放这个相邻 `jbd_buf`，下一轮解引用成为 UAF。
2. `jbd_trans_finish_callback()` 以 `buf == NULL` 直接完成 descriptor 时，会调用
   `jbd_trans_end_write()`。旧代码只在 `buf != NULL` 时清理 bcache 的 `end_write`/`end_write_arg`，因而
   留下指向已释放 `jbd_buf` 的回调槽；后续 writeback 再次调用该槽即使用悬空对象。

`/usr/lib/bfd-plugins/liblto_plugin.so` 则是 Binutils 以绝对路径加载的 native linker plugin，不是
Ya2yOS 兼容 DSO store 应按 basename 改写的 legacy library 路径。对其进入 fallback 只制造无意义 warning，
并可能掩盖真实的 `ENOENT`。

## 修复

- 新增 `ext4_bcache_retain()`：在 index lock 内确认 buffer 仍被引用并增加 checkpoint 引用；
  `jbd_trans_set_block_dirty()` 使用它替代裸 `refctr++`，并在后续 block-record 分配失败时归还引用。
- `jbd_journal_prepare()` 在会触发 callback 的 clean-buffer 分支后重新取得前/后继，不使用可能已失效的
  `TAILQ_FOREACH_*_SAFE` 相邻节点快照。
- `jbd_trans_end_write()` 以 callback 槽的函数和参数都仍精确等于当前 `jbd_buf` 为条件清理槽位；这既覆盖
  `buf == NULL` 的直接完成路径，也不会清除已转交给后继 transaction 的新 callback。
- 动态链接映射把 `/usr/lib/bfd-plugins/` 识别为 native toolchain plugin 路径，直接返回原绝对路径，保留
  成功打开和真实 `ENOENT` 给调用者处理。
- host bcache 生命周期测试新增 checkpoint retain 场景：owner 先释放、checkpoint 引用后释放，两个阶段均校验
  cache 不变量。

## 涉及文件

- `os/src/fs/map_dynamic_link.rs`
- `crates/lwext4_rust/c/lwext4/include/ext4_bcache.h`
- `crates/lwext4_rust/c/lwext4/src/ext4_bcache.c`
- `crates/lwext4_rust/c/lwext4/src/ext4_journal.c`
- `crates/lwext4_rust/c/lwext4/fs_test/bcache_lifecycle.c`

## 验证

```text
make build-arch TARGET_ARCH=riscv64
  -> PASS

cmake --build /tmp/lwext4-bcache-retain --target lwext4-bcache-lifecycle -j2
/tmp/lwext4-bcache-retain/fs_test/lwext4-bcache-lifecycle
  -> lwext4-bcache-lifecycle: PASS

timeout 900s make run TARGET_ARCH=riscv64
  -> BuildStorm 从 0/446 连续推进至 131/446 后按验收窗口人工终止
```

最终 RISC-V 运行日志在 `0..131/446` 未出现 `map_dynamic_link_file`、`liblto_plugin.so` warning、
`ext4_assert`、`panic`、`EIO`、`TFAIL`、`TBROK` 或 compiler fatal error。测试按需求证明稳定超过
117 crate；没有运行完整 446 crate、`e2fsck -fn`、文件系统 LTP 或 LoongArch64 guest runtime。
