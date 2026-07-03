# LTP acct01 acct errno 语义修复

## 背景

LTP `acct01` 覆盖 `acct(2)` 开启或关闭进程记账时的错误路径，重点检查目录、设备文件、缺失路径、尾随 `/`、非特权用户、`NULL`、循环符号链接、超长文件名和只读挂载等场景的 errno。

本轮 `log.ans` 中 `acct01` 多项核心断言失败，说明 `sys_acct()` 的错误优先级和路径检查还不符合 Linux 语义。

## 现象

修复前日志中的失败项：

```text
acct01.c:115: TFAIL: acct(./tmpfile/) succeeded
acct01.c:115: TFAIL: acct(./tmpfile) expected EPERM: EACCES (13)
acct01.c:115: TFAIL: acct(NULL) succeeded
acct01.c:115: TFAIL: acct(aaaa...) expected ENAMETOOLONG: ENOENT (2)
acct01.c:115: TFAIL: acct(ro_mntpoint/file) succeeded
```

对应 syscall 轨迹显示：

- `acct(./tmpfile/)` 被 `get_abs_path()` 规范化成 `/tmp/.../tmpfile`，尾随 `/` 被吞掉，最终把普通文件当作合法 accounting 文件。
- 非特权场景使用 `setresuid(-1, nobody, -1)` 只改变 effective uid。原 `sys_acct()` 检查 real uid，随后 `open(O_WRONLY)` 返回 `EACCES`，覆盖了 `acct(2)` 自身应返回的 `EPERM`。
- `acct(NULL)` 在 root 下被当作关闭 accounting 并成功返回，但该测例在降权后期待 `EPERM`。
- 256 字节文件名分量缺少 `NAME_MAX` 检查，最终路径查找返回 `ENOENT`。
- 只读 `tmpfs` 挂载点上的 accounting 文件未检查 mount 只读标志，root 打开写文件成功后错误开启 accounting。

## 分析

原 `sys_acct()` 的检查顺序是：

1. `filename == NULL` 时按 real uid 决定是否关闭 accounting。
2. 非 `NULL` 时仍按 real uid 判断是否 root。
3. 读取路径，只检查整条路径长度。
4. 用 `open(abs_path, O_WRONLY)` 验证并保存文件。

这会带来几类问题：

- `acct(2)` 需要 `CAP_SYS_PACCT`，本项目当前等价于 effective uid 为 0；只看 real uid 会把 `setresuid(-1, nobody, -1)` 的降权场景误判为 root。
- `open(O_WRONLY)` 的普通文件权限检查发生得太早，会把 `EPERM`、`EROFS` 等 `acct(2)` 自身语义覆盖为 `EACCES` 或成功。
- `get_abs_path()` 会规范化尾随 `/`，因此 `./tmpfile/` 必须在规范化前保留并单独处理，否则普通文件路径会被误接受。
- `MAX_PATH_LEN` 是当前内核用户字符串读取上限，但 LTP 此处测试的是单个文件名分量超过 Linux `NAME_MAX=255`。
- 只读挂载需要基于解析后的绝对路径查找所属 mount，而不是只依赖底层文件打开权限。

## 根因

1. `sys_acct()` 使用 real uid 判断特权，和 `acct(2)` 所需的 effective capability 语义不一致。
2. 直接 `O_WRONLY` 打开目标文件导致 VFS 普通权限错误覆盖 `acct(2)` 应返回的 `EPERM`/`EROFS`。
3. 路径规范化前没有处理尾随 `/`，缺少 `NAME_MAX` 文件名分量检查。
4. 目标文件位于只读挂载点时缺少 `EROFS` 判断。

## 修复

### 1. 改为 effective uid 特权判断

`sys_acct()` 读取 `task_inner.effective_uid`。非 root effective uid 对 `acct(NULL)` 和 `acct(path)` 都先返回 `EPERM`，避免后续文件权限检查覆盖 syscall 语义。

### 2. 补齐路径错误检查

新增 `MAX_FILE_NAME_LEN = 255` 和 `has_too_long_path_component()`：

- 空路径返回 `ENOENT`。
- 整条路径超过当前读取上限返回 `ENAMETOOLONG`。
- 任一路径分量超过 255 字节返回 `ENAMETOOLONG`。

对 `path.ends_with('/') && path != "/"` 的输入，在路径规范化前识别尾随斜杠，并用 `open(abs_path, O_RDONLY | O_DIRECTORY)` 保持 Linux 风格错误：目标是普通文件时返回 `ENOTDIR`。

### 3. 分离路径验证和最终写打开

先用 `O_RDONLY` 打开目标路径，确认路径存在且目标是普通文件。这样可以先处理目录、设备和 symlink loop 等路径语义。

确认目标普通文件后，使用 `MNT_TABLE.mount_for_path(&abs_path)` 判断所属挂载点是否只读；若只读则返回 `EROFS`。

最后再用 `O_WRONLY` 打开一次，并把可写 `OSFile` 保存到 `ACCT_FILE`，确保后续进程退出时仍能写 accounting 记录，不回归 `acct02` 的记录写出能力。

## 涉及文件

| 文件 | 修改 |
|------|------|
| `os/src/syscall/task/acct.rs` | `sys_acct()` 改用 effective uid；补齐空路径、`NAME_MAX`、尾随 `/`、只读挂载检查；分离只读路径验证和最终可写文件保存 |

## 验证

已执行：

```text
make
```

结果：默认 RISC-V 构建通过，仅有既有 warning。

沙箱内尝试运行：

```text
timeout 120s make run > /tmp/acct01-fix.log 2>&1
```

QEMU 因沙箱 `/var/tmp` 只读无法创建临时文件而未启动：

```text
qemu-system-riscv64: ... Could not open temporary file '/var/tmp/...': Read-only file system
```

随后维护者在外部环境运行并更新 `log.ans`。AI 读取最新 `log.ans`，`acct01` 9 项核心断言均通过：

```text
acct01.c:115: TPASS: acct(.) : EISDIR (21)
acct01.c:115: TPASS: acct(/dev/null) : EACCES (13)
acct01.c:115: TPASS: acct(/tmp/does/not/exist) : ENOENT (2)
acct01.c:115: TPASS: acct(./tmpfile/) : ENOTDIR (20)
acct01.c:115: TPASS: acct(./tmpfile) : EPERM (1)
acct01.c:115: TPASS: acct(NULL) : EPERM (1)
acct01.c:115: TPASS: acct(test_file_eloop1) : ELOOP (40)
acct01.c:115: TPASS: acct(aaaa...) : ENAMETOOLONG (36)
acct01.c:115: TPASS: acct(ro_mntpoint/file) : EROFS (30)

Summary:
passed   9
failed   0
broken   0
skipped  0
warnings 0
```

最终总 summary 为：

```text
passed   9
failed   0
broken   0
skipped  0
warnings 1
```

其中 `warnings 1` 来自 LTP cleanup 阶段删除循环符号链接时报 `ELOOP`，不影响 `acct01` 核心断言。日志中仍有包装器行 `FAIL LTP CASE acct01 : 10`，但本仓库判读 LTP 结果时以 `TPASS/TFAIL/TBROK/Summary` 为准。

未执行 `TARGET_ARCH=loongarch64` 验证；本次修复和验证基于当前默认 RISC-V 配置。
