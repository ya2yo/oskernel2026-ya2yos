# LTP access04 faccessat errno 语义修复

## 背景

LTP `access04` 使用 `access()`/`faccessat()` 检查异常路径的 errno 优先级，覆盖非法 mode、文件不存在、超长文件名、非目录路径、循环符号链接，以及只读挂载上的 `W_OK` 检查。

本轮 `initproc` 运行 `ltp-musl` 单测 `access04`，`log.ans` 显示测例核心断言有 4 项失败。

## 现象

失败集中在两类 errno：

```text
access04.c:68: TFAIL: access as root expected ENAMETOOLONG: ENOENT (2)
access04.c:68: TFAIL: access as nobody expected ENAMETOOLONG: ENOENT (2)
access04.c:68: TFAIL: access as root succeeded
access04.c:68: TFAIL: access as nobody expected EROFS: EACCES (13)
```

前两项使用 256 字节文件名分量检查 `R_OK`，Linux 语义应返回 `ENAMETOOLONG`。后两项在 `tmpfs` 只读挂载点 `mntpoint` 上检查 `W_OK`，Linux 语义应返回 `EROFS`，并且该错误应优先于普通权限位检查。

## 分析

`sys_faccessat()` 原本只检查 `path.len() > MAX_PATH_LEN`。当前内核的 `MAX_PATH_LEN` 为 256，`read_user_cstr()` 最多读取 256 字节，LTP 传入的相对路径文件名刚好为 256 字节时不会触发 `path.len() > MAX_PATH_LEN`，随后路径解析尝试打开 `/tmp/.../<long-name>`，最终返回 `ENOENT`。

这类用例测的是单个路径分量超过 Linux `NAME_MAX=255`，不是整个路径超过 `PATH_MAX`。因此需要在 syscall 层对每个 path component 单独检查。

只读挂载失败来自另一个路径匹配问题。原实现只在 `mode.contains(W_OK)` 时调用：

```text
MNT_TABLE.lock().got_mount(path.clone())
```

这里有两个缺陷：

- `path` 仍可能是相对路径，例如 `mntpoint`，而挂载表记录的是解析后的绝对路径 `/tmp/LTP.../mntpoint`。
- `got_mount()` 只做精确匹配，无法识别位于只读挂载点下的路径。

因此 root 对 `mntpoint` 的 `W_OK` 被错误放行，nobody 则继续进入普通权限检查并返回 `EACCES`。

## 根因

1. `sys_faccessat()` 混淆了完整路径长度限制与单个文件名分量长度限制，缺少 `NAME_MAX=255` 检查。
2. 只读挂载判断使用未解析的输入路径，并且挂载表只能精确匹配挂载点，不能按最长挂载点前缀查找目标路径所属的 mount。
3. `W_OK` 对只读文件系统的 `EROFS` 判断发生在错误的路径匹配基础上，导致错误优先级不符合 Linux 语义。

## 修复

### 1. 补齐文件名分量长度检查

`os/src/syscall/fs/stat.rs` 新增 `MAX_FILE_NAME_LEN = 255` 与 `has_too_long_path_component()`。`sys_faccessat()` 在实际路径解析前检查每个 `/` 分隔的非空分量，任一分量超过 255 字节即返回 `ENAMETOOLONG`。

同时将 `FaccessatMode::from_bits(mode).unwrap()` 改为 `ok_or(SysErrNo::EINVAL)?`，避免非法用户 mode 触发内核 panic。

### 2. 按绝对路径查找所属挂载点

`os/src/fs/mount.rs` 新增 `MountTable::mount_for_path(&self, path)`，按最长挂载点前缀匹配目标路径，支持：

- 目标路径正好等于挂载点。
- 目标路径位于挂载点子树下。
- 多个挂载点嵌套时选择最长匹配。

### 3. 调整 `EROFS` 判断位置

`sys_faccessat()` 先解析 `abs_path` 并确认目标 inode 存在，再对 `W_OK` 调用 `mount_for_path(&abs_path)`。如果所属挂载点带只读标志，则返回 `EROFS`，优先于普通 uid/gid 权限位判断。

## 涉及文件

| 文件 | 修改 |
|------|------|
| `os/src/syscall/fs/stat.rs` | `faccessat` 增加 `NAME_MAX` 分量检查；非法 mode 改为返回 `EINVAL`；`W_OK` 使用绝对路径做只读挂载判断 |
| `os/src/fs/mount.rs` | 新增 `mount_for_path()`，按最长挂载点前缀查找路径所属 mount |

## 验证

已执行：

```text
make
```

结果：默认 RISC-V 构建通过，仅有既有 warning。

沙箱内尝试运行：

```text
timeout 120s make run > /tmp/access04-faccessat.log 2>&1
```

QEMU 因沙箱 `/var/tmp` 只读无法创建临时文件而未启动：

```text
qemu-system-riscv64: ... Could not open temporary file '/var/tmp/...': Read-only file system
```

随后维护者在外部环境运行并更新 `log.ans`。AI 读取最新 `log.ans`，`access04` 12 项核心断言均通过：

```text
access04.c:68: TPASS: access as root : ENAMETOOLONG (36)
access04.c:68: TPASS: access as nobody : ENAMETOOLONG (36)
access04.c:68: TPASS: access as root : EROFS (30)
access04.c:68: TPASS: access as nobody : EROFS (30)

Summary:
passed   12
failed   0
broken   0
skipped  0
warnings 0
```

最终总 summary 为：

```text
passed   12
failed   0
broken   0
skipped  0
warnings 1
```

其中 `warnings 1` 来自 LTP cleanup 阶段删除循环符号链接时报 `ELOOP`，不影响 `access04` 核心断言。日志中仍有包装器行 `FAIL LTP CASE access04 : 10`，但本仓库判读 LTP 结果时以 `TPASS/TFAIL/TBROK/Summary` 为准。

未执行 `TARGET_ARCH=loongarch64` 验证；本次修复和验证基于当前默认 RISC-V 配置。
