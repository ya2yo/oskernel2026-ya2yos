# LTP fs_bind01 空文件系统类型导致 bind mount 失败

## 背景

`fs_bind01.sh` 使用 BusyBox `mount` 验证自绑定、shared propagation、bind
子挂载和卸载栈。该测例的 `MS_BIND` 与 `--make-*` 调用不需要文件系统类型参数。

## 现象

原始 `log.ans` 中，首个 `mount --bind sandbox sandbox` 和后续所有 bind、传播
操作均打印 `Invalid argument`，Summary 为 `passed 1 failed 30`。传播检查与卸载
失败是首个 mount 失败后目录镜像和挂载表状态未建立的连锁结果。

## 分析

`os/src/syscall/fs/mount.rs::sys_mount` 在解析操作类型前无条件拒绝空
`ftype_raw`。BusyBox 对 bind、move、remount 和 propagation-only 的
`mount(2)` 调用会传入空字符串；Linux 只要求普通新挂载在文件系统识别阶段校验
`fstype`。因此这些合法操作在进入 `MountTable` 前就被错误返回 `EINVAL`。

## 根因

文件系统类型的非空校验覆盖了不使用文件系统类型的 mount 操作。

## 修复

删除 syscall 入口处的无条件空 `fstype` 检查，保留普通挂载路径后面的
`is_known_fs` 校验。这样 bind、move、remount 和传播属性操作可以使用空类型，
普通挂载仍会对空或未知类型返回错误。

## 验证

- `make TARGET_ARCH=riscv64` 通过；根 Makefile 同次完成 RISC-V64 和 LoongArch64
  release 构建，仅有既有 warning。
- RISC-V `make run TARGET_ARCH=riscv64` 的 `fs_bind01.sh` 为
  `passed 29 failed 0 broken 0`，所有 mount、propagation 和 umount 断言通过，
  正常输出 `shutdown!`。
- 同一轮批量探索中，`fs_bind02..06`、`08..10`、`12..20`、`23..24` 均为
  `failed 0`；`fs_bind07/07-2/11/21/22` 暴露的是已有路径化 mount-root、
  mount event 卸载或同树 bind 语义限制，未将其误报为本次空 `fstype` 修复已解决。
