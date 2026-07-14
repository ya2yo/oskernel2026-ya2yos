# LTP open11 目录打开 flags 语义修复

## 背景

LTP `open11` 同时在 musl 与 glibc 轮次出现 3 个失败：符号链接解析后的目录以 `O_WRONLY` 打开错误成功，以及已有目录（直接路径和符号链接路径）以 `O_RDONLY | O_CREAT` 打开错误成功。

## 现象

原始 `log.ans` 的两轮 summary 均为 `passed 25 failed 3 broken 0`。失败断言均期望 `EISDIR`：

- `open symlink directory O_WRONLY`
- `open symlink directory O_RDONLY | O_CREAT`
- `open directory O_RDONLY | O_CREAT`

LTP 源码位于 `ltp-full-20240524/testcases/kernel/syscalls/open/open11.c`。符号链接已被正确解析为目录 inode，失败发生在解析完成后的 inode flags 校验阶段。

## 根因

`open_inner()` 原先仅以 `flags.contains(O_RDWR)` 拒绝目录。`O_RDWR` 的 access mode 值为 2，而 `O_WRONLY` 的值为 1，因此 `O_WRONLY` 不会命中该判断。对已存在目录的 `O_CREAT` 也没有单独处理，最终错误创建了普通 `OSFile`。

同时不能简单按 flag 位相交判断写意图：Linux 的 `O_PATH` 是路径句柄，除 `O_CLOEXEC`、`O_DIRECTORY`、`O_NOFOLLOW` 外的 open flags 都应忽略。若把 `O_PATH | O_WRONLY` 或 `O_PATH | O_CREAT` 当作普通写/创建打开，会破坏路径句柄语义。

## 修复

在 `open_inner()` 找到 inode 后：

- 以 `OpenFlags::read_write()` 取得实际读写能力，避免把 `O_WRONLY` 与 `O_RDWR` 当作可组合 bit；
- 目录带真实写意图（写 access mode，或非 `O_PATH` 的 `O_TRUNC`）时返回 `EISDIR`；
- 已存在目录带非 `O_PATH` 的 `O_CREAT` 时返回 `EISDIR`；
- `O_PATH` 不再触发创建、排他创建、截断或 `O_NOATIME` 权限检查；
- 保持 `O_DIRECTORY` 对非目录返回 `ENOTDIR`，并保持 `O_CREAT | O_EXCL`、`O_TMPFILE` 的既有专用创建路径。

## 涉及文件

- `os/src/fs/kernel_fs_ops/open.rs`
- `os/src/syscall/fs/ctl.rs`

`sys_symlinkat()` 取得父目录时同步改为 `O_DIRECTORY | O_RDONLY`，不再将目录作为可写普通文件打开；目录写权限仍由随后实际的创建操作检查。

## 验证

- `cargo fmt --manifest-path os/Cargo.toml` 通过。
- `make` 通过，包含 RISC-V 与 LoongArch64 构建；仅出现既有 vendored `smoltcp` warning。
- RISC-V `make run` 已启动到用户态，但 musl/glibc `open11` 均在输出 LTP 断言前以 `IllegalInstruction` 退出，未获得有效的测试判断结果。
- LoongArch64 `make TARGET_ARCH=loongarch64 run` 因沙箱无法在只读 `/var/tmp` 创建 QEMU 临时文件而未启动。

因此构建验证已完成，`open11` 的运行回归仍需在可正常启动两种测试镜像的环境中复跑。
