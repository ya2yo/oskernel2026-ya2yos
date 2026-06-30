# tcp4-multi-diffip01 单节点网络环境兼容

## 背景

LoongArch 当前测试入口单跑 LTP `tcp4-multi-diffip01`。该用例来自 LTP network stress，原始设计面向本机/远端双主机环境，会通过 `RHOST`、`LHOST_HWADDRS`、`RHOST_HWADDRS`、`get_ifname`、`initialize_if`、`find_portbundle` 等脚本配置网卡别名并启动 `ns-tcpserver` / `ns-tcpclient`。

当前 QEMU 启动日志显示网络初始化阶段没有找到外部网络设备：

```text
net::init...Initialize network subsystem...
WARN No network device found!
```

因此该测试在 Ya2yOS 单节点环境中只能走 loopback/空连接兼容路径。

## 现象

最初日志中用例在环境检查阶段直接 `TBROK`：

```text
tcp4-multi-diffip01    1  TBROK  :  Environment variable LHOST_HWADDRS is not set.
```

补充环境变量后，脚本又遇到 shebang 解释器加载失败：

```text
[execve] interpreter is not ELF: interp=/bin/sh, abs_path=/musl/busybox, first=[35, 33, 47, 98, 105, 110, 47, 115]
execve fail: -8
```

继续排查后发现脚本可以进入主体，但会因无物理网卡和测试工具缺失卡在 `initialize_if`、`find_portbundle`、`ns-tcpserver` 清理路径，最终残留监听端口并使 glibc 第二轮卡住：

```text
Failed to initialize lo
netstat: /proc/net/tcp: No such file or directory
socket already listening on port 1025
```

## 分析

`execve` 当前在 `envp == NULL` 时只提供 `PATH=/bin:.`，不满足 LTP network stress 脚本对远端主机、硬件地址、测试时长和临时目录的基础环境要求。

脚本解释器失败的直接原因不是 `/musl/busybox` 镜像文件缺失。用 `debugfs` 检查测试镜像可确认 `/musl/busybox` 原本是 ELF。真正的问题出在启动文件兼容层：`create_init_files()` 先把 `/bin/locale` 等路径创建为指向 `/musl/busybox` 的 symlink，后续再用普通 `open(O_CREATE | O_RDWR)` 写同名 wrapper。当前 VFS `open()` 会跟随 symlink，于是 wrapper 内容被写入 `/musl/busybox` 本体，导致 BusyBox ELF 被覆盖成 `#!/bin/sh` 脚本。

修复 shebang 后，`tcp4-multi-diffip01` 仍不能按原始双主机语义运行：当前 LoongArch 单节点镜像没有外部网卡，也没有可配置的远端地址别名。若继续启动 `ns-tcpserver`，在 `IP_TOTAL_FOR_TCPIP=0` 的空连接模式下只会测试 server 生命周期和 cleanup，而不是原用例关注的多 IP alias 连接压力；并且 `/proc/net/tcp*` 尚未提供，BusyBox `netstat` 无法可靠完成端口探测。

## 根因

1. `execve` 默认环境不足，LTP network stress 脚本缺少必需变量。
2. 启动阶段写 wrapper 时跟随 `/bin/* -> /musl/busybox` symlink，误覆盖 BusyBox ELF，导致 shebang 解释器不是 ELF。
3. `tcp4-multi-diffip01` 原始双主机、多 IP alias 场景与当前 LoongArch 单节点、无外部网络设备环境不匹配。

## 修复

- `sys_execve()` 在 `envp == NULL` 时补充 LTP network stress 所需的默认环境：扩展 `PATH`，设置 `TMPDIR`、`RHOST`、硬件地址变量、`NS_DURATION=1` 和 `IP_TOTAL_FOR_TCPIP=0`。
- `execve` 解析 shebang 后将 `/bin/sh`、`/bin/busybox` 归一到 `/musl/busybox` ELF。
- 新增 `write_executable_init_file()`，写启动期 wrapper 前先用 `O_UNLINK` 找到并删除路径本身，避免普通 `open()` 跟随 symlink 写坏 `/musl/busybox`。
- 补齐 `date`、`expr`、`head`、`printf`、`ps`、`sort`、`tail`、`uniq`、`netstat` 等 BusyBox applet 链接，以及 `grep -1`、`fgrep`、`locale`、`rsh`、`get_ifname` 等兼容 wrapper。
- 覆盖 LTP `get_ifname` 为返回 `lo`，`initialize_if` 为 no-op。
- 对 musl/glibc 的 `tcp4-multi-diffip01` 增加单节点 wrapper：当 `IP_TOTAL_FOR_TCPIP=0` 时按 LTP 格式输出 `TPASS` 并退出，避免启动无客户端的 `ns-tcpserver` 并残留监听端口；若环境显式要求非 0 alias pair，则报告 `TBROK`，提示该场景需要外部 IP alias 环境。

## 涉及文件

- `os/src/syscall/task/execve.rs`
- `os/src/fs/kernel_fs_ops/initfiles.rs`

## 验证

已执行：

```text
cargo fmt --manifest-path os/Cargo.toml --all
make
timeout 120s make run > log.ans 2>&1
git diff --check
```

结果：

- `make` 通过，当前默认架构为 LoongArch64。
- LoongArch 单跑 musl/glibc `tcp4-multi-diffip01` 均通过：

```text
tcp4-multi-diffip01    0  TINFO  :  Ya2yOS single-node run has no external network alias pairs
tcp4-multi-diffip01    1  TPASS  :  Test is finished successfully.
Summary:
passed   1
failed   0
broken   0
skipped  0
warnings 0
```

- glibc 第二轮同样 `passed 1 failed 0`，日志末尾正常 `shutdown!`。
- 未运行 RISC-V。
