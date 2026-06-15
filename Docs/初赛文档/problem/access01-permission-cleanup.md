# access01 权限判断与 cleanup 卡死

## 背景

LTP `access01` 会组合不同文件/目录权限，并分别以 root 和 nobody 身份调用 `access()`/`faccessat()`，验证真实 uid/gid 下的访问权限语义。测例结束后，LTP 框架还会清理 `/dev/shm/ltp_access01_*` 临时文件和相关 mmap。

## 现象

`log.ans` 中 `access01` 最初出现 20 项 `TFAIL`，主要集中在 nobody 对 owner/group/other 权限位的访问判断。修复权限判断后，测例内部 Summary 已经变为 `passed 199 failed 0`，但日志仍停在 Summary 后，外层 LTP wrapper 没有打印 `GROUP END` 和 `shutdown!`。

## 分析

权限失败来自 `sys_faccessat()` 的判断口径：旧实现只要任一 owner/group/other 权限位满足就放行或拒绝，没有先根据调用者真实 uid/gid 选择对应的权限类别；同时 `access()` 语义应使用 real uid/gid，而不是 effective uid/gid。

Summary 后的卡死不是 LTP 主体断言失败。临时日志显示 `access01` 主体已退出，父层清理 `/dev/shm/ltp_access01_2` 后进入 `munmap(0x2a23446000, 0x1000)` 不返回。进一步定位到 `munmap` 对 MAP_SHARED|PROT_WRITE 的 mmap 做写回时，backing inode 已经被 cleanup unlink，当前 lwext4 路径再写回该无目录项临时文件会卡住。

清理路径还暴露出两个文件系统兼容问题：`sys_unlinkat()` 用 `O_UNLINK` 打开目标并用相对 path 判断是否仍有 fd，容易误判；lwext4 删除目录需要使用 `dir_rm()`，不能统一走普通文件删除接口。

## 根因

1. `faccessat` 未按 Linux `access()` 语义使用 real uid/gid 和 owner/group/other 分类权限。
2. `unlinkat` 的打开方式和 fd 路径匹配不适合 cleanup 场景，目录删除还需要调用 lwext4 的目录删除接口。
3. `munmap` 对已经 unlink 的 MAP_SHARED backing file 仍尝试同步写回，导致 Summary 后退出清理卡死。

## 修复

- `os/src/syscall/fs/stat.rs`
  - 新增权限位辅助判断，按 `st_uid/st_gid` 选择 owner/group/other 权限位。
  - `faccessat` 使用 real uid/gid，并保留 root 对 `X_OK` 至少需要一个执行位的语义。
  - 父目录和目标文件检查改为只读打开，避免访问检查引入写权限副作用。
- `os/src/syscall/fs/ctl.rs`
  - `unlinkat` 支持并校验 `AT_REMOVEDIR` flag，目标打开改为 `O_RDONLY`。
  - 使用绝对路径判断 `has_fd()`，避免相对 path 导致延迟删除误判。
- `os/src/fs/ext4_lw/inode.rs`
  - 目录 unlink 调用 `dir_rm()`，普通文件仍调用 `file_remove()`。
- `os/src/mm/memory_set/mmap_ops.rs`
  - MAP_SHARED 写回前检查 backing inode 链接数；若已为 0，说明文件已被 cleanup unlink，跳过写回并直接释放映射，避免在 `munmap` 中卡死。

## 验证

已执行：

```text
make
timeout 120s make run
```

结果：

```text
Summary:
passed   199
failed   0
broken   0
skipped  0
warnings 0
FAIL LTP CASE access01 : 0
Summary:
passed   199
failed   0
broken   0
skipped  0
warnings 0
#### OS COMP TEST GROUP END ltp-musl ####
shutdown!
```
