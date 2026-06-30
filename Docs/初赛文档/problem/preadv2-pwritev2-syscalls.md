# preadv2/pwritev2 系统调用实现

## 背景

LTP 与 libc 包装层会使用 `preadv2(2)` / `pwritev2(2)` 测试带 iovec 的定位读写语义。项目中 `os/src/syscall/fs/io.rs` 已有样板代码，但原实现没有完整处理 raw syscall ABI、offset、flags、iovec 校验和错误码。

同时，部分 libc 在 flags 为 0 时可能走旧的 `preadv(2)` / `pwritev(2)` 号，因此需要让旧号复用同一套后端实现。

## 现象

未实现完整语义时，相关测例会出现以下风险：

- `preadv2/pwritev2` 的第 4、5 个参数没有按 raw ABI 合并为 64 位 offset，第 6 个参数 flags 也没有独立处理。
- `offset == -1` 与显式 offset 的文件偏移行为不同，样板代码没有区分。
- `pwritev2` 样板代码误走 `readv` 路径，且权限检查依赖 `File::writable()`，不能覆盖 fd access mode。
- pipe/FIFO、目录、只读/只写 fd、坏用户地址等错误码不能稳定匹配 Linux/LTP 期望。

## 分析

LTP 的 `lapi/preadv2.h` / `lapi/pwritev2.h` 通过：

```c
tst_syscall(__NR_preadv2, fd, iov, iovcnt, LO_HI_LONG(offset), flags)
```

传参。内核分发必须将 `args[3]`、`args[4]` 合并成 signed 64-bit offset，并使用 `args[5]` 作为 flags。

Linux 语义中：

- `offset == -1` 表示使用并更新当前 fd offset。
- `offset >= 0` 表示从指定 offset 读写，调用结束后原 fd offset 不变。
- 负 offset 除 `-1` 外返回 `EINVAL`。
- 当前未支持 `RWF_*` 行为时，非零 flags 返回 `EOPNOTSUPP`。
- `iovcnt == 0` 直接返回 0；`iovcnt > IOV_MAX` 或 iovec 总长度溢出返回 `EINVAL`。
- `preadv2` 读入用户缓冲区前需要能发现不可写用户地址并返回 `EFAULT`，但不能为了探测而修改用户缓冲区内容。

## 根因

已有样板只占位了 syscall 名称，没有按 Linux raw ABI 和 iovec 定位读写语义实现。`pwritev2` 还存在方向错误，实际调用了读路径。用户缓冲区探测也缺少无副作用 helper，若直接用 `copy_to_user(..., &[0])` 探测会污染用户首字节。

## 修复

- 在 `Syscall` 枚举与分发中接入 `preadv/pwritev/preadv2/pwritev2`：
  - `preadv2/pwritev2` 使用 6 参数 raw ABI。
  - `preadv/pwritev` 以 flags=0 复用 `preadv2/pwritev2` 后端。
- 在 `io.rs` 中实现通用 iovec 读取与校验：
  - 限制 `IOV_MAX=1024`。
  - 检查单个 iov_len 与总长度不超过 `isize::MAX`。
  - 支持 `iovcnt == 0` 直接返回 0。
- 实现 offset 语义：
  - `offset == -1` 不恢复 fd offset。
  - 显式 offset 先 seek 到目标位置，完成或错误返回前恢复原 offset。
- 实现读写路径：
  - 每个 iovec 按 64KiB 分片搬运，避免大 iovec 造成内核堆压力，同时不截断用户请求。
  - `pwritev2` 从用户态拷贝到内核缓冲区后写文件。
  - `preadv2` 先探测用户写缓冲区，再读文件并写回用户态。
  - 文件读写时不持有进程锁。
- 在 `mm::translate` 增加 `probe_user_write()`，只触发页表/COW/权限检查，不修改用户缓冲区。
- 同步修正 `pwrite64` 的 fd access mode 检查，避免 `O_RDONLY | O_CREAT` 只依赖 `File::writable()`。

## 涉及文件

- `os/src/syscall/mod.rs`
- `os/src/syscall/fs/io.rs`
- `os/src/mm/translate.rs`

## 验证

已执行：

```text
rustfmt os/src/syscall/fs/io.rs os/src/syscall/mod.rs os/src/mm/translate.rs
make
timeout 120s make run > /tmp/preadv2-pwritev2.log 2>&1
make
make TARGET_ARCH=riscv64
```

验证结果：

- `make` 在当前默认 `loongarch64` 下通过。
- 临时切换 `initproc.rs` 单跑 musl/glibc 的 `preadv201`、`preadv202`、`pwritev201`、`pwritev202` 后，LTP 内部 Summary 均为 `failed 0`：
  - musl `preadv201`: passed 6 failed 0
  - musl `preadv202`: passed 8 failed 0
  - musl `pwritev201`: passed 6 failed 0
  - musl `pwritev202`: passed 7 failed 0
  - glibc `preadv201`: passed 6 failed 0
  - glibc `preadv202`: passed 8 failed 0
  - glibc `pwritev201`: passed 6 failed 0
  - glibc `pwritev202`: passed 7 failed 0
- 测试后已恢复 `initproc.rs` 到本轮开始时的 `pread02` 单测配置，并再次运行 `make` 通过。
- `make TARGET_ARCH=riscv64` 编译通过。

日志中仍可见既有噪声 `Fail to convert LoongArch Unknown to Trap type! 0x0` 与 ext4 `write_back_cache ... rc = 2`，但未导致上述 LTP 内部失败。

## 限制

当前未实现 `RWF_NOWAIT`、`RWF_HIPRI`、`RWF_DSYNC` 等具体行为。所有非零 flags 统一返回 `EOPNOTSUPP`，满足当前 LTP 错误码用例。
