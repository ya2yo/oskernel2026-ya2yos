# basic umount 相对挂载点路径不匹配

## 背景

basic-musl 和 basic-glibc 都会在当前工作目录下执行：

```text
mount("/dev/vda2", "./mnt", ...)
umount("./mnt")
```

内核的挂载表只做字符串匹配，因此 mount 和 umount 对同一挂载点必须使用一致的路径表示。

## 现象

`log.ans` 中 basic 测试的 `test_mount` 阶段 mount 成功，但紧随其后的 umount 返回 `-22`：

```text
Testing mount :
========== START test_mount ==========
Mounting dev:/dev/vda2 to ./mnt
mount return: 0
mount successfully
umount return: -22
```

后续独立 `test_umount` 再次挂载后触发 `Assert Fatal`。

## 分析

`sys_umount2()` 会先通过 `proc_inner.get_abs_path(AT_FDCWD, target)` 将用户传入路径转成绝对路径，再调用 `MNT_TABLE.umount()`。

但 `sys_mount()` 旧实现直接把用户传入的 `dir` 原样写入挂载表。例如测试传入 `./mnt` 时，挂载表中保存的是 `./mnt`；随后 `umount("./mnt")` 查表时使用的是 `/musl/basic/mnt` 或 `/glibc/basic/mnt`，字符串不相等，于是返回 `EINVAL`。

## 根因

mount 和 umount 对挂载点路径的规范化策略不一致：

- `sys_mount()` 保存相对路径原文；
- `sys_umount2()` 使用当前进程工作目录解析后的绝对路径。

## 修复

在 `os/src/syscall/fs/mount.rs` 中，`sys_mount()` 读取 `dir` 后调用：

```rust
let dir = proc_inner.get_abs_path(AT_FDCWD as isize, &dir)?;
```

这样挂载表中的挂载点路径与 `sys_umount2()` 查表路径保持一致。`special`/source 不做路径规范化，避免把 `none`、`tmpfs` 等非路径 source 错当文件路径处理。

## 涉及文件

| 文件 | 修改 |
|------|------|
| `os/src/syscall/fs/mount.rs` | `sys_mount()` 写入挂载表前规范化目标挂载点路径 |
| `Docs/初赛文档/开发日志.md` | 记录本次修复 |
| `Docs/初赛文档/problem/README.md` | 更新问题索引 |
| `Docs/初赛文档/ai.log` / `Docs/初赛文档/AI_INTERACTION.md` | 记录 AI 辅助分析与验证 |

## 验证

已执行：

```text
make
timeout 120s make run
```

结果：

```text
#### OS COMP TEST GROUP START basic-musl ####
...
Testing mount :
...
umount return: 0
...
Testing umount :
...
umount success.
return: 0
#### OS COMP TEST GROUP END basic-musl ####

#### OS COMP TEST GROUP START basic-glibc ####
...
Testing mount :
...
umount return: 0
...
Testing umount :
...
umount success.
return: 0
#### OS COMP TEST GROUP END basic-glibc ####
shutdown!
```
