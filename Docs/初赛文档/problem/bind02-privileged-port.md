# bind02: getgrgid 失败与特权端口权限

## 背景

LTP `bind02` 验证非 root 用户绑定特权端口时，`bind()` 是否按 Linux 语义返回 `EACCES`。测试入口会先查找 `nobody` 用户，将有效 gid/uid 切到该用户，再尝试绑定 TCP 463 端口。

## 现象

`log.ans` 中 musl 和 glibc 的 `bind02` 都没有进入真正的 `bind()` 断言，而是在 setup 阶段直接 `TBROK`：

```text
bind02.c:50: TBROK: getgrgid(0) failed: ENOENT (2)
Summary:
passed   0
failed   0
broken   1
```

旧日志中 glibc 单测前还出现过一次 LoongArch unknown trap 转换错误，但测试最终失败信号仍是 `getgrgid(0)` 返回 `ENOENT`。

## 分析

LTP 源码中 `setup()` 逻辑如下：

1. `SAFE_GETPWNAM("nobody")` 读取 `nobody` 用户。
2. `SAFE_GETGRGID(pw->pw_gid)` 根据用户所属 gid 读取 group。
3. `SAFE_SETEGID()` / `SAFE_SETEUID()` 切换凭据。
4. 用非 root 身份绑定 463 端口，期望 `EACCES`。

Ya2yOS 的 `initfiles` 已创建 `/etc/passwd`：

```text
root:x:0:0:root:/root:/bin/bash
nobody:x:1:0:nobody:/nobody:/bin/bash
```

因此 `nobody` 的 gid 是 0。但内核初始化没有创建 `/etc/group`，libc 的 `getgrgid(0)` 无法解析 gid 0 对应的组名，导致测试在前置条件处 `TBROK`。

补齐 `/etc/group` 后，测试会继续进入 `bind()` 断言。继续检查 TCP/UDP `bind` 路径发现，内核只检查端口占用和 socket 状态，没有实现 Linux 的特权端口约束：非特权进程不能绑定小于 1024 的端口。这样会导致 `bind02` 从 `TBROK` 变成 `TFAIL`。

## 根因

1. 根文件系统初始化缺少 `/etc/group`，而 `/etc/passwd` 中的 `nobody` 指向 gid 0，导致 `getgrgid(0)` 失败。
2. 网络栈 `bind()` 对 `port < 1024` 缺少权限检查，非 root 用户也能绑定特权端口。

## 修复

涉及文件：

- `os/src/fs/kernel_fs_ops/initfiles.rs`
- `os/src/net/mod.rs`
- `os/src/net/tcp.rs`
- `os/src/net/udp.rs`

修改内容：

- 增加最小 `/etc/group` 内容：

```text
root:x:0:
nobody:x:1:
```

- 在 `create_init_files()` 中写入 `/etc/group`，让 `getgrgid(0)` 能解析为 `root`。
- 在 `net` 模块新增 `check_privileged_port_bind(port)`，对 `port < 1024` 检查当前任务 `effective_uid`。
- TCP/UDP `bind()` 在自动分配端口后、端口占用检查前调用该辅助函数；非 root 绑定特权端口返回 `EACCES`。

本次按现有 capability 简化模型实现：`effective_uid == 0` 视为具备 `CAP_NET_BIND_SERVICE`。

## 验证

已执行：

```text
make
make log
timeout 120s make run > log.ans 2>&1
```

结果：

- `make` 通过，当前默认 `TARGET_ARCH=loongarch64`。
- `make log` 通过。
- 当前入口单跑 musl/glibc `bind02`，两者都输出 `TPASS`：

```text
bind02.c:52: TINFO: Switching credentials to user: nobody, group: root
bind02.c:39: TPASS: bind() : EACCES (13)
Summary:
passed   1
failed   0
broken   0
```

- glibc `bind02` 同样输出 `TPASS: bind() : EACCES (13)`，未再出现原来的 `getgrgid(0) failed`。
- glibc 路径仍打印一次 `Fail to convert LoongArch Unknown to Trap type! 0x0`，但测例继续执行并通过；该日志不是本次 `bind02` 的失败根因，后续可单独排查 LoongArch trap 分类。
