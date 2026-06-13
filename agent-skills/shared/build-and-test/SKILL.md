---
name: build-and-test
description: >-
  编译、运行 QEMU 和分析 log.ans。内核修改后的验证步骤，见 kernel-change。
  用于 make/run、切换架构、GDB、解读测试输出时。
---

# 编译与测试运行

> 内核改动后的验证环节，见 [kernel-change](../kernel-change/SKILL.md)。测例通过后补文档见 [doc-writing](../doc-writing/SKILL.md)。

## 环境

推荐 Docker 镜像：`docker.educg.net/cg/os-contest:20250714`

工具链：`rust-toolchain.toml` → nightly，targets `riscv64gc-unknown-none-elf` / `loongarch64-unknown-none-softfloat`

## 常用命令

```bash
# 项目根目录
make                          # 默认 riscv64，warn 级别日志
make log                      # 启用 debug! 日志
make TARGET_ARCH=loongarch64  # 龙芯
make run                      # QEMU 运行（会临时 symlink disk.img）
make clean
make gdbserver                # QEMU -s -S
make gdbclient                # gdb-multiarch 附加
```

## 关键配置

| 配置项 | 位置 | 说明 |
|--------|------|------|
| 架构 | `Makefile` 顶部 `TARGET_ARCH` | `riscv64` / `loongarch64` |
| 磁盘镜像 | `make_scripts/riscv64.mk` 或 `loongarch64.mk` 的 `DISK_IMG` | 默认 `2026_testsuits_img/pre_tests/sdcard-*.img` |
| 日志级别 | `make` vs `make log` | 控制 `debug!` 是否输出 |
| 测试入口 | `user/src/bin/initproc.rs` | `get_score()` / `test_ltp()` |
| rust-analyzer | `os/Cargo.toml` default features | 编辑时可设 arch feature；**编译时勿与 Makefile 冲突** |

## QEMU 参数要点（RISC-V）

- `-machine virt`，128M RAM，virtio-blk + virtio-net（user netdev）
- `-snapshot`：不持久化磁盘修改
- 内核产物：`kernel-rv`（RISC-V）/ `kernel-la`（LoongArch）

## 选择运行哪些测试

编辑 `user/src/bin/initproc.rs`：

```rust
// 单测 busybox
run_testsuit("musl\0", "busybox_testcode.sh\0");

// LTP 分批（当前常用）
const LTP_TEST_START: usize = 3;      // filelist 起始索引
const LTP_TESTS_PER_GROUP: usize = 1; // 每批数量
test_ltp();
```

测试列表：`user/src/bin/ltp/filelist.rs`（约 2800+ 项）

## 解读测试输出

```
RUN LTP CASE accept02
TPASS: ...
FAIL LTP CASE accept02 : 0    ← 注意：不是失败！冒号后是退出码，0=成功
#### OS COMP TEST GROUP END ...
```

**判据**：看 LTP 内部的 `TPASS`/`TFAIL`/`Summary`，以及退出码；不要仅凭 `FAIL LTP CASE` 字样判断。

## log.ans 分析

日志可能含二进制字节，用文本模式搜索：

```bash
rg -a -n "TPASS|TFAIL|panic|ERROR|WARN" log.ans
strings log.ans | tail -80
```

关注顺序：panic 栈 → syscall 序列 → 测试 Summary → 末尾 cleanup 错误（常为无害噪音）。

## GDB

```bash
make gdbserver   # 终端 1
make gdbclient   # 终端 2，已配置符号与架构
```

断点常用：`sys_*`、`trap_handler`、`page_table` 相关函数。

## 相关技能

- 总流程 → [kernel-change](../kernel-change/SKILL.md)
- 文档 → [doc-writing](../doc-writing/SKILL.md)
- LTP 排查 → [ltp-test-triage](../ltp-test-triage/SKILL.md)
- 网络问题 → [network-debug](../network-debug/SKILL.md)
- 已知 bug 模式 → [debug-playbook](../debug-playbook/SKILL.md)
