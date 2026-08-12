// Ya2yOS 答辩演示稿。构建：typst compile --root . defense.typ ya2yos-defense.pdf
#let navy = rgb("102a43")
#let blue = rgb("1677b8")
#let cyan = rgb("2a9d8f")
#let orange = rgb("e07a35")
#let red = rgb("b23a48")
#let ink = rgb("1d2833")
#let muted = rgb("52606d")
#let light = rgb("eef4f7")
#let pale = rgb("f7fafb")
#let white = rgb("ffffff")
#let body = ("WenQuanYi Zen Hei", "Libertinus Serif")
#let latin = ("Aptos", "DejaVu Sans")
#let mono = ("DejaVu Sans Mono", "WenQuanYi Zen Hei Mono")

#set document(title: "Ya2yOS 内核设计与工程实践答辩", author: "Ya2yOS")
#set page(paper: "presentation-16-9", margin: (x: 1.1cm, y: 0.75cm), fill: white)
#set text(font: body, size: 18pt, fill: ink, lang: "zh")
#set par(leading: 0.8em, spacing: 0.45em, justify: false)
#show heading: set text(font: body, weight: "bold", fill: navy)
#show heading.where(level: 1): set text(size: 28pt)
#show heading.where(level: 2): set text(size: 20pt)
#show raw: set text(font: mono, size: 12pt, lang: "en")
#set raw(block: true)
#show raw.where(block: true): it => block(fill: light, inset: 8pt, radius: 3pt)[#it]
#set list(indent: 1.3em, body-indent: 0.45em, spacing: 0.28em)

#let footer() = align(right)[#text(size: 9pt, fill: muted)[Ya2yOS · 2026-08 · #context counter(page).display("1")]]
#let titlebar(kicker, title, subtitle: none) = {
  text(size: 11pt, weight: "bold", fill: blue)[#kicker]
  v(0.1em)
  text(size: 28pt, weight: "bold", fill: navy)[#title]
  if subtitle != none { v(0.2em); text(size: 13pt, fill: muted)[#subtitle] }
  v(0.55em)
  line(length: 100%, stroke: 1.3pt + blue)
}
#let pill(label, color: blue) = box(fill: color, radius: 4pt, inset: (x: 9pt, y: 4pt))[text(size: 11pt, weight: "bold", fill: white)[#label]]
#let stat(value, label, color: blue) = block(fill: pale, stroke: 0.8pt + color, radius: 4pt, inset: 9pt)[
  text(size: 25pt, weight: "bold", fill: color)[#value]\
  text(size: 11pt, fill: muted)[#label]
]
#let card(title, body, color: blue) = block(fill: pale, stroke: 0.7pt + rgb("cbd5df"), radius: 4pt, inset: 10pt)[
  text(size: 15pt, weight: "bold", fill: color)[#title]\
  text(size: 12.5pt, fill: ink)[#body]
]
#let two(left, right) = grid(columns: (1fr, 1fr), gutter: 14pt, left, right)
#let three(a, b, c) = grid(columns: (1fr, 1fr, 1fr), gutter: 10pt, a, b, c)

// 1
#align(center)[
  v(1.4cm)
  text(font: latin, size: 21pt, weight: "bold", fill: blue)[Ya2yOS]
  v(0.25cm)
  text(size: 35pt, weight: "bold", fill: navy)[从能启动到能承载真实负载]
  v(0.35cm)
  text(size: 19pt, fill: muted)[Rust 宏内核 · Linux 用户态兼容 · RISC-V64 / LoongArch64]
  v(1.1cm)
  grid(columns: (1fr, 1fr), gutter: 10pt,
    stat("2", "目标架构", cyan), stat("4", "近月工程主线", orange),
    stat("1.50×", "定向 BuildStorm 加速", blue), stat("0", "虚构的完整成绩", red),
  )
  v(1.0cm)
  text(size: 13pt, fill: muted)[参赛队员：饶晓杰　指导老师：杨磊]
  v(0.2cm)
  text(size: 10pt, fill: muted)[代码快照：HEAD 088f10b82bb8 · 2026-08-12]
]
#footer()

// 2
#titlebar("01 · 项目定位", "我们解决什么问题？", subtitle: "让真实 Linux 用户态路径在双架构 Rust 内核上闭环运行")
three(
  card("用户态目标", [glibc / musl、BusyBox、libc-test、LTP 子集与 Rust 工具链], blue),
  card("内核形态", [Rust 宏内核；以 Linux ABI 为兼容边界，而不是 syscall 数量竞赛], cyan),
  card("工程约束", [高并发 fork/exec/mmap、真实 ext4、SMP 共享地址空间、QEMU 双架构], orange),
)
v(0.6cm)
#align(center)[
  pill("启动 → 进程 → 内存 → 文件 → 网络 → 信号 → 设备", navy)
  v(0.4cm)
  text(size: 15pt, fill: muted)[近月工作的重点：把“能调用”推进到“语义正确、并发可控、证据可复核”。]
]
#footer()

// 3
#titlebar("02 · 总体架构", "一条用户态请求如何穿过内核？")
#align(center)[
  grid(columns: (1fr,), gutter: 7pt,
    box(fill: rgb("dbeef7"), inset: 10pt, radius: 4pt)[text(size: 16pt, weight: "bold", fill: navy)[Cargo / Rustc / BusyBox / LTP]],
    text(size: 20pt, fill: blue)[↓],
    box(fill: rgb("e6f4f1"), inset: 10pt, radius: 4pt)[text(size: 15pt, weight: "bold", fill: cyan)[syscall · trap · uaccess · ABI 校验]],
    text(size: 20pt, fill: blue)[↓],
    box(fill: rgb("fff0e4"), inset: 10pt, radius: 4pt)[text(size: 15pt, weight: "bold", fill: orange)[task / mm / fs / net / signal / timer]],
    text(size: 20pt, fill: blue)[↓],
    box(fill: rgb("f3e9f4"), inset: 10pt, radius: 4pt)[text(size: 15pt, weight: "bold", fill: red)[VFS + lwext4 + page cache + VirtIO + smoltcp]],
  )
]
v(0.45cm)
#align(center)[text(size: 13pt, fill: muted)[核心设计原则：syscall 入口保持薄；语义落在所属子系统；锁外 I/O、锁内短更新。]]
#footer()

// 4
#titlebar("03 · 近月主线 A", "共享地址空间：把一致性协议做成可解释的状态机")
two(
  card("问题", [多 hart 同时执行 fork / COW / munmap / mremap：逐目标等待、全量帧保留和旧 TLB 可能放大延迟与错误], red),
  card("方案", [范围化旧帧保留；广播 mailbox 后收集 ACK；ACK 收敛前不释放旧帧；独占 COW 就地恢复写权限], blue),
)
v(0.55cm)
#align(center)[
  text(size: 14pt, weight: "bold", fill: navy)[UPDATE_LOCK → MemorySet 写锁 → PTE 更新 → TLB / I-cache shootdown → 释放旧帧]
  v(0.35cm)
  three(pill("RISC-V Sv39", cyan), pill("LoongArch PTE / IBar", orange), pill("同一生命周期协议", blue))
]
#footer()

// 5
#titlebar("04 · 近月主线 B", "从信号帧到动态链接：边界回归 Linux 语义")
three(
  card("信号 ABI", [`rt_sigreturn` 读取完整受检 frame；非法 frame 返回 `EINVAL`；两架构统一布局与 trampoline], blue),
  card("动态 ELF", [`PT_INTERP` 原路径经 VFS 打开；缺失保留 `ENOENT`；共享对象返回原始字节], cyan),
  card("新增兼容面", [`memfd_secret` 基础匿名 fd、`signalfd4`、System V 消息队列、`rseq`、`seccomp` 等], orange),
)
v(0.55cm)
#align(center)[text(size: 13pt, fill: muted)[关键取舍：内核负责 ELF/VFS 边界；库搜索、重定位和安全增强不伪装成内核已实现。]]
#footer()

// 6
#titlebar("05 · 近月主线 C", "ext4 并发优化：缩短安全串行域，而不是取消它")
two(
  card("必须保留的边界", [lwext4 C API、journal、bcache callback、单一 VirtIO 队列存在不可并行资源；任务感知锁负责 park/wake 与退出清理], orange),
  card("可安全消除的重复", [页缓存复用、连续冷页 read、目录局部 stat epoch、稀疏 range 合并、连续 bcache 写回批处理], cyan),
)
v(0.65cm)
#align(center)[
  grid(columns: (1fr, 1fr, 1fr, 1fr), gutter: 8pt,
    pill("VFS 短锁", blue), pill("资源锁 FIFO", orange), pill("锁外 I/O", cyan), pill("失败可重试", red),
  )
]
#footer()

// 7
#titlebar("06 · 近月主线 D", "调度与时间：让多核真实可见、让计时可信")
three(
  card("共享 CFS", [all-hart ready queue、affinity 过滤、空闲远端 hart IPI 唤醒，避免 runnable task 困在 home hart], blue),
  card("全局 timer", [每 10ms 由单一 hart 排他维护共享 timer/futex 状态；当前线程 interval timer 仍在本 hart 投递], cyan),
  card("可观测性", [`/proc/uptime` 动态生成；perf 按 scheduler / COW / TLB / EXT4 / block 聚合，release 默认低开销], orange),
)
v(0.55cm)
#align(center)[text(size: 13pt, fill: muted)[时间口径先正确，性能数字才有意义。累计计数用于定位，不冒充 wall-clock。]]
#footer()

// 8
#titlebar("07 · 性能证据", "BuildStorm：从“跑不通”进入“可优化”")
two(
  block(fill: rgb("eaf4fb"), radius: 4pt, inset: 14pt)[
    text(size: 15pt, weight: "bold", fill: navy)[可追溯定向观测]\
    v(0.3em)
    text(size: 31pt, weight: "bold", fill: blue)[12 min → 8 min]\
    text(size: 15pt, fill: muted)[同一尾部编译单元 · 时间缩短约 33.3% · 加速约 1.50×]
  ],
  card("证据边界", [根目录当前日志尚未形成官方完整 `BUILDSTORM_COMPILE ... ok=true elapsed_s=...` 收尾行，因此不宣称 446 crate 全量成绩。长测必须同源码、架构、SMP/内存、镜像和缓存条件对照。], red),
)
v(0.6cm)
#align(center)[text(size: 14pt, weight: "bold", fill: navy)[正确性 → 可观测性 → 热点归因 → 局部优化 → 双架构回归]]
#footer()

// 9
#titlebar("08 · 验证方法", "我们如何知道改动没有破坏内核？")
three(
  card("语义证据", [`TPASS / TFAIL / TBROK`、panic、errno 与 summary；关注测试断言而非包装脚本返回码], blue),
  card("架构证据", [`make TARGET_ARCH=riscv64` 与 `loongarch64`；共享状态改动尽量双侧构建/运行], cyan),
  card("设计证据", [每个高风险问题保留 problem 复盘：背景、现象、根因、修复、涉及文件、验证与已知边界], orange),
)
v(0.7cm)
#align(center)[pill("当前快照：HEAD 088f10b82bb8 · 文档版本 0.6", navy)]
#footer()

// 10
#titlebar("09 · 设计选择", "我们刻意没有把什么写成“已经完成”？")
two(
  card("明确边界", [完整 `io_uring` 引擎、timerfd、独立 procfs/devtmpfs、真实 loop backing file、网络中断唤醒、NUMA、完整 namespace/cgroup/LSM 仍是后续方向], red),
  card("答辩口径", [把“基础兼容 fd”与“完整 Linux 安全语义”分开；把“定向加速”与“完整成绩”分开；把“接口存在”与“测试通过”分开], blue),
)
v(0.75cm)
#align(center)[text(size: 14pt, fill: muted)[诚实的边界描述，本身是内核设计可维护性的组成部分。]]
#footer()

// 11
#titlebar("10 · 下一步", "从兼容面走向更深的系统语义")
grid(columns: (1fr, 1fr), gutter: 12pt,
  card("短期", [完善脏页回写调度、VirtIO-net 中断/唤醒、真实 loop 数据路径、stub 分类与双架构自动化矩阵], blue),
  card("中期", [独立 tmpfs/procfs/devtmpfs、mount namespace 与真实 dentry 切换、CPU affinity/负载均衡、io_uring/AIO], cyan),
  card("长期", [namespace/cgroup/capability/seccomp 深化、更多 VirtIO 设备、IPv6/netlink/raw socket 与工程化持续测试], orange),
  block(fill: rgb("eaf4fb"), stroke: 0.8pt + navy, radius: 4pt, inset: 13pt)[text(size: 18pt, weight: "bold", fill: navy)[主线：减少兼容假设，把已经跑通的路径做深。]],
)
#footer()

// 12
#align(center)[
  v(1.6cm)
  text(font: latin, size: 23pt, weight: "bold", fill: blue)[Ya2yOS]
  v(0.35cm)
  text(size: 34pt, weight: "bold", fill: navy)[谢谢！]
  v(0.7cm)
  text(size: 17pt, fill: muted)[问题与讨论]
  v(1.1cm)
  text(size: 12pt, fill: muted)[设计文档：`Docs/ya2yos/`　·　问题复盘：`Docs/决赛文档/problem/`]
]
#footer()
