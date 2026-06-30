# writetest: 普通文件 1MiB read 被截断为 64KiB

## 背景

LTP `writetest` 验证普通文件大块写入后能按相同随机数据读回。默认参数下测试写入 1 个 1MiB block：

- 写阶段用 `write(fd, buf, 1048576)` 写入随机数据。
- 验证阶段重新 seed 随机数后，用 `read(fd, buf, 1048576)` 一次读回 1MiB。
- 若 `read()` 返回值不是 1MiB，测试直接返回 `-1`，报告 verify 失败。

## 现象

新的 `log.ans` 中 musl/glibc 单跑 `writetest` 均失败：

```text
writetest    1  TPASS  :  Write: Success
writetest    2  TFAIL  :  writetest.c:253: Verify: Failure
writetest    0  TINFO  :  Total mismatches: -1 bytes
Summary:
passed   1
failed   1
broken   0
skipped  0
warnings 0
```

`Total mismatches: -1 bytes` 来自 `verify_file()` 在 `read()` 返回值不是 `BLOCKSIZE` 时直接返回 `-1`。

## 分析

读取 LTP `writetest.c` 后确认，校验阶段没有循环处理短读：

```text
rv = read(fd, buf_read, BLOCKSIZE);
if (rv != BLOCKSIZE) {
    ret = -1;
    break;
}
```

内核侧 `sys_write()` 已经对大于 `IO_CHUNK_SIZE` 的写入做分片循环，因此 1MiB 写入能成功并返回完整长度。相对地，`sys_read()` 为避免内核堆 OOM，只按 `IO_CHUNK_SIZE=64KiB` 分配内核缓冲区，调用一次 `file.read()` 后直接返回短读结果。普通文件还有后续数据可读，但系统调用已经返回 64KiB，导致 `writetest` 认为读回失败。

不能简单地让所有 fd 都循环读满请求长度，因为 pipe/socket 等非普通文件允许并依赖短读语义；强行读满可能导致阻塞行为变化。

## 根因

`sys_read()` 对普通文件的大块读取只执行了一次 64KiB 分片读，没有像 `sys_write()` 一样继续读取后续分片并写回用户缓冲区。普通文件场景下，这会把用户请求的 1MiB read 截断为 64KiB。

## 修复

涉及文件：

- `os/src/syscall/fs/io.rs`

主要修改：

- `sys_read()` 先从 fd 表获取 `FileDescriptor`，同时判断该 fd 是否为普通 `OSFile`。
- 对普通文件 fd，按 `IO_CHUNK_SIZE` 分片循环：
  - 每次读取最多 64KiB 到内核缓冲区。
  - 读到数据后立即 `copy_to_user()` 到当前用户缓冲区偏移。
  - 直到读满用户请求长度、遇到 EOF、遇到短读或错误。
- 对非普通文件 fd，仍只执行一次读操作后返回，保持 pipe/socket/device 等短读和阻塞语义。
- 如果已经成功读出部分数据，后续读或写回用户缓冲区失败时返回已读字节数，保持与原有分片 `write()` 类似的部分成功语义。

## 验证

已执行：

```text
cargo fmt --manifest-path os/Cargo.toml --all
make
timeout 120s make run > log.ans 2>&1
```

结果：

- `make` 通过，当前默认 `TARGET_ARCH=loongarch64`。
- LoongArch 当前配置单跑 musl/glibc `writetest`，两者 LTP Summary 均通过：

```text
Summary:
passed   2
failed   0
broken   0
skipped  0
warnings 0
```

- 新 `log.ans` 中 `writetest` 写入和校验均为 `TPASS`：

```text
writetest    1  TPASS  :  Write: Success
writetest    2  TPASS  :  Verify: Success
```

- 未运行 `riscv64` 验证。
