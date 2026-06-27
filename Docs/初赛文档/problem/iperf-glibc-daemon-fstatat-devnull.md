# iperf-glibc daemon 启动失败

## 背景

RISC-V 当前测试入口单跑 glibc `iperf_testcode.sh`。脚本先执行：

```text
./iperf3 -s -p 5001 -D
```

随后依次执行 UDP/TCP 基础、并发和反向传输用例。`-D` 依赖 glibc `daemon()` 完成 fork、setsid、标准 fd 重定向和工作目录切换。

## 现象

`log.ans` 中服务端启动阶段直接失败：

```text
iperf3: error - unable to become a daemon: Invalid argument
```

服务端没有监听 5001 端口，后续六个客户端子项全部报 `Connection refused` 并判定 fail。

## 分析

从镜像中导出 `/glibc/iperf3` 后反汇编 `daemon()`，确认 glibc 静态实现的关键路径为：

1. `fork()`
2. 子进程 `setsid()`
3. `open("/dev/null", O_RDWR)`
4. `fstat(fd)`，要求 `/dev/null` 是字符设备且 `st_rdev == makedev(1, 3)`
5. `dup2(fd, 0/1/2)`
6. `chdir("/")`

glibc RISC-V 的 `fstat(fd)` 并不调用内核 `fstat(80)`，而是调用 `fstatat(fd, "", buf, AT_EMPTY_PATH)`。原 `sys_fstatat()` 忽略 `AT_EMPTY_PATH` 和空路径语义：空路径被解析后继续 `open()`，当 `dirfd` 是 `/dev/null` 这种 devfs 抽象文件时，`get_abs_path()` 尝试把 fd 当普通 `OSFile` 取 inode，最终返回 `EINVAL`。这正好对应用户态打印的 `Invalid argument`。

继续检查 `daemon()` 的下一步可知，即使 `fstatat` 成功，glibc 还会检查 `/dev/null` 的 `st_rdev`。原 devfs 为每个设备分配内部递增编号，`/dev/null` 的 `st_rdev` 不是 Linux 兼容的 `makedev(1, 3)=259`，会导致 daemon 把设备判为非法。

## 根因

- `sys_fstatat()` 缺少 `AT_EMPTY_PATH` 支持，不能处理 glibc `fstat(fd)` wrapper 使用的 `fstatat(fd, "", ...)`。
- `DevNull::fstat()` 返回内部动态设备号作为 `st_rdev`，不符合 glibc `daemon()` 对 `/dev/null` 的 Linux 设备号检查。

## 修复

- `os/src/syscall/fs/stat.rs`
  - `sys_fstatat()` 接收并检查 `flags`。
  - 当路径为空且设置 `AT_EMPTY_PATH` 时，按 `dirfd` 查询 fd 本身的 `fstat()`；`dirfd == AT_FDCWD` 时返回当前工作目录 stat；未设置 `AT_EMPTY_PATH` 的空路径返回 `ENOENT`。
  - 补齐 `kst` 用户指针基础校验。

- `os/src/fs/files/devfs.rs`
  - `/dev/null` 的 `st_rdev` 固定返回 `(1 << 8) | 3`，即 Linux `makedev(1, 3)=259`。
  - 保留 `st_dev` 使用内部设备表编号，不影响读写行为。

## 验证

已执行：

```text
make
timeout 120s make run > log.ans 2>&1
```

结果：

- `make` 通过。
- `iperf3 -s -D` 不再输出 `unable to become a daemon`。
- `iperf-glibc` 六个子项全部 success：

```text
====== iperf BASIC_UDP end: success ======
====== iperf BASIC_TCP end: success ======
====== iperf PARALLEL_UDP end: success ======
====== iperf PARALLEL_TCP end: success ======
====== iperf REVERSE_UDP end: success ======
====== iperf REVERSE_TCP end: success ======
#### OS COMP TEST GROUP END iperf-glibc ####
shutdown!
```

日志中 TCP 项仍会打印 `iperf3: getsockopt - Protocol not available`，但本组测试判定通过；该 TCP option 可后续单独补齐。
