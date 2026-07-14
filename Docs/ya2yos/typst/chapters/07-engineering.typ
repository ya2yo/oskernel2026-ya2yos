= 构建、验证与维护约定

== 构建入口

仓库根目录 Makefile 负责选择架构、准备 Cargo 配置和启动 QEMU。架构敏感的验证应显式指定目标：

```sh
make TARGET_ARCH=riscv64
make TARGET_ARCH=loongarch64
make log TARGET_ARCH=loongarch64
make run TARGET_ARCH=loongarch64
```

`make run` 会创建临时 `disk.img` 链接并在退出后清理。日志型验证关注 `TPASS`、`TFAIL`、`TBROK`、panic 和测试 summary，而不是仅凭测试包装器的退出行判断。

== 推荐验证矩阵

#table(
  columns: (1.3fr, 1fr, 1.7fr),
  table.header([*变更范围*], [*最低检查*], [*进一步检查*]),
  [仅 Typst/文档], [`typst compile`], [链接、目录、PDF 视觉检查],
  [架构无关内核逻辑], [默认 `make`], [两架构构建、对应 `make run` 回归],
  [内存/信号/调度/VFS/网络], [两架构 `make`], [目标测例 `make log` 或 QEMU 运行],
  [驱动或内存布局], [对应架构构建], [QEMU 启动、设备探测和真实 I/O 路径],
)

== 并发与错误处理

内核高风险路径的共同原则如下：

- 获取资源 `Arc` 后尽快释放外层 slot/table 锁；禁止持锁访问用户内存、文件系统、网络、信号投递或调度。
- 多进程/多线程同时加锁时按 pid/tid 稳定排序，优先复制标量或克隆 `Arc` 而非长时间双持锁。
- 用户指针均经 `copy_from_user` / `copy_to_user`；先检查空指针、长度和溢出。
- 用 `Result`/errno 保留失败原因。只有 Linux 语义确实要求时才能把内部错误转换为成功或短读写。
- 新增行为应在 `Docs/决赛文档/problem/` 记录非平凡问题的背景、根因、修复和验证；AI 协助的实质性修改同步更新 AI 记录。

== 文档工程

本目录中的 `main.typ` 是唯一的 PDF 入口，章节位于 `chapters/`。生成物 `ya2yos-kernel-design.pdf` 被忽略，不提交二进制；从本目录执行：

```sh
typst compile main.typ ya2yos-kernel-design.pdf
```

章节文件只描述稳定的设计与当前代码边界。具体测试故障、日志和临时诊断归入 `Docs/决赛文档/problem/`，避免设计文档被一次性调试细节淹没。
