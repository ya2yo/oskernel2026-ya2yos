# LoongArch busybox-glibc mprotect 越界改权限

## 背景

LoongArch 当前测试入口配置为运行 glibc 目录下的 `busybox_testcode.sh`。该脚本先用 busybox shell 启动，再按 `busybox_cmd.txt` 逐条执行 busybox 命令。

## 现象

`log.ans` 中 busybox-glibc 进入 GROUP START 后，在 shell fork 管道子进程附近很快打印 glibc malloc 断言：

```text
Fatal glibc error: malloc.c:2589 (sysmalloc): assertion failed
```

旧日志中 fatal 后仍有 syscall ret，但脚本无法继续跑到 `#### OS COMP TEST GROUP END busybox-glibc ####`。

## 分析

日志显示 busybox/glibc 初始化阶段先多次 `brk` 扩展堆，再调用 `mprotect`，随后在子进程执行 `writev` 时触发 malloc 的 `sysmalloc` 断言。排查 COW、`brk` 返回值和 fork 后堆页复制后，发现真正的问题在 `MemorySetInner::mprotect()` 的页表权限更新循环。

`sys_mprotect()` 计算出的 `end_vpn` 与 `VPNRange` 的语义一致，都是右开区间 `[start_vpn, end_vpn)`。`mprotect()` 拆分 `MapArea` 时也按右开区间处理，但最后实际更新 PTE 权限时使用了 `start_vpn.0..=end_vpn.0`。

这会把请求范围后一页也传给 `handle_mprotect()`。如果后一页已经映射，就会被错误改成同一组权限，破坏相邻 VMA 或堆页的真实访问属性。

## 根因

`mprotect` 的 VMA 区间语义和 PTE 更新区间语义不一致：

- VMA 拆分和 `VPNRange` 使用 `[start, end)`；
- PTE 更新误用 `..=end`，额外处理了 `end_vpn` 对应页。

## 修复

在 `os/src/mm/memory_set/mmap_ops.rs` 中，将页表权限更新循环改为右开区间：

```rust
for vpn in start_vpn.0..end_vpn.0 {
    self.page_table.handle_mprotect(vpn.into(), map_perm);
}
```

这样只修改用户请求的页，避免污染相邻页权限。

## 涉及文件

| 文件 | 修改 |
|------|------|
| `os/src/mm/memory_set/mmap_ops.rs` | 修正 `mprotect()` PTE 权限更新范围 |
| `Docs/初赛文档/开发日志.md` | 记录本次修复 |
| `Docs/初赛文档/problem/README.md` | 更新问题索引 |
| `Docs/初赛文档/ai.log` / `Docs/初赛文档/AI_INTERACTION.md` | 记录 AI 辅助分析与验证 |

## 验证

已执行：

```text
make log
timeout 120s make run
```

结果：

```text
#### OS COMP TEST GROUP START busybox-glibc ####
...
#### OS COMP TEST GROUP END busybox-glibc ####
shutdown!
```

重新扫描 `log.ans`，未再出现 `Fatal glibc`、`malloc.c`、`panic`、`TFAIL`、`TBROK`。日志中仍有 `testcase busybox hwclock fail`，这是 busybox 命令兼容性输出，和本次 malloc abort 不同。
