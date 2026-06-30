# writev06: MAP_ANONYMOUS 忽略 fd 语义错误导致 SIGSEGV

## 背景

LTP `writev06` 验证 `writev(2)` 对页边界可读缓冲区的处理。测试创建多段匿名映射：

- `PROT_NONE` 区域作为 guard page。
- `PROT_READ | PROT_WRITE` 区域作为可读写页。
- iovec 指向可读写页最后 1 字节，周围紧邻不可读页。

内核应只读取 iovec 指定的 2 个字节，`writev()` 返回 2。

## 现象

新的 `log.ans` 中 musl/glibc 单跑 `writev06` 均在 setup 或写入前收到 `SIGSEGV`：

```text
[kernel] hart 0 Exception(StorePageFault) in application, bad addr = ...
writev06    1  TBROK  :  tst_sig.c:232: unexpected signal SIGSEGV(11) received
writev06    2  TBROK  :  tst_sig.c:232: Remaining cases broken
writev06    3  TFAIL  :  writev06.c:204: unlink Failed--file = writev_data_file.2, errno = 2
Summary:
passed   0
failed   1
broken   2
skipped  0
warnings 0
```

cleanup 阶段的 unlink 失败只是测试被 SIGSEGV 打断后的附带结果，真正根因是用户态 StorePageFault。

## 分析

读取 LTP `writev06.c` 后确认，测试使用：

```text
mmap(NULL, page_size * 3, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, 0, 0)
mmap(NULL, page_size, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, 0, 0)
```

注意这些匿名映射的 `fd` 参数为 `0`。Linux 对 `MAP_ANONYMOUS` 的语义是忽略 `fd`，实际不应要求 `fd == -1`。

内核原 `sys_mmap()` 分支为：

- `fd == usize::MAX` 且 `MAP_ANONYMOUS`：按请求正常匿名映射。
- `fd != usize::MAX` 且 `MAP_ANONYMOUS`：无视用户传入的 `len/prot/addr`，只映射 1 字节 `PROT_NONE`，并登记为 bad address。

因此 `writev06` 中本应可写的 `good_addr` 实际也被映射成不可写地址。测试在 `memset(good_addr, ...)` 强制触发页分配时发生 StorePageFault，进程收到 `SIGSEGV`。

## 根因

`sys_mmap()` 错误地把 `MAP_ANONYMOUS` 是否正常映射绑定到 `fd == -1`。但 Linux 对匿名映射会忽略 fd 参数，LTP 多个用例会传 `fd=0`。当前兼容分支还把 `fd=0` 的匿名映射降级成 1 字节 `PROT_NONE`，直接破坏了调用方请求的权限和长度。

## 修复

涉及文件：

- `os/src/syscall/mm/mmap.rs`

主要修改：

- `sys_mmap()` 在解析 flags 后，优先判断 `MAP_ANONYMOUS`。
- 只要包含 `MAP_ANONYMOUS`，就忽略 fd，按用户请求的 `addr/len/prot/flags` 调用 `memory_set.mmap(..., None, ...)`。
- 删除 `fd != -1 && MAP_ANONYMOUS` 时映射 1 字节 `PROT_NONE` 并 `insert_bad_address()` 的特殊分支。
- 非匿名映射仍要求 fd 有效；`fd == -1` 且非匿名时返回 `EBADF`。

这样 `PROT_NONE` guard page 仍会按请求映射为不可访问，`PROT_READ | PROT_WRITE` 匿名页也能正常被用户态 `memset()` 写入。

## 验证

已执行：

```text
cargo fmt --manifest-path os/Cargo.toml --all
make
timeout 120s make run > log.ans 2>&1
```

结果：

- `make` 通过，当前默认 `TARGET_ARCH=loongarch64`。
- LoongArch 当前配置单跑 musl/glibc `writev06`，两者 LTP Summary 均通过：

```text
Summary:
passed   1
failed   0
broken   0
skipped  0
warnings 0
```

- 新 `log.ans` 中未再出现 `SIGSEGV`，断言为：

```text
writev06    1  TPASS  :  writev returned 2 as expected
```

- 未运行 `riscv64` 验证。
