# iozone 连续 random-read/backward-read 的 inode 缓存复用卡死

## 背景

`iozone 3.506` 的子测例已拆分到 `user/src/bin/iozone/`。为稳定复现，
`initproc` 当前顺序调用 `random_read::run_musl()` 和
`backward_read::run_musl()`。两个测例单独运行均正常，但连续运行时会在
iozone cleanup 后卡死。

## 现象

random-read 的四个 random writer 已结束，`sync()` 与两次 `nanosleep(2)` 均返回；
cleanup worker PID 19、20 成功执行 `fstatat()` 与 `unlink()`。随后父进程创建
PID 21 的 `/proc/21/stat` 时停滞。

GDB 显示 hart 1 位于：

```text
ext4_fread
  -> Ext4File::check_cached
  -> Ext4File::file_seek
  -> Ext4Inode::write_at
  -> create_proc_dir_and_file
  -> TaskControlBlock::clone_process
```

PC 落在 `ext4_fread+164` 的自跳转指令。关键临时日志为：

```text
proc create pid=21 before stat write \
  requested=/proc/21/stat inode_path=/musl/iozone.DUMMY.1
```

也就是说，`open("/proc/21/stat")` 返回了仍绑定到已删除
`/musl/iozone.DUMMY.1` 的 `Ext4Inode`。`file_seek()` 因而以错误路径查缓存，
无法命中 task proc 文件的 cache bypass，最终进入 `check_cached()->ext4_fread()`。

## 分析

原 `FsIndex` 用两张独立表保存：

```text
PATH_INDEX:  path -> (st_dev, st_ino)
INODE_CACHE: (st_dev, st_ino) -> Arc<dyn Inode>
```

iozone 的同名 DUMMY 文件在不同阶段重新打开/重建时，`PATH_INDEX.insert()` 会用
新 key 覆盖路径映射，但不会清理旧 `INODE_CACHE` 强引用。旧 canonical inode
成为无路径引用的孤儿项。unlink 只能删除路径最后一次绑定的 key；ext4 后续复用
更早的 orphan key 给 `/proc/21/stat` 时，FsIndex 错误返回 DUMMY canonical inode。

另外，`file_remove()` 原先只删除 `CACHE_TABLE`，没有清理 `FIFO_TABLE`/
`FIFO_SET`。同名文件重建后可能不能重新入队；FIFO 满时若在队列锁内进入 lwext4
写回，异常清理还可能重入同一自旋锁。

## 根因

一级根因是 FsIndex 的 inode identity 生命周期不一致：同一路径 key 更新遗留强
引用 canonical inode，unlink 无法回收所有历史 key，导致 inode 号复用后新路径
共享已删除文件的 VFS inode 对象。`check_cached()` 复用活跃 descriptor 并且不检查
缓存打开失败，使该错误进一步演变为 lwext4 读路径自旋。

## 修复

- 将路径索引和 inode 缓存收敛为单个 `InodeCacheState` 锁，原子发布、重绑与删除。
- `bind_path()` 覆盖同一路径 key 时，在旧 key 不再被任何路径引用后立即回收其
  canonical inode；unlink 只在没有其他 hard-link path 时回收 canonical。
- 插入同 key inode 前在索引锁外验证旧 canonical 仍有效；失效时撤销该 key 的全部
  旧路径映射，再发布 replacement，避免旧路径被路由到新 inode。
- `/proc/<数字 pid>` 子树使用 path key，避免与普通文件可复用的 inode 号归并；
  `cache_path_alias()` 也移到索引锁释放后执行。
- task proc runtime 文件绕过 delayed write-back cache；`check_cached()` 使用独立
  `ext4_file` 描述符，打开失败立即返回，不再污染活跃 descriptor 或调用 `fread`。
- 删除、缓存阈值回退与 FIFO 淘汰统一清理 cache/FIFO 元数据；FIFO 只在队列锁内
  摘下 victim，随后在锁外写回已摘下的 cache。
- `waitpid`/`waitid` 在 procfs/PID 表清理前释放父 `ProcessMeta`，避免将进程元数据
  锁带入文件系统路径。

## 涉及文件

- `os/src/fs/kernel_fs_ops/fsidx.rs`
- `crates/lwext4_rust/src/file.rs`
- `os/src/syscall/task/wait.rs`
- `user/src/bin/initproc.rs`
- `user/src/bin/iozone/`

## 验证

```bash
make build-arch TARGET_ARCH=riscv64
timeout 180s make run TARGET_ARCH=riscv64 > log.ans 2>&1
make build-arch TARGET_ARCH=loongarch64
timeout 180s make run TARGET_ARCH=loongarch64 > /tmp/iozone-loong.log 2>&1
git diff --check
```

- RISC-V 最终 `log.ans` 出现两次 `iozone test complete.`、
  `iozone throughput read-backwards measurements` 和最终 `shutdown!`，未出现
  `panic`、`TFAIL`、`TBROK` 或 `ERROR`。
- LoongArch64 最终 `/tmp/iozone-loong.log` 同样完成两项测例并输出 `shutdown!`。
- 两个架构 release 构建均通过；仅有项目既有 Cargo config 弃用提示和 vendored
  `smoltcp` warning。

## 剩余风险

`Ext4File::file_open(path, flags)` 的跨路径调用语义仍应谨慎使用。当前常规
`Ext4Inode` 均以自身 live path 打开；未来若要复用同一个 `Ext4File` 跨路径操作，
需要单独定义关闭旧 descriptor、更新路径和缓存失效的完整语义。
