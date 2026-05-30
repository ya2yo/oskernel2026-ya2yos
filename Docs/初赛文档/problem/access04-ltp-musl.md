# LTP access04（LoongArch / musl）修复

## 背景

LTP `access04`（filelist 第 10 项，index 9）测试 `access()` / `faccessat()` 在多种 errno 路径下的行为，包括挂载只读文件系统后访问文件应返回 `EROFS`。

**实际测试顺序**：先在 **RISC-V + `sdcard-rv.img`** 上跑通 LTP 前 10 项（`abort01` … `access04`），再切换到 **LoongArch + `sdcard-la.img`** 跑同一索引。RV 侧已全部通过后，LA 上 `access04` 仍失败，才引出本文补丁。

## 与 RISC-V 的差异：为何 RV 前 10 项已过，LA 还要改

### 先澄清常见误解

| 误解 | 实际情况 |
| ------ | ---------- |
| 「RV 几乎不用改内核就能过 access04」 | RV 能过，是在 **syscall/fs 已修到可支撑 RV 用户态路径** 之后；不是裸内核在双架构上行为一致、仅 LA 缺功能。 |
| 「两架构跑同一份 access04 二进制」 | **不是。** 两镜像中是 **各自 ISA 编译** 的可执行文件，体积与 MD5 均不同（见下表）。 |
| 「RV 镜像 ext4 里自带 `/dev/loop*`」 | **不是。** `debugfs` 查看两镜像，根文件系统上均无 `/dev/loop*`、`/dev/loop-control`；loop 只能由内核 devfs 提供。 |

对 2026 预测试镜像中用户态文件的核对：

| 文件 | RISC-V (`sdcard-rv.img`) | LoongArch (`sdcard-la.img`) |
| ------ | -------------------------- | ----------------------------- |
| `/musl/ltp/testcases/bin/access04` | ELF RISC-V，797216 B，`77c84c8b…` | ELF LoongArch，859872 B，`366e9b7a…` |
| `/musl/lib/libc.so` | `28971771…` | `58432c90…`（不同 musl 构建） |
| ext4 上 `/dev/loop*` | 不存在 | 不存在 |

**结论**：不是「同一测例、同一 libc、只是 CPU 不同」，而是 **两套用户态 + 两套页表（`arch/`）+ 一套共享 fs/syscall** 叠在一起。RV 前 10 项已通过，只说明 **RV 用户态路径与当时内核的交集已对齐**，不能推出 LA 无需额外工作。

### access04 在内核里会走什么

LTP 框架 `tst_test.c` 挂载只读测试区的逻辑（语义相同，**代码在各架构二进制里分别编译**）：

1. **主路径**：在 `mntpoint` 上 `mount(..., "tmpfs", MS_RDONLY)`
2. **备用路径**（主路径失败）：写 `test_dev.img` → **`/dev/loop-control` + `/dev/loopN` + ioctl** 挂 ext2，再测 `EROFS` 等

loop 子系统、真实 `ioctl` **两架构原本都没有**；是否在评测中暴露，取决于 **主路径 mount 是否成功**。

### 观测到的执行路径差异（log.ans）

| 阶段 | RISC-V（前 10 项已通过） | LoongArch（同索引失败） |
| ------ | ------------------------- | ------------------------- |
| tmpfs mount | 成功，直接 12× TPASS | `Can't mount (null) at mntpoint (tmpfs): EFAULT` |
| loop fallback | **未进入** | `falling back to block device` → `/dev/loop-control` |
| 结果 | 通过 | `TBROK`（直至补 NULL special + loop 子系统） |

LA log 打印 **`mount (null)`**：LA 版 access04/musl 对 tmpfs 传 **`special == NULL`**。内核 `copy_from_user` 对地址 0 报 `EFAULT`（`checked_user_range`：`start == 0` → EFAULT），主路径失败，**被迫** 走 RV 上未触发的 loop 分支。

RV 能通过 access04，是因为 **RV 版 access04 + musl 在主路径 mount 成功**，从未执行 loop fallback——不是 RV 内核「自带 loop」，而是 **执行路径没踩到 loop 缺口**。

### 三类差异（由浅入深）

**① 用户态 / 镜像层（决定性）**

- 两镜像中 access04、libc **不是同一文件**（已用 MD5 核实）。
- LA 版触发 `mount(NULL, dir, "tmpfs", …)` → 需内核接受 NULL `special`（Linux 语义：空设备名）。
- LA 版 tmpfs 失败后走 **loop-control** 分配流程；RV 版在本项目 RV 验收中 **未走到该分支**。
- 共享 `mount.rs` 在 RV 上「够用」、在 LA 上仍缺 NULL 语义 + loop：**差异首先在用户态走哪条分支**，不是 ISA 本身。

**② 共享内核 fs/syscall 缺口（RV 未暴露、LA 暴露）**

| 缺口 | RV 前 10 项 | LA access04 |
|------|-------------|-------------|
| `sys_mount` + `translated_str` 缺页 panic | access04 会 mount，需 `copy_from_user` | 同样需要 |
| `special == NULL` → 空串 | **未触发** | **触发** |
| loop-control / DevLoop / loop ioctl | **未触发** | tmpfs 失败后 **必需** |
| `sys_ioctl` 从 stub 改为分发 | 未触发 | fallback **必需** |

loop/ioctl 是 **全架构通用的 fs 补全**；RV 阶段 **现有 access04 二进制未强迫内核走 loop 路径**，故在「RV 前 10 项已通过」时 **不会表现为 RV 失败**。

**③ 架构专属：`arch/*/page_table.rs` 的 `handle_mprotect`**

`access04` 使用 guarded buffer（`mmap` + `mprotect`）。两架构 **syscall 相同，页表实现不同**：

- **RISC-V**（`riscv64/qemu/page_table.rs`）：`find_pte_create().unwrap()` + `RVPTEFlags::from_bits_truncate`——旧实现，**本测例在 RV 上未失败**。
- **LoongArch**（`loongarch64/qemu/page_table.rs`）：旧代码把 `MapPermission` **错映射** 到 `LAPTEFlags`（R/W/X 与 MAT/PLV/DIRTY 位布局不同），且对懒分配页硬建 PTE；access04 在 LA 上易出问题，故 **仅在 LA 页表侧** 改为 `find_valid_pte` + `LAPTEFlags::from()`。

VirtIO MMIO（RV）vs PCI（LA）、LA 网络为空等，见 dual-arch skill；**与 access04 无关**。

### 汇总

```text
RV 前 10 项已通过
  └─ sdcard-rv.img 上 RV 版 access04 + musl
       └─ tmpfs 主路径成功 → 不碰 loop → 共享 loop 缺口不暴露
       └─ mprotect 走 RV 页表旧实现 → 本测例未失败

LA 同索引失败
  └─ sdcard-la.img 上 LA 版 access04 + musl（不同二进制）
       └─ mount(NULL) → EFAULT → 需 NULL 语义
       └─ fallback → 需 loop-control / ioctl → 需共享 fs 补全
       └─ guarded buffer → LA 页表 mprotect 需 arch 侧修复
```

**一句话**：LA 镜像上的 access04 **会走更严的 mount 与 loop 分支**，同时 **LA 页表 mprotect 有独立 bug**；RV 前 10 项通过 **不能** 说明 LA 只需跟 RV 做同样最少的改动。

## 现象（多轮 log.ans）

### 第一轮：内核 panic

```bash
[syscall ret --- OK] Mkdirat ret = 0
panic
[kernel] Panicked at src/mm/translate.rs:118 called `Option::unwrap()` on a `None` value
```

`mkdirat(mntpoint)` 后调用 `mount()`，`sys_mount` 用 `translated_str` 读用户态路径，缺页时 `.unwrap()` panic。

### 第二轮：TBROK，loop 设备缺失

```bash
Summary: passed 0, failed 0, broken 1
FAIL LTP CASE access04 : 512
```

扫描 `/dev/loopN`、`/dev/block/loopN` 等路径全部 `ENOENT`，`tst_device.c:354 TBROK: Failed to acquire device`。

### 第三轮：tmpfs mount EFAULT + loop-control 缺失

```bash
tst_test.c:1003: Can't mount (null) at mntpoint (tmpfs): EFAULT (14)
tst_test.c:1303: Can't mount tmpfs read-only, falling back to block device...
Unexpected error ... /dev/loop-control ... ENOENT
TBROK: Failed to acquire device
```

tmpfs 挂载时 `special` 为 **NULL**，`copy_from_user` 对空指针返回 `EFAULT`；备用 loop 路径缺少 `/dev/loop-control`。

## 分析

| 问题 | 根因 |
|------|------|
| mount panic | `translated_str` 不支持 lazy 缺页，与已修复的 `sys_statx` 同类 |
| tmpfs EFAULT | Linux 允许 `mount(NULL, dir, "tmpfs", ...)`，`special` 应为空串而非 EFAULT |
| loop TBROK | 无 loop 块设备节点；LTP 通过 `/dev/loop-control` + `LOOP_CTL_GET_FREE` 分配 |
| LA mprotect | `handle_mprotect` 对未映射页 `find_pte_create` 误建 PTE；`from_bits_truncate` 与 `LAPTEFlags` 语义不一致 |

## 修复

### 1. sys_mount / sys_umount2 用户指针

`os/src/syscall/fs/mount.rs`：`translated_str` → `read_user_cstr()`（内部 `copy_from_user`）；`NULL` 指针返回空字符串。

### 2. loop 设备子系统

新建 `os/src/fs/files/loopdev.rs`：

- `DevLoop`：解析 `/dev/loopN`、`/dev/loop/N`、`/dev/block/loopN`
- `DevLoopControl`：`/dev/loop-control`，实现 `LOOP_CTL_GET_FREE` / `LOOP_CTL_ADD` / `LOOP_CTL_REMOVE`
- loop 设备 ioctl：`LOOP_GET_STATUS64`（空闲返回 `ENXIO`）、`LOOP_SET_FD`、`LOOP_CLR_FD`、`LOOP_SET_STATUS64`

`os/src/fs/files/devfs.rs`：`find_device` / `open_device_file` 识别 loop 路径。

`os/src/fs/vfs.rs`：`File` trait 增加默认 `ioctl()` → `ENOTTY`，loop 设备覆写。

`os/src/syscall/fs/ctl.rs`：`sys_ioctl` 分发到 `File::ioctl`；`LOOP_SET_FD` 前校验 backing fd。

`os/src/fs/kernel_fs_ops/initfiles.rs`：创建 `/dev/block`、`/dev/loop`、`/dev/shm` 目录。

### 3. LoongArch handle_mprotect

`os/src/arch/loongarch64/qemu/page_table.rs`：仅对已映射有效页修改权限；用 `LAPTEFlags::from(add_flags)` 替代 `from_bits_truncate`。

## 涉及文件

| 文件 | 改动 |
|------|------|
| `os/src/syscall/fs/mount.rs` | `copy_from_user` + NULL special |
| `os/src/fs/files/loopdev.rs` | 新建 loop / loop-control 设备 |
| `os/src/fs/files/devfs.rs` | 打开 loop 设备 |
| `os/src/fs/files/mod.rs` | 注册 `loopdev` 模块 |
| `os/src/fs/vfs.rs` | `File::ioctl` 默认实现 |
| `os/src/syscall/fs/ctl.rs` | 真实 `sys_ioctl` |
| `os/src/fs/kernel_fs_ops/initfiles.rs` | dev 目录初始化 |
| `os/src/arch/loongarch64/qemu/page_table.rs` | `handle_mprotect` |

## 验证

- **RISC-V**：`sdcard-rv.img`，LTP 前 10 项（含 `access04`）已通过（验收阶段）。
- **LoongArch**：补全本文补丁后，单跑 `access04`（`LTP_TEST_START=9`）12/12 TPASS，退出码 0，无 panic。

建议在 LA 侧改完 loop / NULL special 后，**用 RV 再跑一遍 access04**，确认共享 fs 改动未回归 RV 主路径（RV 仍应不触发 loop fallback）。
