# fchmod02 /etc/group 缺少 users/daemon 导致 TBROK

## 背景

`fchmod02` 验证 root 进程在非文件 owner、但有效组或 supplementary groups 匹配文件 group 时，`fchmod(2)` 可以成功设置权限位和 sticky bit。

该用例在真正调用 `fchmod()` 前会先查找测试用户和测试组：

- `SAFE_GETPWNAM("nobody")`
- `SAFE_GETGRNAM_FALLBACK("users", "daemon")`

因此 `/etc/passwd` 和 `/etc/group` 的最小内容也是用例前置环境的一部分。

## 现象

原始 `log.ans` 中 musl 和 glibc 两轮都停在组查找阶段：

```text
fchmod02.c:54: TINFO: getgrnam(users) failed - try fallback daemon
fchmod02.c:54: TBROK: getgrnam(daemon) failed: SUCCESS (0)

Summary:
passed   0
failed   0
broken   1
```

这说明测试并未进入 `fchmod()` 语义断言，而是在 LTP setup 阶段因缺少组数据库条目被标记为 broken。

## 分析

Ya2yOS 启动期会在 `fs::init()` 中通过 `create_init_files()` 补齐 `/etc/passwd`、`/etc/group` 等测试兼容文件。修复前的 `/etc/group` 模板只有：

```text
root:x:0:
nobody:x:1:
```

而 LTP `fchmod02` 不查找 `nobody` 组，而是先找 `users`，失败后找 `daemon`。两者都不存在时，`SAFE_GETGRNAM_FALLBACK()` 直接触发 `TBROK`。

## 根因

启动期生成的 `/etc/group` 过于精简，缺少 LTP 常用的 `users` 和 `daemon` 组，导致 libc 的 `getgrnam()` 无法满足 `fchmod02` 的 setup 前置条件。

## 修复

在 `os/src/fs/kernel_fs_ops/initfiles.rs` 中扩展启动期 `/etc/group` 模板：

```text
root:x:0:
daemon:x:2:
users:x:100:
nobody:x:1:
```

这样 `SAFE_GETGRNAM_FALLBACK("users", "daemon")` 可以在第一项 `users` 命中；即使未来测试查找 fallback `daemon`，也有对应条目。

该修复只调整测试环境兼容文件，不修改 `fchmod(2)`、`chown(2)`、凭证或 VFS 权限语义。

## 涉及文件

- `os/src/fs/kernel_fs_ops/initfiles.rs`

## 验证

已执行：

```text
make
```

结果：默认 LoongArch64 构建通过。

最新 `log.ans` 显示 musl 和 glibc 两轮 `fchmod02` 均已通过核心断言：

```text
fchmod02.c:43: TPASS: Functionality of fchmod(3, 01777) Successful

Summary:
passed   1
failed   0
broken   0
```

日志中仍有 wrapper 打印：

```text
FAIL LTP CASE fchmod02 : 10
RESULT GLIBC LTP SINGLE CASE fchmod02 : 10
```

该行不是 LTP 断言结果；本次判断以 `TPASS` 和 Summary 为准。
