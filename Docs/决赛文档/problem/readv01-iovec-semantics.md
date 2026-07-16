# LTP readv01 空 iovec 与参数校验修复

## 背景

LTP `readv01` 验证 `readv(2)` 对空 iovec、空分量、多分量和普通文件读取的基础语义。测试会分别由 musl 与 glibc 运行。

## 现象

原始 `log.ans` 中 musl/glibc 都只有首项失败：

```text
readv01.c:61: TFAIL: readv() failed unexpectedly: EINVAL (22)
```

其余四组场景已经通过。失败场景传入合法只读文件 fd、`iovcnt == 0`，预期返回 0。

## 分析

`readv01.c` 的第一组用例调用 `readv(fd, rd_iovec, 0)`。Linux 将零长度 iovec 数组视为成功的无操作，但 fd 仍应先按正常规则验证。

修复前 `sys_readv()` 在查找 fd 前执行 `if iovcnt == 0 || iovcnt > IOV_MAX { return EINVAL; }`，把合法的空数组与超过上限的数组混为同一种错误。该路径与同文件已修复的 `writev(2)` 行为不一致。

同时，原始 readv 逐项读取 metadata，未复用现有的 `read_iovecs()`：它遗漏单项长度、总长度 `ssize_t` 上限与整数累加溢出校验。用户输出缓冲区也在文件读取后才会失败，可能让错误地址消耗文件 offset。

## 根因

`sys_readv()` 没有遵循 vectored I/O 的空数组 no-op 语义，并且参数校验路径落后于同模块的 `writev`、`preadv2` 实现。

## 修复

在 `os/src/syscall/io_mpx/file.rs` 中：

- 先验证 fd 和可读性；合法 fd 的 `iovcnt == 0` 返回 0，超过 `IOV_MAX` 继续返回 `EINVAL`。
- fd 越界和不可读 fd 返回 `EBADF`，目录显式返回 `EISDIR`。
- 使用共享 `read_iovecs()` 统一校验每个 `iov_len`、累计长度和溢出。
- 对每个非空 iovec 以 64 KiB 分片，先 `probe_user_write()`，再执行文件读取和 `copy_to_user()`；短读、EOF 和已完成部分读取均按既有 `read(2)`/`preadv2(2)` 语义返回已读取字节数。

## 涉及文件

- `os/src/syscall/io_mpx/file.rs`

## 验证

执行：

```text
cargo fmt --manifest-path os/Cargo.toml -- --check
make
timeout 120s make TARGET_ARCH=loongarch64 run > /tmp/readv01-after-fix-loongarch64.log 2>&1
```

- 格式检查通过。
- 根目录 `make` 完成 RISC-V 与 LoongArch64 构建；仅有既有 vendored `smoltcp` warning。
- LoongArch64 QEMU 中 musl 与 glibc `readv01` 均为 `passed 10 failed 0 broken 0 skipped 0 warnings 0`，返回状态为 0 并正常 `shutdown!`。
- 未运行 RISC-V QEMU 和 `readv02` 单测；两架构均已完成编译验证。
