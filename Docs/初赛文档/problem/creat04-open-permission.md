# open(O_CREATE) / open(existing, w) 文件权限检查

## 背景

LTP `creat04` 测例报告 `TFAIL: call succeeded unexpectedly`（creat04.c:40）。测试逻辑为：
1. 创建测试目录，`fchownat` 改变 owner，`fchmodat` 限制权限
2. `setresuid` 切换到非特权用户（只改变 effective uid，real uid 仍为 0）
3. 调用 `creat()`（等价于 `open(O_CREATE|O_WRONLY|O_TRUNC)`）→ 应返回 `EACCES`

此前内核中 **`create_file` 完全没有权限检查**，且 `open()` 打开已有文件时也不检查写权限，导致 creat 永远成功。

## 根因分析

| 子问题 | 说明 |
|--------|------|
| `create_file` 无权限检查 | `open(O_CREATE)` 创建新文件时，未检查父目录写/执行权限 |
| `sys_fchownat` 是 stub | 永远返回 `Ok(0)`，不改变文件 owner |
| `open` 无已有文件写检查 | 以写模式打开已有文件时，未检查文件的写权限 |
| root 判定用错字段 | 最初使用 `user_id`（real uid）判断 root→绕过权限。但 `setresuid(-1, nobody, -1)` 只改 effective uid，real uid 仍为 0，导致非 root 进程被误判为 root |

## 实现要点

### 1. `create_file`：检查父目录的写+执行权限

在 `os/src/fs/kernel_fs_ops/open.rs` 的 `create_file` 函数中，创建文件前：

- 从 `abs_path` 提取父目录路径
- 通过 `FsIndex` 或 `superblock_root_inode().find()` 查找父目录 inode
- 获取父目录的 `fmode()`（权限位）和 `fstat()`（owner uid/gid）
- 获取当前进程的 `effective_uid` / `effective_gid`（**Linux 文件权限检查基于 effective uid，非 real uid**）
- 按 owner → group → other 三级判断：
  - `effective_uid == st_uid` → 检查 `S_IWUSR` / `S_IXUSR`
  - `effective_gid == st_gid` → 检查 `S_IWGRP` / `S_IXGRP`
  - 否则 → 检查 `S_IWOTH` / `S_IXOTH`
- 任一项不满足 → 返回 `EACCES`
- `effective_uid == 0`（root）→ 绕过检查

### 2. `open`：已有文件写模式检查写权限

在 `open()` 函数的 "inode 已存在" 分支，若 `writable` 为 true：
- 同样按 owner/group/other 三级判断文件的写权限位
- 不满足 → 返回 `EACCES`

### 3. `sys_fchmodat`：启用调试日志

取消注释 debug 输出，便于排查 fchmodat 实际设置的 mode。

### 4. 调试日志

在 `create_file` 和 `open`（已有文件分支）添加详细 debug 日志：
- `uid/euid/gid/egid` — 进程凭证
- `parent_mode` / `file_mode` — 文件权限位
- `owner_uid/owner_gid` — 文件 owner
- `has_write/has_exec` — 权限判断结果

## 修改文件

| 文件 | 修改内容 |
|------|----------|
| `os/src/fs/kernel_fs_ops/open.rs` | 新增 `create_file` 父目录权限检查、新增 `open` 已有文件写权限检查、添加调试日志 |
| `os/src/syscall/fs/ctl.rs` | 取消注释 `sys_fchmodat` 调试日志、添加 `FaccessatFileMode` import |

## 调试过程

定位 effective_uid 问题的关键日志：

```
[create_file] uid=0 euid=1 gid=0 egid=0 parent_mode=700
[create_file] root user, bypass permission check   ← BUG: 用了 real uid(=0)
```

修复后 `user_id` → `effective_uid`：
```
[create_file] uid=0 euid=1 gid=0 egid=0 parent_mode=700
[create_file] EACCES: no write permission on parent  ← 正确：effective uid=1，属于 other，S_IWOTH 未置位
```

## 验证

RISC-V `make run`，运行 `creat04` LTP 测例。

## 后续

- `sys_fchownat` 仍为 stub（需在 Inode trait 增加 `fowner_set` 接口 + ext4_lw 实现）
- `sys_fchmodat` 的 debug 日志可重新注释
- 最终可移除调试日志，保留权限检查代码
