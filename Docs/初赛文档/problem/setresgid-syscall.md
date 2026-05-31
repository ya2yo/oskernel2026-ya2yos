# setresgid (149) 系统调用实现

## 背景

LTP `setresgid01` 等测例依赖 `setresgid(2)` / `getresgid(2)`。内核此前仅在 `Syscall` 枚举中声明了 `SetResgid = 149`，未注册路由，也未维护进程的 real/effective/saved GID。

## 实现要点

### 1. 进程凭证

在 `TaskControlBlockInner` 增加：

- `real_gid`
- `effective_gid`
- `saved_gid`

初始化为 0（root），`clone`/`fork` 时从父任务复制。`getgid`/`getegid`/`getgroups` 改为读取 `effective_gid`，不再硬编码 0。

### 2. setresgid 语义

按 Linux man page 实现：

- 参数 `(gid_t)-1`（`u32::MAX`）表示不修改对应 ID
- 设置 `rgid` 且 `egid`/`sgid` 为 -1 时，三者同步为新 rgid
- 设置 `egid` 且 `sgid` 为 -1 时，`egid` 与 `sgid` 同步
- 仅设置 `sgid` 时只改 saved gid
- 非特权进程：新 GID 须属于 `{real, effective, saved}` 且显式参数不超过 1 个；否则 `EPERM`
- 非法 GID（>65535 且非 -1）返回 `EINVAL`

### 3. getresgid

syscall 150（修正原错误编号 148）：通过 `put_data` 写回三个 GID，允许 NULL 指针跳过。

### 4. 枚举修正

- `GetResuid = 148`（新增路由，读 `user_id`）
- `GetResgid = 150`（原误为 148，与 getresuid 冲突）

## 验证

RISC-V `make run`，单跑 `setresgid01`：5 项 TPASS。

## 后续

- `setresuid(147)` 仍为 stub，若 LTP 需要可对称实现 UID 三元组
- `setresgid04` 要求新建文件 `st_gid` 跟随 effective GID，需在 ext4 创建路径调用 `ext4_owner_set`
