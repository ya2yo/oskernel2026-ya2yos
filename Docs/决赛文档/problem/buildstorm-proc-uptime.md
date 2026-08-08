# BuildStorm P0-A 动态 `/proc/uptime` 计时

## 背景

BuildStorm 的计时脚本在正式编译前后读取 `/proc/uptime`，用第一列计算
`elapsed_s`。Ya2yOS 原来只在启动时把固定内容写入若干 proc 兼容文件，没有提供
`/proc/uptime`，因此脚本的时间变量为空，最终成绩中的 `elapsed_s=0.00` 不能作为
性能基线。

## 现象

`BUILDSTORM_COMPILE` 可以正常生成产物，但正式计时字段为 `0.00`。这会让评测器把
功能成功误认为性能计时成功，也无法将 guest 时间与 `[perf] t=...` 快照对齐。

## 根因

当前 proc 采用路径化模型，`/proc/mounts`、`/proc/cpuinfo` 和 `/proc/meminfo` 等
文件由 `create_init_files()` 写入 rootfs。`/proc/uptime` 没有动态文件对象，打开
路径时只能落到普通 ext4 查找，因而返回 `ENOENT`。

## 修复

- 新增只读 `UptimeFile`，在公共 VFS `open()` 处理 `/proc/uptime` 时直接分发，不在
  ext4 镜像中创建静态伪文件；`stat`/`faccessat` 等通过公共入口复用同一动态节点。
- 第一列使用架构层 `get_ticks()/get_clock_freq()` 转换为秒和两位厘秒，第二列统计
  所有 Hart 的 idle 时间；RISC-V 和 LoongArch64 都复用各自的架构时钟 API。
- 每个打开的文件描述符保存一次内容快照，连续小读、跨页读和 EOF 都按同一内容与
  偏移处理；`lseek(SEEK_SET, 0)` 重新采样，`fstat()` 暴露当前动态内容长度。
- 以只读 proc 节点的方式处理打开标志：写打开返回 `EACCES`，`O_DIRECTORY` 返回
  `ENOTDIR`，`O_CREAT|O_EXCL` 返回 `EEXIST`。
- 增加 initproc 回归：逐字节读取两次，检查 `"uptime idle\n"` 格式、EOF 和第二次
  uptime 不小于第一次。

## 验证

已执行：

```text
make TARGET_ARCH=riscv64
make TARGET_ARCH=loongarch64
timeout 90s qemu-system-riscv64 ... -smp 8 ... -snapshot
git diff --check
```

两种架构 release 构建通过。RISC-V QEMU 启动日志中 `fstat unlink regression`、
`sigaltstack regression`、`rseq regression` 和 `uptime regression` 均为 `PASS`；随后
进入嵌套 QEMU 启动阶段，外层命令因 90 秒 timeout 结束，未完成完整 BuildStorm。
LoongArch64 本轮完成构建验证，未运行完整 QEMU 回归。
