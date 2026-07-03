# LTP access02: CLONE_VM panic 与 O_RDONLY 权限误判

## 背景

LTP `access02` 测试 `access()` / `faccessat()` 在 `F_OK/R_OK/W_OK/X_OK` 下的行为，并对符号链接做同样检查。`X_OK` 分支在 `access()` 通过后还会 `system("./file_x")` 验证文件确实可执行。

setup 会创建：

- `file_r`：只读文件，用于验证 `R_OK` 与只读 `open()`。
- `file_x`：mode `0555`，内容为 `#!/bin/sh\n`，用于验证 shebang 脚本执行。
- `symlink_*`：指向对应 `file_*` 的符号链接。

## 现象

第一阶段，`access02` 在 `clone()` 路径触发内核 panic。GDB backtrace 指向：

```text
os::mm::memory_set::MemorySet::get_mut
os::task::task::task::TaskControlBlock::alloc_user_res
os::task::task::task::TaskControlBlock::clone_process
os::syscall::task::clone::sys_clone
```

触发参数中包含 `CLONE_VM`，属于共享地址空间的 clone/vfork 类路径。

修复 panic 后，测例继续运行但仍有 4 个 `TFAIL`：

```text
access02.c:91: TFAIL: open file_r with O_RDONLY as nobody failed: EACCES (13)
/bin/sh: can't open '/tmp/LTP_.../file_x': Permission denied
access02.c:129: TFAIL: execute file_x as nobody failed: SUCCESS (0)
access02.c:91: TFAIL: open file_r with O_RDONLY as nobody failed: EACCES (13)
/bin/sh: can't open '/tmp/LTP_.../file_x': Permission denied
access02.c:129: TFAIL: execute file_x as nobody failed: SUCCESS (0)
```

`access(file_r, R_OK)`、`access(file_x, X_OK)` 以及对应 symlink 的 `access()` 断言已经通过，失败集中在后续真正 `open(O_RDONLY)` 和 `/bin/sh` 读取脚本文件。

## 分析

### CLONE_VM panic

`clone_process()` 原先只把 `CLONE_THREAD` 当作共享地址空间路径处理。对于 `CLONE_VM | CLONE_VFORK | SIGCHLD` 这类非线程 clone，代码仍进入 fork 分支：

1. 为 child 调用 `alloc_user_res()`，尝试重新分配用户栈和 trap context。
2. 同时父子进程实际共享 `MemorySet`。
3. 在父地址空间读锁尚未释放时再次进入可变分配路径，最终在 `MemorySet::get_mut().expect()` 处 panic。

共享 `CLONE_VM` 的子任务不应复制父地址空间，也不应分配一套新的 fork 用户栈。用户传入了 child stack 时，只需要为 child 分配独立 trap context 并复制父 trap 上下文。

### O_RDONLY 被误判为读写

4 个 `TFAIL` 的共同点是用户态以只读方式打开文件失败：

- `open(file_r, O_RDONLY)` 作为 nobody 返回 `EACCES`。
- `system("./file_x")` 已成功进入 shebang 路径并执行 `/bin/sh`，但 busybox shell 需要重新只读打开脚本路径，结果被内核拒绝，打印 `Permission denied`。

`sys_openat()` 最终通过 `OpenFlags::read_write()` 计算文件描述符可读/可写属性。旧实现为：

```rust
if self.is_empty() {
    (true, false)
} else if self.contains(Self::O_WRONLY) {
    (false, true)
} else {
    (true, true)
}
```

这段逻辑只在 flags 完全为 0 时识别 `O_RDONLY`。一旦用户态传入 `O_RDONLY | O_CLOEXEC`、`O_RDONLY | O_LARGEFILE` 等常见组合，flags 非空且不含 `O_WRONLY`，就会被错误判断成 `(readable=true, writable=true)`。

随后 VFS 对已有文件执行写权限检查。`file_r` 和 `file_x` 对 nobody 没有写权限，所以只读 open 被错误拒绝为 `EACCES`。

## 根因

1. `clone_process()` 用 `CLONE_THREAD` 判断共享地址空间路径，遗漏了非线程 `CLONE_VM`，导致 vfork/clone 类路径错误分配用户资源并触发 `MemorySet::get_mut()` panic。
2. `OpenFlags::read_write()` 没有按 Linux `O_ACCMODE` 低两位解析访问模式，而是把“非空且非 `O_WRONLY`”全部当作读写打开，导致只读 open 携带其他 flag 时误触发写权限检查。

## 修复

- `os/src/task/task/task.rs`
  - `clone_process()` 改为以 `CLONE_VM` 判断共享地址空间路径。
  - `CLONE_VM` 且用户提供 child stack 时，只分配 trap context，不再调用 `alloc_user_res()` 复制 fork 栈。
  - 非 `CLONE_VM` 的 fork 分支继续分配独立用户资源并执行 lazy clone。

- `os/src/fs/mod.rs`
  - `OpenFlags::read_write()` 改为使用 `flags & O_ACCMODE` 解码访问模式：
    - `O_RDONLY` -> `(true, false)`
    - `O_WRONLY` -> `(false, true)`
    - `O_RDWR` -> `(true, true)`
  - `O_PATH` 特判为 `(false, false)`，避免路径 fd 被误当作可读写文件。

## 涉及文件

| 文件 | 改动 |
|------|------|
| `os/src/task/task/task.rs` | 修复 `CLONE_VM` clone/vfork 分支的用户资源分配和 trap context 初始化 |
| `os/src/fs/mod.rs` | 修复 `OpenFlags::read_write()` 对 `O_RDONLY | 其他 flag` 的访问模式解析 |

## 验证

已执行：

```text
make
```

结果：默认 RISC-V 构建通过。

用户随后提供的最新 `log.ans` 显示 `access02` 全部核心断言通过：

```text
access02.c:139: TPASS: access(file_f, F_OK) as root behaviour is correct.
access02.c:139: TPASS: access(file_f, F_OK) as nobody behaviour is correct.
access02.c:139: TPASS: access(file_r, R_OK) as root behaviour is correct.
access02.c:139: TPASS: access(file_r, R_OK) as nobody behaviour is correct.
access02.c:139: TPASS: access(file_w, W_OK) as root behaviour is correct.
access02.c:139: TPASS: access(file_w, W_OK) as nobody behaviour is correct.
access02.c:139: TPASS: access(file_x, X_OK) as root behaviour is correct.
access02.c:139: TPASS: access(file_x, X_OK) as nobody behaviour is correct.
access02.c:139: TPASS: access(symlink_f, F_OK) as root behaviour is correct.
access02.c:139: TPASS: access(symlink_f, F_OK) as nobody behaviour is correct.
access02.c:139: TPASS: access(symlink_r, R_OK) as root behaviour is correct.
access02.c:139: TPASS: access(symlink_r, R_OK) as nobody behaviour is correct.
access02.c:139: TPASS: access(symlink_w, W_OK) as root behaviour is correct.
access02.c:139: TPASS: access(symlink_w, W_OK) as nobody behaviour is correct.
access02.c:139: TPASS: access(symlink_x, X_OK) as root behaviour is correct.
access02.c:139: TPASS: access(symlink_x, X_OK) as nobody behaviour is correct.
```

日志未再出现 `TFAIL`、`TBROK`、panic 或死循环；最后正常 `shutdown!`。

`FAIL LTP CASE access02 : 10` 仍是外层 wrapper 打印的退出码行，本仓库判读 LTP 结果时以 `TPASS/TFAIL/TBROK/Summary` 为准。

## 历史相关修复

同一测例早期还修过 shebang 支持：`sys_execve()` 对非 ELF 的 `#!` 脚本解析解释器路径，将 `file_x` 这类无 `.sh` 后缀脚本转为执行 `/bin/sh`/`/musl/busybox`。本次 `/bin/sh: can't open ... Permission denied` 已说明 shebang 路径进入成功，剩余问题在脚本只读 open 被误判成读写 open。
