# access01: wait4 ERESTART 暴露与非空目录 rmdir 语义

## 背景

LTP `access01` 会在临时目录中反复创建不同权限的测试文件，并通过父子进程分别以 root/nobody 身份检查 `access()` 语义。测试过程中还会通过信号与 `waitpid()` 协调父子进程；测试框架清理临时目录时会调用 `unlinkat(..., AT_REMOVEDIR)` 删除工作目录。

## 现象

用户提供的 `log.ans` 和 `ana.ans` 中主要有三类异常：

```text
tst_test.c:1654: TBROK: waitpid(3,0x2fff447b68,0) failed: ERESTART (85)
access01.c:245: TFAIL: access(accessfile_rwx, F_OK) as nobody failed: ENOENT (2)
```

旧日志中还曾出现：

```text
access01.c:281: TBROK: Failed to chmod file 'accessfile_w': ENOENT (2)
```

`ana.ans` 的 syscall 时序显示：

```text
[PID 3] SigKill ret = 0
[PID 2] Wait4 ret = Interrupted system call should be restarted
[PID 2] SigReturn ret = -85
tst_test.c:1654: TBROK: waitpid(...) failed: ERESTART (85)
...
tst_tmpdir.c:342: TWARN: tst_rmdir ... errno=39: ENOTEMPTY
```

## 分析

`waitpid()` 返回 `ERESTART` 是第一个关键错误。`ERESTART` 是内核内部 errno，只能交给信号返回路径决定重启 syscall 或转成 `EINTR`，不能暴露给用户态。`ana.ans` 显示 PID 3 向父进程发送信号后，PID 2 的 `wait4` 返回 `ERESTART`，随后 `sigreturn` 恢复出 `-85`，LTP 将其解释为用户可见 errno 并立刻 `TBROK`。

`setup_frame()` 中已有处理 `a0 == -ERESTART` 的逻辑，但它依赖 `get_trap_cause() == Syscall`。实际 signal frame 是在 `trap_return()` 阶段构造的，此时当前 CSR trap cause 不一定还能作为“刚从 syscall 返回”的可靠判据。结果保存到用户 signal frame 的 `MachineContext` 仍带着 `a0=-ERESTART`，`sigreturn` 后内部 errno 直接进入用户态。

同时，syscall trap 入口没有保存原始 `a0` 到 `TrapContext::origin_a0`。即使 `setup_frame()` 进入重启分支，也无法恢复 wait4 的原始第一个参数。

`waitpid` 提前 `TBROK` 后，LTP cleanup 与仍在运行的子进程并发，导致临时目录里仍有条目时父进程尝试删除目录。这里暴露出第二个文件系统语义缺口：Linux `rmdir`/`unlinkat(AT_REMOVEDIR)` 对非空目录必须返回 `ENOTEMPTY`；当前 `sys_unlinkat()` 未检查目录是否为空，底层 ext4 目录删除委托到 lwext4 的递归 `dir_rm()`，语义比 Linux 更宽松。

## 根因

1. syscall trap 入口未保存原始 `a0`，信号重启路径无法恢复原 syscall 参数。
2. `setup_frame()` 用当前 trap cause 判断 `ERESTART`，在 `trap_return()` 构造 signal frame 时不可靠，导致内部 `ERESTART` 被保存进用户上下文并由 `sigreturn` 暴露给 LTP。
3. `sys_unlinkat()` 未在调用底层 ext4 `dir_rm()` 前检查目录是否为空，导致 `AT_REMOVEDIR` 可能变成递归删除语义。

## 修复

- `os/src/trap/mod.rs`
  - syscall trap 入口在 `sepc_step(4)` 前保存 `origin_a0 = a0`，供 `SA_RESTART` 路径恢复原 syscall 参数。
- `os/src/signal/mod.rs`
  - `setup_frame()` 处理 `-ERESTART` 时不再依赖当前 trap cause，只要 trap context 的 `a0` 仍是内部 `ERESTART`，就在保存用户 `MachineContext` 前按 `SA_RESTART` 决定回退 `sepc` 并恢复原 `a0`，或转换为 `EINTR`。
- `os/src/fs/vfs.rs`
  - 为 `Inode` 增加 `is_dir_empty()` 接口，供 `unlinkat(AT_REMOVEDIR)` 在删除前判断目录内容。
- `os/src/fs/ext4_lw/inode.rs`
  - 基于已有 `read_dir_from(0)` 遍历目录项，忽略 `.` 和 `..`，发现其他目录项即返回非空。
  - 非目录调用返回 `ENOTDIR`。
- `os/src/syscall/fs/ctl.rs`
  - 普通文件带 `AT_REMOVEDIR` 时返回 `ENOTDIR`。
  - 目录未带 `AT_REMOVEDIR` 时返回 `EISDIR`。
  - 目录带 `AT_REMOVEDIR` 但非空时返回 `ENOTEMPTY`，避免进入递归 `dir_rm()`。

## 验证

已执行：

```text
make
timeout 120s make run > /tmp/access01-wait-restart.log 2>&1
```

结果：

- `make` 在默认 `riscv64` 配置下通过。
- 沙箱外 `make run` 正常退出，未超时。
- `/tmp/access01-wait-restart.log` 中 `access01` 输出：

```text
Summary:
passed   199
failed   0
broken   0
skipped  0
warnings 0
#### OS COMP TEST GROUP END ltp-musl ####
shutdown!
```

- 未再出现 `waitpid(... ) failed: ERESTART`、`accessfile_rwx ... ENOENT`、`Failed to chmod file 'accessfile_w'` 或 cleanup 死循环。
- wrapper 仍打印 `FAIL LTP CASE access01 : 10`，但 LTP Summary 显示 `failed 0 broken 0`，按项目规则以 `TPASS`/`Summary` 为准。
