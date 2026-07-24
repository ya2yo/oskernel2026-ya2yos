# LTP mount03 EOF 读取未更新 atime

## 背景

LTP `mount03` 验证 `MS_NOATIME`、`MS_NODIRATIME` 和 `MS_STRICTATIME` 对普通文件及目录访问时间的影响。测试写入普通文件后不重置 fd 偏移，等待一秒再读取，因此该次 `read()` 从 EOF 返回 0；Linux 仍将其视为成功访问并在未设置 `MS_NOATIME` 时更新普通文件 atime。

## 现象

修复前 musl 和 glibc 分别在 ext2、ext4、tmpfs 三类挂载上各有两项失败：

```text
mount03.c:140: TFAIL: st.st_atime(0) < atime(0)
```

`MS_NOATIME` 的普通文件和目录断言、以及 `MS_NODIRATIME`/`MS_STRICTATIME` 的目录断言均已通过，范围收敛到普通文件的 atime 更新。

## 分析

`OSFile::read()` 在 `inode.size() <= offset` 时直接返回 EOF，不会调用 `Inode::read_at()`。先前仅在 `Ext4Inode::read_at()` 中补充更新时间，无法覆盖该快速路径；而 `mount03` 恰好固定触发它。

lwext4 的路径型 `set_time()` 需要关闭该 inode 持有的临时读句柄后执行。目录读取路径已证明此顺序可持久化时间戳，因此普通文件也需要一个独立的 inode 级访问时间操作。

## 根因

普通文件成功 EOF 读取绕过了底层 inode 读取函数，内核没有记录此次访问，导致 `MS_NODIRATIME` 与 `MS_STRICTATIME` 下的 `st_atime` 保持旧值。

## 修复

- 在 `Inode` 增加默认无操作的 `touch_atime()`；只有实际文件系统 inode 覆盖它，避免改变设备、管道等其他文件对象语义。
- `Ext4Inode::touch_atime()` 查询 `MNT_TABLE`：`MS_NOATIME` 时不更新，否则关闭临时句柄并通过 lwext4 更新 atime，错误向上返回而不再静默吞掉。
- `Ext4Inode::read_at()` 在成功读取后调用 `touch_atime()`。
- `OSFile::read()` 的 EOF 快速返回前同样调用 `touch_atime()`，使返回 0 的成功读取符合 Linux atime 语义。

`MS_NODIRATIME` 仍只抑制目录 atime；普通文件在该标志下会更新。`MS_NOATIME` 则同时抑制普通文件和目录 atime。

## 涉及文件

- `os/src/fs/vfs.rs`
- `os/src/fs/ext4_lw/inode.rs`
- `os/src/fs/files/os_file.rs`

## 验证

- `make build-arch TARGET_ARCH=riscv64`：通过；仅有既有 `smoltcp` warning。
- 最新根目录 `log.ans`：musl 与 glibc 的 `mount03` 均为 `passed 55 failed 0 broken 0 skipped 0 warnings 0`，正常 `shutdown!`。
- `MS_NOATIME` 不更新普通文件/目录 atime，`MS_NODIRATIME` 只更新普通文件 atime，`MS_STRICTATIME` 更新普通文件和目录 atime，三种语义均为 `TPASS`。
- `git diff --check`：通过。

本轮未运行 LoongArch64 QEMU 或完整 LTP 批量套件。
