# lwext4 journal 满时断言自旋导致 BuildStorm 卡死

## 背景

final-2026 的 EXT4 镜像 journal 大小为 32 MiB（4 KiB block 下为 8192 个 block）。BuildStorm 的并行
Cargo 编译会同时产生大量临时文件、rename 和 write-back；一次 dense cache 回写及其元数据、descriptor、revoke、
commit block 都会占用 journal 环形日志空间。这个负载与普通的小文件或单进程写入不同，会持续把 checkpoint 队列推到
空间边界。

## 现象

旧的 `server.ans` 在 BuildStorm 编译中停止产生新输出。配套 `client.ans` 的 GDB 现场将一个 hart 定位到
`jbd_journal_alloc_block()`；反汇编表明其地址落在 lwext4 `ext4_assert()` 打印之后的自跳转：

```asm
jal printf
j   <same address>
```

因此该现象不是 scheduler idle、hart-id 或普通 trap，而是内核态的永久自旋。旧分配器先消费 journal 的最后一个
slot，再要求 checkpoint 必须立即推进 `journal->start`；在 checkpoint 仍排队时触发断言，整机不再有新的串口输出。

## 根因

`jbd_journal_alloc_block()` 原先以 `last` 作为本次块地址后立即前移 `last`。当环形日志空间耗尽时，它强制执行一次
`jbd_journal_purge_cp_trans(..., true, true)`，随后以：

```c
ext4_assert(journal->last != journal->start);
```

要求 `start` 已前进。

但 `last == start` 在环形日志中具有双重含义：既可表示所有 checkpoint 完成后的空日志，也可表示刚刚消费最后一个空闲
slot 后的满日志。BuildStorm 的 deferred checkpoint 在强制调用返回时仍可能尚未回收最旧 transaction，因此这一条件并不
是可由断言保证的不变量。`ext4_assert()` 的实现又是无限循环，使原本应由 transaction abort 处理的资源不足变为整机卡死。

## 修复

修改集中在以下 lwext4 路径：

- `crates/lwext4_rust/c/lwext4/src/ext4_journal.c`
  - 新增 `jbd_journal_next_block()` 和 `jbd_journal_make_room()`；在交付最后一个可用 slot 前预留空间。
  - 接近边界时先强制 checkpoint，再执行一次非 I/O checkpoint 回收，以接住强制写回期间已经完成的 transaction。
  - 空间仍不足时返回 `EIO`，让既有 transaction abort/cleanup 路径收尾，不再调用会永久自旋的 `ext4_assert()`。
  - 将 `jbd_journal_alloc_block()` 改为可失败接口，并在 descriptor、data、revoke 与 commit block 四类分配点传播错误。
- `crates/lwext4_rust/c/lwext4/src/ext4.c`
  - 创建、写入、目录和 metadata 等调用点保存并向上传播 `ext4_trans_stop()` 的返回值，避免 journal commit 失败后仍向
    上层报告操作成功。
- `crates/lwext4_rust/src/file.rs`
  - 将 dense whole-file write-back cache 的单文件上限从 16 MiB 收紧到 4 MiB，避免单次写回过度逼近 32 MiB journal
    的容量；这只是降低压力，不能替代 journal 空间不足时的正确错误处理。

曾试验在 C `ext4_fwrite()` 内强制按 1 MiB 拆分 transaction。该方案会增加 checkpoint 交错，在
`jbd_journal_prepare()` 暴露既有 `jbd_buf` 生命周期/链表问题并触发新 panic，已从当前工作树撤回，不能作为本修复或
长期策略。

## 验证

- `make build-arch TARGET_ARCH=riscv64` 已完成；仅有既有 `smoltcp` unused import/dead code 警告。
- 最新 RISC-V `server.ans` 从 BuildStorm 开始连续推进，最后可见进度为 `Building 117/446`，已经越过旧的 journal
  assertion 无输出停滞区间。
- 新 `client.ans` 没有 hart 停在旧 `ext4_assert()` 自跳转。其活跃栈仍覆盖 `ext4_bcache_index_lock()`、
  `ext4_bcache_release_locked()`、`jbd_journal_cp_trans()` 和 `ext4_fwrite()`，说明编译后期仍在文件系统读写与
  checkpoint 相关路径执行。
- 新日志未出现本问题对应的 journal assertion/panic；但没有 `BUILDSTORM_COMPILE ... ok`、完整测试结束或
  `shutdown!`，因此不能据此宣称 BuildStorm 全量通过。

## 当前边界与后续工作

`server.ans` 在 `111/446` 附近开始出现 `map_dynamic_link_file cannot find DL path for
/usr/lib/bfd-plugins/liblto_plugin.so` 警告；这不是 journal assertion 的复现证据，也尚未确认是否会阻断后续编译。
同时，GDB 中的 bcache index/release 和 journal checkpoint 栈只能说明当前工作负载所在位置，尚不足以把它们定性为新的
死锁或内存安全根因。

下一轮应依据新的完整输出分别处理这些独立问题，并补充：

1. BuildStorm 从 `117/446` 继续运行后的首个确定性失败、超时或 GDB 停滞现场；
2. `liblto_plugin.so` 动态链接路径警告的兼容性与实际影响；
3. journal 返回 `EIO` 后 create/rename/write 的 Linux 可见错误语义、文件系统一致性和 `e2fsck -fn`；
4. 文件系统 LTP、完整 RISC-V BuildStorm 和 LoongArch64 回归。

在上述验证完成前，本问题的结论仅限于：已消除 journal 满时由断言造成的永久 CPU 自旋，并已观察到 BuildStorm 推进至
`117/446`；不等同于端到端正确性、性能或跨架构通过。
