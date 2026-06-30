# write06: O_APPEND 写入未在每次 write 前定位到 EOF

## 背景

LTP `write06` 验证 `write(2)` 与 `O_APPEND` 的语义。测试流程为：

- 创建并写入 2KiB 普通文件。
- 关闭后以 `O_RDWR | O_APPEND` 重新打开。
- 手动 `lseek(fd, 1KiB, SEEK_SET)` 把当前 offset 移到文件中间。
- 再写入 1KiB 数据。
- 期望写入发生在文件末尾，最终 offset 和文件大小都为 3KiB。

Linux `O_APPEND` 要求每次 `write(2)` 前都把文件 offset 定位到 EOF，并且 offset 调整与写入作为一个原子步骤完成。

## 现象

新的 `log.ans` 中 musl/glibc 单跑 `write06` 均失败：

```text
write06.c:57: TFAIL: Wrong offset after write 2048 expected 3072
write06.c:63: TFAIL: Wrong file size after append 2048 expected 3072
Summary:
passed   0
failed   2
broken   0
skipped  0
warnings 0
```

这说明第二次写入覆盖了 `[1KiB, 2KiB)` 范围，而不是追加到 EOF。

## 分析

读取 LTP `write06.c` 后确认，测试特意在 `open(O_APPEND)` 后调用 `lseek(fd, K1, SEEK_SET)`，用来验证 `O_APPEND` 是否在每次 `write()` 时重新生效。

内核侧原实现只在 `open()` 处理 `O_APPEND` 时执行一次：

```text
osfile.lseek(0, SEEK_END)
```

这只能把打开时的 offset 初始化到文件末尾。一旦用户之后调用 `lseek()` 改变 offset，`OSFile::write()` 仍按当前 offset 写入，不会再检查 `O_APPEND`，因此本用例在 1KiB 处覆盖 1KiB 数据，文件大小保持 2KiB，写后 offset 为 2KiB。

## 根因

`O_APPEND` 被错误实现为 open 阶段的一次性 seek，而不是文件描述符/打开文件描述上的持续写入语义。`OSFile` 本身没有记录 append 模式，导致 `write/writev/pwrite` 等共用的 `File::write()` 路径无法在写入前定位到 EOF。

## 修复

涉及文件：

- `os/src/fs/files/os_file.rs`
- `os/src/fs/kernel_fs_ops/open.rs`

主要修改：

- `OSFile` 增加 `append` 字段，创建时由 `OpenFlags::O_APPEND` 初始化。
- `OSFile::write()` 在持有 `OSFileInner` offset 锁后，如果 `append == true`，先把 `inner.offset` 设置为 `inode.size()`，再执行写入并推进 offset。
- `open()` 不再在 `O_APPEND` 时执行一次性 `lseek(SEEK_END)`；是否追加由后续每次 `write()` 决定。
- `create_file()` 路径也同步传入 append 标志，覆盖 `O_CREAT | O_APPEND` 的组合。

这样 `lseek()` 仍可改变当前 offset，但下次 `write()` 会按 `O_APPEND` 语义重新定位到 EOF，写后 offset 也会变成新的文件末尾。

## 验证

已执行：

```text
cargo fmt --manifest-path os/Cargo.toml --all
make
timeout 120s make run > log.ans 2>&1
```

结果：

- `make` 通过，当前默认 `TARGET_ARCH=loongarch64`。
- LoongArch 当前配置单跑 musl/glibc `write06`，两者 LTP Summary 均通过：

```text
Summary:
passed   2
failed   0
broken   0
skipped  0
warnings 0
```

- 新 `log.ans` 中 `write06.c` 两个断言均为 `TPASS`：

```text
write06.c:59: TPASS: Offset is correct after write 3072
write06.c:65: TPASS: Correct file size after append 3072
```

- 日志中仍有外层 `FAIL LTP CASE write06 : 10` 与 LoongArch trap 转换噪声，但 LTP 内部 Summary 为 `failed 0`，本问题已修复。
- 未运行 `riscv64` 验证。
