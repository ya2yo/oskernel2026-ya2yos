# LTP access02（execve shebang）

## 背景

LTP `access02`（filelist index 7）测试 `access()` / `faccessat()` 在 F_OK/R_OK/W_OK/X_OK 下的行为，并对符号链接做同样检查。X_OK 分支在 `access()` 通过后还会 `system("./file_x")` 验证文件确实可执行。

setup 创建 `file_x`（mode `0555`），内容为 `#!/bin/sh\n`（shebang 脚本，非 ELF）。

## 现象

LoongArch + `sdcard-la.img`，12 TPASS / 4 TFAIL，退出码 256：

```text
sh: can't execute './file_x': Exec format error
access02.c:129: TFAIL: execute file_x as root failed: SUCCESS (0)
access02.c:129: TFAIL: execute file_x as nobody failed: SUCCESS (0)
```

`access(file_x, X_OK)` 与符号链接相关用例均已 TPASS；失败集中在 X_OK 的 **执行验证** 阶段（各测 root / nobody 各一次，共 4 次 TFAIL）。

## 分析

LTP `access02.c` X_OK 逻辑：

1. `access(pathname, X_OK)` 须成功
2. `system("./file_x")` 须成功（返回 0）

`file_x` 是 shebang 脚本。Linux 内核在 `execve` 时读取 `#!` 行，转而加载解释器（如 `/bin/sh`），argv 为 `[解释器, 脚本路径, ...]`。

本内核 `sys_execve` 原先只对 ELF 魔数 `\x7fELF` 加载；非 ELF 直接 `ENOEXEC`。busybox `sh` 尝试执行脚本时收到 `ENOEXEC`，打印 “Exec format error”，`system()` 非零返回，LTP 报 TFAIL。

已有 `.sh` 后缀特判（改走 `/musl/busybox sh`）**不覆盖** `file_x` 这种无 `.sh` 后缀的 shebang 文件。

`/bin/sh` → `/musl/busybox` 的 symlink 已在 `initfiles.rs` 中创建（早期 access02 符号链接修复时加入），解释器路径本身可用。

## 修复

`os/src/syscall/task/execve.rs`：

1. 增加 `parse_shebang()`：解析 `#!` 行中的解释器路径及可选参数（对齐 Linux `binfmt_script`）
2. 非 ELF 且含 shebang 时：
   - 重建 argv：`[interp, (shebang_arg?), script_abs_path, ...原 argv[1..]]`
   - 打开解释器 ELF 并 `task.exec`
3. 非 ELF 且无 shebang → 仍返回 `ENOEXEC`

## 涉及文件

| 文件 | 改动 |
|------|------|
| `os/src/syscall/task/execve.rs` | `is_elf`、`parse_shebang`、shebang 分支加载解释器 |

## 验证

LoongArch QEMU 单跑 `access02`（`LTP_TEST_START=7`）：全部 TPASS，退出码 0。

## 历史相关修复（同测例早期）

| 问题 | 修复 |
|------|------|
| fork 子进程 trap 未初始化 LoadPageFault | fork 时复制父进程 `trap_cx` |
| 子进程退出后父进程 StorePageFault | 进程退出时按 `memory_set` 引用计数回收 |
| 符号链接相对路径 + 缺 `/bin/sh` | `ext4_lw/inode.rs` 相对 symlink 解析；`initfiles.rs` 创建 `/bin/sh` symlink |

上述已在更早迭代中完成；本次 LoongArch 回归失败仅因 shebang 未实现。
