# open02 O_NOATIME 权限检查缺失

## 背景

Linux `open(2)` 指定 `O_NOATIME` 时，调用者必须是目标 inode 的 owner，或在用户命名空间中持有 `CAP_FOWNER`；否则系统调用返回 `EPERM`。该约束避免无权限用户在读取任意文件时抑制 atime 更新。

LTP `open02` 以 root 创建 `test_file2`，随后将 effective uid 切换为 `nobody`，验证：

```text
open("test_file2", O_RDONLY | O_NOATIME) == -1, errno == EPERM
```

## 现象

新的 `log.ans` 中，musl 和 glibc 均报告：

```text
TPASS: open() new file without O_CREAT : ENOENT (2)
TFAIL: open() unprivileged O_RDONLY | O_NOATIME succeeded

Summary:
passed   1
failed   1
broken   0
```

文件不存在的常规 `ENOENT` 路径已经正确，失败仅限既有文件的 `O_NOATIME` 授权。

## 分析

`sys_openat()` 解析用户参数、得到绝对路径后统一调用 `fs::open()`；后者在 `open_inner()` 找到既有 inode 后构造 `OSFile`。原逻辑只在 writable open 时检查普通 Unix mode 权限，并未处理 `O_NOATIME`。

因此 `nobody` 能以只读方式打开 root 创建的 `0644` 文件，且 `O_NOATIME` 被原样接受。`setreuid` 在 effective uid 变为非 root 时会清空 effective capabilities，所以 LTP 调用者既不是 owner，也不具有 `CAP_FOWNER`，应被拒绝。

## 根因

现有 VFS open 路径把 `O_NOATIME` 当作普通状态 flag 保存，却遗漏了 Linux 所要求的 inode owner / capability 授权检查。

## 修复

在 `os/src/fs/kernel_fs_ops/open.rs` 增加 `check_noatime_permission()`，并在既有 inode 已完成 `O_EXCL` 与 `O_DIRECTORY` 检查后、构造 `OSFile` 前调用。

- 不含 `O_NOATIME` 时不改变原有路径；
- 启动期没有 current task 的内部 `open()` 保持允许；
- 有 task 时先短暂读取 effective uid 与 `CAP_FOWNER` 位并释放 task 锁；
- 随后读取 inode `st_uid`，euid 等于 owner 或有效 capability 集含 `CAP_FOWNER` 则允许，否则返回 `EPERM`。

该锁边界避免在持有 task lock 时进入 inode/filesystem 元数据路径。

## 涉及文件

| 文件 | 修改 |
| --- | --- |
| `os/src/fs/kernel_fs_ops/open.rs` | 增加既有 inode 的 `O_NOATIME` owner/`CAP_FOWNER` 权限检查 |

## 验证

已执行：

```text
rustfmt --edition 2021 --check os/src/fs/kernel_fs_ops/open.rs
make
timeout 180s make run > /tmp/open02-noatime-fix.log 2>&1
```

结果：

- `rustfmt --check` 与 `git diff --check` 通过；
- `make` 完成 RISC-V 与 LoongArch64 构建，仅有既有 vendored `smoltcp` warning；
- LoongArch64 QEMU 下 musl 与 glibc `open02` 均通过：

```text
TPASS: open() new file without O_CREAT : ENOENT (2)
TPASS: open() unprivileged O_RDONLY | O_NOATIME : EPERM (1)

Summary:
passed   2
failed   0
broken   0
skipped  0
warnings 0
...
shutdown!
```

`FAIL LTP CASE open02 : 0` 是 musl 单测包装器对成功退出码的既有输出，应以 LTP Summary 为准。
