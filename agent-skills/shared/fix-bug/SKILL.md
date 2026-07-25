---
name: fix-bug
description: >-
  Ya2yOS bug 修复流程。用于排查 panic、LTP/TFAIL、死循环、阻塞、语义错误、
  网络/文件/任务/内存问题、跨架构失败、LTP/回归程序/BuildStorm 日志分析，
  并要求修复后验证和写文档时。
---

# 修复 Bug

目标：先复现和缩小范围，再最小改动修复；不要把调试噪音或无关重构混进修复。

## 排查顺序

1. 明确失败信号：panic、`TFAIL`、卡死、退出码、日志关键字或用户描述。
2. 先识别测试族和边界，再解释日志；不要把 LTP 专用标记当作所有测例的统一判据。
3. 找测试源或同类实现，确认期望语义。
4. 从 syscall 入口追到领域模块：`syscall/* -> fs/net/task/mm/arch/drivers`。
5. 加临时日志时保持可删除；修复前后都要能解释日志变化。

## 日志判读

`log.ans` 可能含 NUL 字节和 ANSI 控制序列。先用 `rg -a` 定位原始位置，必要时用
`strings` 查看连续的用户态输出：

```bash
rg -a -n -i 'panic|TPASS|TFAIL|TBROK|Summary|regression: (PASS|FAIL)|OS COMP TEST GROUP|buildstorm|fatal error|error|could not compile|linking with|exit status|QEMU: Terminated' log.ans
strings -n 4 log.ans | rg -n -i 'TPASS|TFAIL|TBROK|Summary|regression: (PASS|FAIL)|OS COMP TEST GROUP|buildstorm|fatal error|error|could not compile|linking with|exit status|QEMU: Terminated'
```

| 测试类型 | 识别和结论 |
|----------|------------|
| LTP | 仅在日志确实出现 `TPASS`、`TFAIL`、`TBROK` 或 `Summary` 时按 LTP 规则判读。`FAIL LTP CASE x : 0` 只是 initproc 打印的退出码，不单独作为失败结论。 |
| 内置回归程序 | 以 `<name> regression: PASS` / `FAIL` 为单项结果。当前 `sigaltstack regression: PASS`、`rseq regression: PASS` 就属于此类；没有 LTP 汇总是正常的。收集全部回归项，并结合下一个测试组开始标记、测试脚本或最终退出状态确认运行是否完整，不能因前几个 `PASS` 就宣布整轮成功。 |
| BuildStorm/编译类测例 | 用 `OS COMP TEST GROUP START buildstorm-...` 和实际执行脚本识别组边界。优先保留第一个编译器/链接器诊断及其上下文，例如 `fatal error`、`linking with ... failed`、`could not compile`、`exit status`；当前日志的直接失败是 `cc: fatal error: '-fuse-linker-plugin', but liblto_plugin.so not found`。`[ERROR]`、`warning` 或 `QEMU: Terminated` 需要结合前文判断，后两者通常是伴随信息或运行结束现象，不可替代根因。 |

输出可能被多核并发日志穿插，例如测试组名称与目录输出相连。定位后读取前后文和实际执行命令，不能依赖一条被截断的行推断测试名称或错误原因。

## 常见方向

| 症状 | 优先检查 |
|------|----------|
| 用户指针异常、随机 fault | 是否用了 `copy_from_user` / `copy_to_user` |
| syscall 返回成功但测试期望失败 | 内部 `Result` 是否被吞掉，是否缺少 `?` |
| accept/connect/pipe 卡住 | 阻塞路径是否能 wake，是否持有多余 `Arc` 或锁 |
| 死锁、偶现卡住、`try_lock().expect()` panic | 是否违反 `os/src/task/mod.rs` 开头的 task/PCB 锁顺序 |
| futex / signal 相关 panic | wait 被信号打断时是否清理等待队列 |
| COW / page fault | 两架构 PTE flags、TLB 刷新、写时复制分裂 |
| LTP 输出迷惑 | 先确认这是 LTP；再以 `TPASS`/`TFAIL`/`TBROK`/`Summary` 为准，`FAIL LTP CASE x : 0` 只是 initproc 打印退出码 |
| 非 LTP 回归程序没有 Summary | 查找 `<name> regression: PASS/FAIL`、测试组边界和脚本的完成标记；不能只筛 `TFAIL`/`TBROK` |
| BuildStorm 在内核中编译失败 | 先看最早的编译器/链接器 `error` 上下文和执行命令；把 `QEMU: Terminated` 视为结束现象，继续向前找可操作的诊断 |
| 网络组播/setsockopt | `syscall/net/opt.rs` 参数解析 + `net/tcp.rs`/`udp.rs` socket 状态 |
| ext4 `/tmp` cleanup ENOENT | 多数是 cleanup 噪音；先确认测试断言是否已 TPASS |

## 修改原则

- 只改与根因相关的代码。
- 保持模块边界：syscall 层薄，语义在领域模块。
- 严格遵守 `os/src/task/mod.rs` 开头记录的锁顺序；不按顺序获取锁的代码直接判定为 bug，即使当前日志还没有稳定复现死锁。
- `ResourceSlot` 只保护可替换 `Arc<T>` 指针槽。只能短暂 `get` / `replace`，不能在槽锁内进入资源内部锁、用户内存访问、文件系统、网络、调度、futex 或信号发送路径。
- 排查死锁或卡住时，优先画出实际锁链；如果出现反向锁顺序，修正锁边界或先 clone/copy 所需状态再释放锁，不要只延长 timeout 或绕过 `try_lock`。
- 不用 `unwrap()` 处理用户输入或可失败内核路径。
- 不在修 bug 时顺手重排大文件、改格式、改测试策略，除非这是修复必要条件。

## 验证

最小验证：

```bash
make
```

复现类 bug 要跑触发路径：

```bash
make log
make run
```

长日志用：

```bash
rg -a -n -i 'panic|TPASS|TFAIL|TBROK|Summary|regression: (PASS|FAIL)|OS COMP TEST GROUP|buildstorm|fatal error|error|could not compile|linking with|exit status|QEMU: Terminated' log.ans
strings -n 4 log.ans | rg -n -i 'TPASS|TFAIL|TBROK|Summary|regression: (PASS|FAIL)|OS COMP TEST GROUP|buildstorm|fatal error|error|could not compile|linking with|exit status|QEMU: Terminated'
strings log.ans | tail -80
```

若只在某架构失败，分别运行：

```bash
make TARGET_ARCH=riscv64
make TARGET_ARCH=loongarch64
```

## 收尾

- 修通测例、panic、语义 bug：使用 `write-docs` 写开发日志和 problem；用 AI 辅助时同时写 `ai.log` 与 `AI_INTERACTION.md`。
- 写完文档不需要再重新运行，直接回复说明根因、修改点和验证结果；没跑的验证要明确说。

## 提交建议

- 完成并验证 bug 修复后，在最终回复给出一条合理的建议 commit：使用与仓库一致的标题格式，并在需要时提供正文，覆盖根因、修复和验证结果。
- 列出建议暂存的精确文件范围；识别并排除维护者已有的无关改动，不能建议 `git add -A`。
- 不得仅凭本技能执行 `git add` 或 `git commit`。提交建议由维护者审阅、修改并决定是否采纳。
