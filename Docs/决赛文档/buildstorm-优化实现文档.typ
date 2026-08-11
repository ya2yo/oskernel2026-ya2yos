// BuildStorm 人工评审优化实现文档。
// 构建：typst compile Docs/决赛文档/buildstorm-优化实现文档.typ /tmp/buildstorm-优化实现文档.pdf

#let version = "1.0"
#let snapshot = "3e0e982f145885e821b45522c33178eeef9ac473 (2026-08-11)"
#let ink = rgb("161616")
#let muted = rgb("565656")
#let line = rgb("9b9b9b")
#let gray = rgb("f2f2f2")
#let body-font = ("Libertinus Serif", "WenQuanYi Zen Hei")
#let heading-font = ("WenQuanYi Zen Hei", "Libertinus Serif")
#let code-font = ("DejaVu Sans Mono", "WenQuanYi Zen Hei Mono")

#set document(
  title: "BuildStorm 测例内核设计与优化实现文档",
  author: "Ya2yOS",
  date: datetime(year: 2026, month: 8, day: 11),
  keywords: ("Ya2yOS", "BuildStorm", "Rust", "EXT4", "SMP", "performance"),
)
#set page(
  paper: "a4",
  margin: (top: 2.4cm, bottom: 2.25cm, x: 2.35cm),
  header: align(right, text(size: 8pt, fill: muted)[Ya2yOS BuildStorm Optimization · #version]),
  footer: context align(center, text(size: 8pt, fill: muted)[#counter(page).display("1")]),
  numbering: "1",
)
#set text(font: body-font, size: 10.2pt, fill: ink, lang: "zh")
#set par(justify: true, leading: 0.7em, first-line-indent: 2em, spacing: 0.58em)
#set heading(numbering: "1.1")
#show heading: set text(font: heading-font, weight: "bold")
#show heading.where(level: 1): set text(size: 18pt)
#show heading.where(level: 1): set block(above: 1.2em, below: 0.7em)
#show heading.where(level: 2): set text(size: 13.5pt)
#show heading.where(level: 2): set block(above: 1em, below: 0.42em)
#show heading.where(level: 3): set text(size: 11.3pt)
#show heading.where(level: 3): set block(above: 0.7em, below: 0.3em)
#show raw: set text(font: code-font, size: 8.5pt, lang: "en")
#show raw.where(block: true): it => block(fill: gray, inset: 6pt, radius: 2pt, above: 0.5em, below: 0.6em)[#it]
#set table(stroke: line, inset: 5pt)
#set list(indent: 1.35em, body-indent: 0.5em, spacing: 0.25em)

#let note(title, body) = block(
  inset: 8pt,
  fill: gray,
  stroke: (left: 1.4pt + ink),
  radius: 2pt,
  [*#title* #body],
)

#align(center)[
  #v(2.1cm)
  #text(font: heading-font, size: 24pt, weight: "bold")[BuildStorm 测例]
  #v(0.35cm)
  #text(font: heading-font, size: 20pt, weight: "bold")[内核设计与优化实现文档]
  #v(1.1cm)
  #text(size: 12pt)[面向 OS 竞赛人工评审]
  #v(2.0cm)
  #table(
    columns: (7em, 1fr),
    [文档版本], [#version],
    [适用源码快照], [#snapshot],
    [报告范围], [2026-07-12 至 2026-08-11 的 BuildStorm 跑通、稳定性修复和性能优化],
    [目标读者], [竞赛评审者、内核维护者和复现实验的开发者],
    [实现边界], [只描述已合入上述快照或可由其历史提交追溯的实现；计划和未完成长测不当作已实现结果],
  )
  #v(2.6cm)
  #text(size: 9.5pt, fill: muted)[Typst 源文件；不提交 PDF。构建命令见文末。]
]

#pagebreak()
#align(center)[#text(font: heading-font, size: 15pt, weight: "bold")[摘要]]

BuildStorm 在 guest 内检查 Rust 工具链，完成独立 `minibuild`，再以 `nproc` 决定并发度执行
Cargo/Rustc 的 ArceOS 编译。该工作负载同时放大了动态加载、`fork`/`vfork`/`execve`、高频
`mmap`/COW、跨 Hart TLB 失效、文件页缓存、lwext4 全局入口、稀疏临时文件写回和 journal
checkpoint 的缺陷。早期现象既有工具链或语义错误，也有无 panic 但两小时只推进到 `97/446`
的吞吐退化；因此不能把它当作单一文件系统热点处理。

本轮以“先修正确性和可观测性，再收敛可证明的高频工作”为原则。实现恢复了对用户可见的
SMP 拓扑和多 Hart 调度，缩小了文件读取、路径/descriptor、页缓存、EXT4 gate、稀疏写和
bcache 写回的重复工作；同时把 `MemorySet` 的全驻留帧保留改为范围化保留，将 remote-TLB
从逐目标串行等待改为广播后汇集 ACK，并避免独占 COW 被临时引用误判为共享复制。所有优化
保持 Linux 可见语义、lwext4 非 SMP-safe C API 的串行边界和“旧帧必须在远端 ACK 后释放”的
一致性不变量。

#note([实验结论边界], [已有同问题复盘记录的可比定向数据表明，BuildStorm 尾部 `axbuild`
单元从约 12 分钟降至约 8 分钟，时间缩短约 33.3%，加速比约 1.50x。该数据是关键编译
单元的定向观测，不是完整 446 crate 的最终成绩。当前根目录 `log.ans` 只到
`BUILDSTORM_BEGIN`，尚未出现官方 `BUILDSTORM_COMPILE ... ok=true elapsed_s=...` 完成行；
本报告不会虚构完整 A/B 数据，并给出评审者可直接执行的复现和计时步骤。])

#v(0.7em)
*关键词*：BuildStorm；Cargo；Rustc；SMP；remote TLB；EXT4；页缓存；bcache；性能诊断

#pagebreak()
#outline(title: [目录], depth: 2)
#pagebreak()

= 测例、范围与判定口径

== 官方工作负载

官方脚本为 `scripts/buildstorm_testcode.sh`，先检查 `rustc --version` 与 `cargo --version`，
随后在 `/tmp/minibuild` 创建并构建新 Cargo 项目，最后在 `/work/tgoskits` 清理目标目录并执行
`cargo xtask arceos build -p arceos-helloworld --arch "$AXARCH"`。脚本通过 guest 的
`/proc/uptime` 计时，输出内容和评审含义如下。

#table(
  columns: (10em, 1fr, 9em),
  table.header([*输出标记*], [*含义*], [*本报告的使用方式*]),
  [`BUILDSTORM_TOOLCHAIN ok`], [Rust 工具链及动态加载可用], [功能前置条件],
  [`BUILDSTORM_MINIBUILD ok`], [干净 Cargo 项目的创建、fork、exec、mmap 和链接可用], [功能前置条件],
  [`BUILDSTORM_BEGIN mode=multi`], [进入正式多核编译段], [不是完成或计时结果],
  [`BUILDSTORM_COMPILE ... ok=true elapsed_s=X`], [目标产物存在且正式编译成功；`X` 为 guest 编译时间], [唯一的端到端计时和成功判据],
  [`OS COMP TEST GROUP END buildstorm`], [脚本正常收尾], [完整运行的辅助证据],
)

该脚本的 `cores=$(nproc)` 直接受 `sched_getaffinity(2)` 影响；因此内核把可运行 CPU 错报为
单核会改变 Cargo 的 worker 数，而不是单纯损失一个微优化。对于长测，只有同一源码、同一
架构、同一镜像/overlay、相同 memory/SMP、相同冷/热缓存策略下的两个成功完成行，才可按
`speedup = elapsed_before / elapsed_after` 报告完整 BuildStorm 加速比。

== 本报告的来源和边界

代码范围是 `os/`、`user/`、`crates/lwext4_rust/` 及测试入口；证据来自提交历史、
`Docs/决赛文档/problem/` 中逐项复盘和保留的日志快照。下表列出本报告聚合的主要提交，
避免将个人推测写成结论。

#table(
  columns: (6.6em, 6.5em, 1fr),
  table.header([*阶段*], [*代表提交*], [*结果*]),
  [启动与运行时正确性], [`2ce578de`、`bfbfb2c4`、`7280f9c3`、`d50c274c`], [修复工具链路径、fresh fork/loader、长 argv、rename 发布等阻断项],
  [并行与调度], [`82e92a54`、`a29e6ef1`、`f4061a35`、`9cdf0475`], [恢复 SMP 可见拓扑，消除忙让出和重复全局维护，扩展 LoongArch worker 覆盖],
  [内存与同步], [`8e3acbd6`、`6be9add2`、`34fc0c3c`], [范围化帧保留、TLB 批处理/广播 ACK、独占 COW 本地提升],
  [文件系统和写回], [`6e5ae378`、`b4d7d37e`、`c3293124`、`3f66a326`], [文件页缓存复用、读锁域和混合 read 收敛、连续 bcache 写回合并],
  [可观测性], [`b93ad36a`、`01045712`、`3e9c16fb`、`e623e1be`], [perf 聚合计数、动态 `/proc/uptime`、默认关闭 C 侧 telemetry],
)

报告不把以下内容计为性能收益：没有 `ok=true` 的 timeout 日志、不同 Cargo 阶段的
`Building N/446`、跨 Hart 累计 wait/hold、或已撤回的实验。它们只用于定位。完整证据可在
`problem/buildstorm-*.md`、`problem/lwext4-journal-full-buildstorm.md` 和 Git 记录中核查。

= 问题定位与根因分析

== 从“跑不通”到“跑得慢”的分层诊断

BuildStorm 的失败没有单一根因。先以官方标记把问题分成三层：工具链前停止、MINIBUILD
失败、正式编译无进展或异常；再通过 GDB、多 Hart 栈、`perf` 聚合计数和源码路径复核，
从用户态 Cargo/Rustc 逐层追到 syscall、VFS/MM/task 及架构 IPI。核心调用关系为：

```text
Cargo/Rustc workers
  -> fork/vfork/execve, mmap/mprotect/munmap, futex/ppoll, open/read/write/rename
  -> syscall/{task,mm,fs} -> task scheduler / MemorySet / VFS
  -> page cache / EXT4 wrapper / lwext4 bcache / virtio block
  -> remote TLB mailbox, per-hart IPI and architecture page table helpers
```

== 已证实的瓶颈和语义阻断

#table(
  columns: (8.3em, 1fr, 1fr),
  table.header([*现象和证据*], [*根因*], [*为什么会影响 BuildStorm*]),
  [Cargo 并发退化；`nproc` 仅见 home hart], [`sched_getaffinity` 把内核内部 placement 错暴露为 Linux CPU mask], [Cargo 按 `nproc` 设置 worker，八/十二核 guest 退化为近似单 worker],
  [两小时仍约 `97/446`，宿主 60 s 样本 CPU 约 124%，`strace` 中 futex 79.46%], [高频阻塞/唤醒、读路径的重复 EXT4 入口和调度循环叠加；不能仅凭 `pread64` 占比判为磁盘带宽], [Rustc worker 的短任务、管道和文件访问放大调度和锁队列],
  [约 216 s 的中途样本：EXT4 read/write/find/fstat 累计 wait 约 496.947 s、hold 约 89.987 s], [lwext4 C API 和 bcache 不是 SMP-safe，唯一 gate 下有重复 path/descriptor/size/cache 探测], [多 worker 同时读取 crate、写入对象和查询 metadata，在安全串行边界前排队],
  [`axbuild` 尾部单元约 12 分钟；GDB 反复在 `BTreeMap::values` 与 `Vec<Arc<FrameTracker>>::collect`], [每次局部 brk/munmap/mremap 都克隆地址空间全部驻留 frame，引用计数和遍历与变更范围无关], [Rustc 的大驻留集加高频内存调整形成近似卡死],
  [RISC-V 样本 remote shootdown 1,153,785 次，其中 remote 788,891；COW 1,050,895], [发送者对每个目标“发 IPI 后立即等 ACK”；独占 COW 在临时 Arc 保活后被误判为共享], [同一地址空间跨 Hart 时，重复页复制和串行 ACK 把本可重叠的等待放到编译临界路径],
  [约 10 min 样本有 146,045 次 block 写、856,701,952 B，平均单写约 6 KiB], [bcache flush 对连续 dirty 块固定以单 block 提交], [Rustc 临时文件、artifact rename 和 checkpoint 产生小而相邻的写回请求],
)

其中 EXT4 的 wait/hold、mailbox wait 和 device service 是所有 Hart 的累计时间，不能相加成
wall-clock；它们用于比较同配置、同阶段的结构性变化。编译快慢最终仍以脚本的
`elapsed_s` 为准。

== 先修复正确性，避免将故障伪装成性能

性能优化之前完成了会使工作负载中断、污染数据或死锁的修复：Rustc 长 argv 不再受路径长度
截断；`MAP_STACK`、fresh fork TrapContext、`mremap(MAYMOVE)`、VMA split 的 file offset 和
`MAP_FIXED` overlap 保持地址空间语义；动态解释器与 native/legacy library 路径不混用；
rename 发布、跨进程 unlink/fstat、epoll fd reuse、fd reservation、FIONBIO、shebang 与
稀疏文件行为按 Linux/工具链期望收敛。lwext4 journal-full、checkpoint callback、目录项
`rec_len == 0` 和 cache-flush 生命周期也先被修复，以免“进度停住”其实是断言自旋或文件系统
损坏。

这些改动不应与后续微优化混为一谈：它们的价值是让工具链、MINIBUILD 和正式编译可以进入
同一稳定路径，给可重复测速建立前提。

= 修复与优化设计及实现

== 并行度、调度与时间口径

*恢复用户可见的 SMP 拓扑。* `os/src/syscall/task/schedule.rs` 保留 pid/tid 校验，但
`sched_getaffinity` 返回在线 `HART_NUM` 的 mask；内核仍可使用内部 placement 管理线程。
独立地址空间的 Rustc 进程因此能被 Cargo 作为多核 worker 使用。共享 all-hart CFS queue
配合 affinity 过滤和 idle-hart 唤醒，避免 runnable task 只被 home hart 消费。

*减少无效调度维护。* `ppoll` 不再在无事件时忙让出而驱动 scheduler 热循环；全局
timer/futex/task timeout 维护由 CAS 选出一个 Hart 每 10 ms bucket 执行，当前运行线程的
`ITIMER_REAL` 投递仍留在本 Hart 的 timer interrupt。LoongArch 的 polling/idle 路径随后
按全部配置 Hart 复核，防止 12-Hart BuildStorm 只由前 8 核消费队列。

*修正时间来源。* 新增动态只读 `/proc/uptime`，按架构 tick 与 clock frequency 输出稳定的
`seconds.centiseconds`。每个 open 保留一致 snapshot 和 file offset，支持读、`lseek`、`fstat`
和 `poll`；脚本不再因为启动期静态 proc 文件导致 `elapsed_s=0.00`。这项工作保证“成功标记中
的时间”是真实 guest 时间，而不是优化本身。

== 内存管理、COW 与 remote TLB

*按变更范围保留旧页帧。* 原 `MemorySet::with_mut()` 为所有页表更新收集整个驻留集。新接口
`with_retained_frames_mut()` 由调用方给出旧 frame 范围：`munmap`、堆收缩、`mremap`、
`MADV_DONTNEED`、`MAP_FIXED` 和 shared-memory unmap 仅克隆相关 VPN；新增映射、非替换
permission 更新和 fresh non-present fault 不复制旧帧。`recycle_data_pages()` 仍保留全量入口。
关键协议仍是：撤销 active hart -> 获取更新锁和 `MemorySet` 写锁 -> 修改 PTE -> shootdown/ACK
-> 释放旧 frame -> 恢复 active hart。

*批处理并行同步。* 内层 VMA/PTE 操作遗留的 `tlb_invalidate()` 被移除，外层 handle 在一次
syscall 的全部 PTE 修改后执行一次 local invalidation 和必要的 remote shootdown。发送端改为
三阶段：为所有 remote hart 发布 mailbox sequence，发出全部 IPI，再分别 collect ACK；Release/
Acquire 内存序、`UPDATE_LOCK`、active-hart 退出规则和“ACK 后释放旧帧”不变。

*区分独占与共享 COW。* `Arc::strong_count` 必须在为远端 ACK 克隆保活引用之前检查。若该页
独占，只恢复同一 PPN 的 writable/dirty 权限，不分配/复制 4 KiB frame，也不做 PPN 替换的
remote ACK；真实共享页仍按完整 copy + shootdown 协议执行。两架构页表 helper 均提供 COW
识别，perf 输出 `exclusive_upgrade` 与 `shared_frame_copy`，防止优化掩盖语义回归。

== 文件读取、页缓存与 EXT4 入口

*跨进程复用干净文件页。* 将文件页缓存用于干净的 `MAP_PRIVATE` 只读映射，私有可写页仍
清除 PTE write 并标 COW，首次写入走正常分裂；`MAP_SHARED` 可写语义不变。缺页直接把
`inode.read_at()` 写进已由分配器清零的 frame，删除中间 `Vec<u8>` 和重复清零检查。

*消除普通 read 的重复工作。* 只读 `Ext4File` 走 `file_open_read_only()` 和一次
`file_read_at()`，跳过无用的 write-back cache 准备、路径 reopen 与 `fseek`；已有 `O_RDWR`
descriptor 时可被只读操作复用。`OSFile::read()` 删除预先 `inode.size()` EOF 探测，对不超过
64 KiB 的跨页用户缓冲合并为一次 bounded read；混合 cached/miss 的多页读仅把连续 miss
段交给 `inode.read_at()`。缓存 inode 的类型和单次 lookup 结果复用，避免命中后再进
`EXT4_OP_LOCK` 查询类型。

*不错误并行化 lwext4。* lwext4 C API/bcache 仍由安全 gate 串行。优化在 gate 外消除冗余
path/metadata 操作，在 gate 内缩短 descriptor/open/read 临界段；页缓存命中写不再不必要地
进入 EXT4 全局锁。read-ahead 保持最多四页/16 KiB 的有界窗口，曾经会放大 read lock
wait/hold 的更大窗口已撤回。

== 稀疏写、元数据与块层写回

Rustc 会交错写入稀疏临时文件、改变 mode/owner、rename 发布 artifact。实现把可连接的
sparse ranges 合并并按旧写顺序重放，保持 last-write-wins、hole 布局、读可见性与
flush 边界；目录首次 lookup、create 的 mode/owner metadata transaction 和 stat 复用，减少
同一 inode 的多次 gate 占用。rename 仍先回写，失败返回 `EIO`，不发布部分产物。

在 C lwext4 bcache 层，`ext4_block_cache_flush()` 最多收集 32 个连续、同方向且无
`end_write` callback 的 dirty block；反向 dirty-list range 复制为升序临时连续 buffer 后使用
一次 `ext4_blocks_set_direct(..., count)`。遇到 callback、不连续 LBA、分配失败或 I/O 错误时
回退原逐块路径；批量提交失败不 `mark_clean`，所有 dirty block 保留以供重试。这样降低小
写请求数而不重排 journal callback 的可见顺序。

== 低开销观测和回归护栏

`os/src/utils/perf/` 使用 relaxed 原子做累计 syscall、page-cache、EXT4 lock、block request、
scheduler、COW 和 remote-TLB 统计，周期性打印而不逐调用输出。性能 feature 才编译 C 侧
lwext4 telemetry，默认 release 不导出该符号。报告中用它定位，不把它当火焰图或 wall-clock。

相关回归包括：bcache 生命周期测试验证四连续块一次提交、批量 EIO 后内容不变和重试恢复；
VMA/mremap、COW、rseq、uptime、fd/epoll、rename 与动态加载问题均有定向复盘或回归入口。
双架构 release/perf 构建在相应提交轮次完成；真正影响共享状态的改动仍要求在 RISC-V 与
LoongArch64 两侧重跑。

= 实验分析

== 已获得的数据及加速比

下表只列出能从同一问题复盘明确追溯、且没有把累计并发时间冒充 wall-clock 的数据。

#table(
  columns: (9em, 6.2em, 6.2em, 7.1em, 1fr),
  table.header([*项目*], [*修改前*], [*修改后*], [*计算*], [*结论边界*]),
  [`axbuild` 尾部编译单元], [约 12 min], [约 8 min], [时间减少约 4 min；`12 / 8 = 1.50x`；减少约 33.3%], [同一 BuildStorm 尾部单元的定向观测；未达到 446/446 结束，不外推为整场成绩],
  [BuildStorm 早期吞吐现场], [超过 2 h 仅约 `97/446`], [多次修复后可推进到 `443--445/446` 的长窗口], [工作负载和时间窗口不同，不计算倍率], [证明绕过旧卡点和稳定性改善，不是可比 A/B],
  [EXT4 中途计数], [16-run 样本：689 个提交 range，平均约 23.50 KiB], [32-run 样本：628 个，平均约 25.75 KiB], [range 数 -8.85%；平均大小 +9.59%], [Cargo 阶段不同，不能写成端到端加速],
  [独占 COW 冒烟], [独占页被当作共享 copy，产生不必要 PPN 替换], [perf：`exclusive_upgrade=60`，`shared_frame_copy=72`], [语义路径已区分], [短样本，无完整 wall-clock],
)

第一行数据来自 `problem/buildstorm-memoryset-full-resident-retention.md`：范围化
`MemorySet` 更新后，在新 qcow2 overlay 的 RISC-V 15 分钟窗口中，toolchain/minibuild 均
通过，Cargo 从 `440/446` 推进到 `445/446: tg-xtask(bin)`，观测到 `axbuild` 约 8 分钟；
维护者提供的旧版单元基线为约 12 分钟。由于数据以“约”记录，报告只保留合理精度的
1.50x，不伪造均值、方差或小数秒。

== 当前完整成绩的诚实状态

当前工作树的根目录 `log.ans` 已包含 `BUILDSTORM_TOOLCHAIN ok`、
`BUILDSTORM_MINIBUILD ok` 和 `BUILDSTORM_BEGIN mode=multi`，但尚未有
`BUILDSTORM_COMPILE mode=multi ok=true elapsed_s=...` 或测试组 END。因此：

- 不能报告“完整编译时间”或“整体加速比”；
- 不能用 `Building 443/446`、timeout、跨 Hart 累计 lock wait 或 I/O service 反推；
- 已测 1.50x 只能支撑 `axbuild` 尾部热点的方向和量级，不能作为评分脚本的最终性能项；
- 下一节给出固定条件下的完整 A/B 复现，评审者可将两个 `elapsed_s` 直接代入速度比。

该边界是刻意设计：BuildStorm 同时受 guest 持久缓存、QEMU 宿主竞争和 Cargo 编译图影响。
把未完成样本写成确定性能结论既不能复现，也会掩盖内核稳定性问题。

= AI 使用说明与可复现步骤

== AI 的角色、人工核验和采纳范围

本项目在相关开发中使用 Codex（GPT-5）协助阅读 QEMU/GDB/log、检索调用路径、提出可证伪
假设、生成定向回归思路和整理文档。AI 不是性能数据来源：所有采纳结论均以源码审计、
Git diff、真实日志、构建或测试输出核验。特别地，若后续日志推翻了某一推断，已有记录会
明确勘误或撤回，而不是保留为“优化”。完整过程记录在 `Docs/决赛文档/ai.log` 与
`Docs/决赛文档/AI_INTERACTION.md`；各问题的根因、代码范围与验证边界位于 `problem/`。

本报告本身的编制同样使用 AI 辅助聚合近一个月的 Git 历史和已有复盘。人工核验点是：
脚本完成协议、提交触及的源码、各复盘的验证段，以及本报告中所有带数值的表格。AI 没有
生成或补全缺失的 `elapsed_s` 数据。

== 环境固定

1. 检出源码快照 `3e0e982f145885e821b45522c33178eeef9ac473`，确认 `git status --short`
   没有会影响内核、用户程序、脚本或镜像配置的未预期改动。
2. 记录架构、QEMU 版本、宿主 CPU/负载、`scripts/{riscv64,loongarch64}.mk` 中的
   `MEMORY_SIZE` 与 `SMP`。该快照的默认值为 RISC-V `16G/8`，LoongArch64 `36G/12`。
3. 不直接写维护者的 raw 镜像。为每一组 baseline/optimized 单独准备同一 raw base 的 qcow2
   overlay，或者保证 `-snapshot`、overlay 与 guest cache 处理完全一致。RISC-V `make run`
   使用 `-snapshot`；LoongArch64 配置也应显式采用同等隔离的覆盖层后再启动 QEMU。
4. 分别构建 release 和用于归因的 perf 内核：

```bash
make build-arch TARGET_ARCH=riscv64
make build-arch TARGET_ARCH=loongarch64
make perf TARGET_ARCH=riscv64
make perf TARGET_ARCH=loongarch64
```

== 完整 A/B 操作

对 baseline commit 和优化快照各执行至少两次 cold run；每次使用新 overlay，保持同一个
final-2026 base image、相同 Hart/memory、相同 `initproc` 入口和同一 QEMU 命令。把串口输出
和外层 wall-clock 分开保存，示例为：

```bash
/usr/bin/time -f 'elapsed_s=%e user_s=%U sys_s=%S' \
  timeout 14460s make run TARGET_ARCH=riscv64 \
  > /tmp/buildstorm-riscv-baseline-run1.log 2>&1

/usr/bin/time -f 'elapsed_s=%e user_s=%U sys_s=%S' \
  timeout 14460s make run TARGET_ARCH=riscv64 \
  > /tmp/buildstorm-riscv-optimized-run1.log 2>&1
```

真实复现时应替换为指向各自独立 overlay 的 QEMU drive 参数，不能让两轮共享 `/work/tgoskits/
target` 或写回缓存。若必须测热缓存，baseline 和 optimized 都须采用同一预热步骤并另行标注。
对 LoongArch64 重复相同流程；不要把一个架构的 tick、SMP 数或 QEMU 时间外推给另一架构。

== 结果提取、正确性检查和报告模板

每个运行都必须同时满足以下条件才进入速度比较：

```bash
rg -a -n 'BUILDSTORM_(TOOLCHAIN|MINIBUILD|COMPILE)|OS COMP TEST GROUP END buildstorm|panic|TFAIL|TBROK|ERROR' \
  /tmp/buildstorm-riscv-optimized-run1.log
```

要求看到 `TOOLCHAIN ok`、`MINIBUILD ok`、`COMPILE mode=multi ok=true elapsed_s=<X>` 和 group END，
且没有 panic/TFAIL/TBROK。抽取两个客观值后计算：

```text
time_reduction = (baseline_elapsed_s - optimized_elapsed_s) / baseline_elapsed_s
speedup = baseline_elapsed_s / optimized_elapsed_s
```

perf 版本仅用于解释变化：比较同一 phase interval 内的 `ext4_*_lock`、page-cache hit/miss、
block request、`remote_tlb_remote`、`cow_fault_resolution` 和 scheduler 指标；不要把它们的
跨 Hart 累计微秒相加为 elapsed。完整提交前还应执行 `git diff --check`，并对改动过文件系统
路径的 overlay 在 QEMU 停止后运行 `e2fsck -fn`。文档编译可使用：

```bash
typst compile Docs/决赛文档/buildstorm-优化实现文档.typ /tmp/buildstorm-优化实现文档.pdf
```

= 结论

BuildStorm 优化的核心不是解除所有锁或牺牲一致性换吞吐，而是在维持 Linux 用户可见语义、
lwext4 安全串行边界和 remote-TLB 旧帧生命周期协议的条件下，恢复并行度并删除重复工作。
近一个月的修复已让 Rust 工具链、MINIBUILD、Cargo 运行时、内存更新和 EXT4 写回路径可以
跨越多处历史卡点；可比定向观测显示 `axbuild` 尾部热点约 1.50x 加速。全量 446 crate 的
最终 `elapsed_s` 仍需按本文固定环境执行完整 A/B 后报告。本报告给出的边界、代码追溯和
复现流程使该结果可以被独立审核，而不依赖评分脚本或未完成日志的推断。
