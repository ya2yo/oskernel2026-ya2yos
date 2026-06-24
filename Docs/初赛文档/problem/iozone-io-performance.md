# iozone 文件 I/O 性能优化

## 背景

当前测试入口单跑 LoongArch `iozone-musl`。iozone 的自动测试包含 `./iozone -a -r 1k -s 4m`，吞吐测试包含多个 `-t 4 -r 1k -s 1m` 子项。两类负载都会产生大量小块顺序读写、随机读写和重复读写。

## 现象

优化前观察到 `iozone -a -s 4m` 自动测试中 4MiB 文件不进入 lwext4 的 `VFileCache`，多数操作直接落到底层 ext4/virtio 路径；同时 Disk 层对对齐的大缓冲仍按 512B 块循环调用 `read_block/write_block`，无法利用 virtio 驱动已经支持的连续块 I/O。

120 秒窗口内旧路径可以推进到 pwrite/pread 子项，但尚未完整跑到 GROUP END。自动测试首行可见读写吞吐较低，例如上一轮输出中 `write/read` 约为 `1244/2222 kB/sec`。

## 分析

`crates/lwext4_rust/src/blockdev.rs` 的 `dev_bread/dev_bwrite` 会把 lwext4 请求转换成一段连续字节缓冲，并调用 `KernelDevOp for Disk`。`os/src/fs/ext4_lw/sb.rs` 中的 `KernelDevOp::read/write` 循环调用 `Disk::read_one/write_one` 直到填满缓冲。

旧版 `Disk::read_one/write_one` 在 `offset == 0 && len >= 512` 时也只处理一个 512B 块，然后返回给上层继续循环。virtio block 驱动的 `read_block/write_block` 注释和实现都支持一个 buffer 覆盖多个连续块，因此这里存在不必要的 per-block 调用开销。

此外，lwext4 的 `MAX_CACHED_FILE_SIZE` 原为 1MiB。它能覆盖 `-s 1m` 的吞吐子项，但无法覆盖 `-s 4m` 的自动测试文件。iozone 自动测试正好是 4MiB，适合扩大到 4MiB，而不需要扩大 FIFO 数量。

## 根因

1. Disk 层没有批量提交连续对齐块，导致大量 1KiB/4KiB 读写被拆成多次 512B virtio 请求。
2. lwext4 小文件缓存阈值低于 iozone 自动测试文件大小，4MiB 自动测试无法复用现有 VFileCache。

## 修复

涉及文件：

- `os/src/drivers/disk.rs`
- `crates/lwext4_rust/src/file.rs`

修改内容：

1. `Disk::read_one()` 在块对齐且 buffer 至少 512B 时，一次计算 `buf.len() / 512 * 512` 的连续块长度，并直接调用一次 `read_block()`。
2. `Disk::write_one()` 做同样的连续块批量写，非对齐或不足 512B 的路径保留原来的读改写逻辑。
3. `MAX_CACHED_FILE_SIZE` 从 1MiB 提高到 4MiB，仅覆盖 iozone `-s 4m` 等小文件场景；FIFO 大小不变，避免无界扩大缓存占用。

## 验证

已执行：

```text
make
```

结果：LoongArch64 release 构建通过。

已执行：

```text
timeout 180s make run
```

结果：当前入口运行 `iozone-musl`，完整输出 `#### OS COMP TEST GROUP END iozone-musl ####` 和 `shutdown!`，未出现 panic 或 OOM。

性能信号：

- `iozone -a -r 1k -s 4m` 自动测试中，4MiB 文件进入 cache 后，`rewrite/read/reread/random read/random write/backward read/record rewrite/stride read/fwrite/frewrite/fread/freread` 等项目明显提高。
- 本轮自动测试首行输出为：`write 563, rewrite 31532, read 9488, reread 10072, random read 8477, random write 20146, backward read 8667, record rewrite 19231, stride read 8328, fwrite 24006, frewrite 22840, fread 5640, freread 5763 kB/sec`。
