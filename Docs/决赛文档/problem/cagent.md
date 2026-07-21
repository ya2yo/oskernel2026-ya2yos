# CAgent Bash 运行器与 Debian `/bin` 符号链接

## 背景

final-2026 RISC-V 镜像中的 `glibc/cagent_testcode.sh` 使用 Bash 数组
`TEST_PIDS=()` 并通过 `wait "${TEST_PIDS[@]}"` 汇总并行任务。该脚本首行声明
`#!/bin/bash`，镜像中的 `/bin/bash` 是 Debian `/bin -> /usr/bin` 符号链接下的动态
链接 ELF。

## 现象

初始运行日志在脚本第 7 行停止：

```text
cagent_testcode.sh: line 7: syntax error: unexpected "("
```

将决赛脚本改为以 `/bin/bash` 启动后，新的日志变为：

```text
["/bin/bash\\0", "cagent_testcode.sh\\0"]
execve fail: -20
```

其中 `-20` 是 `ENOTDIR`。因此问题并非 CAgent 可执行文件、HTTP 服务或测试任务本身。

## 根因

`user/src/bin/initproc.rs` 的通用 `run_testsuit()` 固定执行
`busybox sh <script>`，会显式绕过脚本 shebang。BusyBox `sh` 不支持 Bash 数组，因此
CAgent 尚未运行任何测试任务就报语法错误。

改用 `/bin/bash` 后暴露出 VFS 的第二个问题。final Debian 镜像的 `/bin` 是指向
`usr/bin` 的相对符号链接，但 `Ext4Inode::find()` 只能跟随末级符号链接。查找
`/bin/bash` 时，中间组件 `/bin` 被当作非目录并返回 `ENOTDIR`。

`open_inner()` 原本在查找失败时具有“按已解析父目录重试”的兼容路径，却对
`ENOTDIR` 立即返回。并且启动期通过 `O_UNLINK` 检查 `/bin` 时会把未跟随的 symlink
缓存到 `FsIndex`，使父目录解析再次命中这个非目录缓存项。

## 修复

- `user/src/bin/initproc.rs`
  - 新增 `run_final_testsuit()`，以 `/bin/bash` 启动决赛脚本；普通测试仍保持
    `busybox sh`，避免改变已有预赛脚本行为。
  - `fork_and_run()` 在 `execve` 失败时打印返回 errno，便于区分启动失败与脚本内失败。
- `os/src/fs/kernel_fs_ops/open.rs`
  - 父目录缓存项不是目录时，使用 `O_DIRECTORY` 从根目录重新解析，使中间 symlink
    得到真实目录 inode。
  - `open_inner()` 收到 `ENOTDIR` 后先尝试以已解析父目录构造的路径重试；实际父目录
    仍不是目录时保留 `ENOTDIR`。

## 验证

执行：

```text
rustfmt --edition 2021 --check os/src/fs/kernel_fs_ops/open.rs user/src/bin/initproc.rs
git diff --check
make build-arch TARGET_ARCH=riscv64
timeout 100s make run
```

RISC-V `log.ans` 结果：

```text
#### OS COMP TEST GROUP START cagent ####
testcase cagent factorial pass 7086
testcase cagent network pass 9676
testcase cagent date pass 9675
testcase cagent fs-directory reject 7988
testcase cagent fs-usage reject 6404
testcase cagent cpu pass 10542
testcase cagent kernel pass 11761
testcase cagent fs-create pass 9526
testcase cagent fs-readwrite reject 12938
testcase cagent fs-search pass 11491
#### OS COMP TEST GROUP END cagent ####
shutdown!
```

本次确认 CAgent 的 Bash 运行链路完整结束：7 项 `pass`，3 项 `reject`。后者是
`agent_lite` 对文件系统任务的语义判定失败，不是脚本启动、Bash 解析或 `execve` 失败，
需作为独立功能问题继续排查。

LoongArch64 未运行本测例；本次变更的 VFS 路径逻辑为架构无关代码。
