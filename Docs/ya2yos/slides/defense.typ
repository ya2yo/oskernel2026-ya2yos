// Ya2yOS 答辩演示稿。构建：typst compile --root . defense.typ ya2yos-defense.pdf
#import "@preview/touying:0.7.4": *
#import themes.simple: *

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
#set text(font: body, size: 18pt, fill: ink, lang: "zh")
#set par(leading: 0.8em, spacing: 0.45em, justify: false)
#show heading: set text(font: body, weight: "bold", fill: navy)
#show heading.where(level: 1): set text(size: 28pt)
#show heading.where(level: 2): set text(size: 20pt)
#show raw: set text(font: mono, size: 12pt, lang: "en")
#set raw(block: true)
#show raw.where(block: true): it => block(fill: light, inset: 8pt, radius: 3pt)[#it]
#set list(indent: 1.3em, body-indent: 0.45em, spacing: 0.28em)

#show: simple-theme.with(
  aspect-ratio: "16-9",
  config-page(margin: (x: 1.1cm, y: 0.75cm), fill: white),
  header: none,
  footer: none,
  footer-right: context align(right)[#text(size: 9pt, fill: muted)[Ya2yOS · 2026-08 · #utils.slide-counter.display()]],
  primary: blue,
)
#let ya-slide(body) = slide[#body]
#let titlebar(kicker, title, subtitle: none) = {
  text(size: 11pt, weight: "bold", fill: blue)[#kicker]
  v(0.1em)
  text(size: 28pt, weight: "bold", fill: navy)[#title]
  if subtitle != none { v(0.2em); text(size: 13pt, fill: muted)[#subtitle] }
  v(0.55em)
  line(length: 100%, stroke: 1.3pt + blue)
}
#let pill(label, color: blue) = box(fill: color, radius: 4pt, inset: (x: 9pt, y: 4pt))[#text(size: 11pt, weight: "bold", fill: white)[#label]]
#let stat(value, label, color: blue) = block(fill: pale, stroke: 0.8pt + color, radius: 4pt, inset: 9pt)[
  #text(size: 25pt, weight: "bold", fill: color)[#value]\
  #text(size: 11pt, fill: muted)[#label]
]
#let card(title, body, color: blue, height: auto) = block(height: height, fill: pale, stroke: 0.7pt + rgb("cbd5df"), radius: 4pt, inset: 10pt)[
  #text(size: 15pt, weight: "bold", fill: color)[#title]\
  #text(size: 12.5pt, fill: ink)[#body]
]
#let two(left, right) = block(width: 100%)[#grid(columns: (1fr, 1fr), gutter: 14pt, left, right)]
#let three(a, b, c) = grid(columns: (1fr, 1fr, 1fr), gutter: 10pt, a, b, c)


// 1
#ya-slide[
#align(center + horizon)[
  #block(height: 82%)[
    #align(center)[
      #text(size: 11pt, weight: "bold", fill: blue)[YA2YOS · KERNEL ENGINEERING]
      #v(0.55cm)
      #text(size: 46pt, weight: "bold", fill: navy)[Ya2yOS]
      #text(size: 46pt, weight: "bold", fill: blue)[答辩演示稿]
      #v(0.45cm)
      #text(size: 18pt, fill: muted)[面向真实 Linux 用户态的双架构 Rust 宏内核]
      #v(1.05cm)
      #grid(columns: (auto, auto, auto), gutter: 14pt,
        pill("RISC-V64", color: cyan), pill("LoongArch64", color: orange), pill("Linux ABI", color: blue),
      )
      #v(1.0cm)
      #text(size: 13pt, fill: muted)[参赛队员：饶晓杰　　指导老师：杨磊]
      #v(0.18cm)
      #text(size: 10pt, fill: muted)[真实负载：BusyBox · LTP · Cargo · Rustc · CAgent · BuildStorm]
    ]
  ]
]
]

// 2
#ya-slide[
#titlebar("01 · 项目定位", "让真实 Linux 用户态运行在双架构 Rust 内核上", subtitle: "Ya2yOS 以 Linux ABI 为边界，贯通从用户程序到硬件设备的核心路径")
#two(
  card("项目形态", [基于 Rust 的宏内核实验系统；
  面向 RISC-V64 与 LoongArch64 平台。], color:blue, height: 3cm),
  card("兼容目标", [以 Linux ABI、errno、uaccess
  和真实用户态语义为边界。], color:cyan, height: 3cm),
)
#v(0.6cm)
#align(center)[
  #text(size: 15pt, weight: "bold", fill: navy)[用户态负载]
  #v(0.2cm)
  #grid(columns: (auto, auto, auto, auto, auto), gutter: 10pt,
    pill("BusyBox", color: blue), pill("LTP", color: cyan), pill("Cargo / Rustc", color: orange), pill("CAgent", color: red), pill("BuildStorm", color: navy),
  )
]
]

// 3
#ya-slide[
#titlebar("02 · 系统框架", "用户态、内核机制与硬件设备的完整闭环", subtitle: "分层组织内核能力，将语义、机制与硬件差异放在正确的位置")
#align(center)[
  #grid(columns: (1fr,), gutter: 6pt,
    box(fill: rgb("dbeef7"), inset: 9pt, radius: 4pt)[#text(size: 16pt, weight: "bold", fill: navy)[用户程序与测试负载]#h(1em)#text(size: 12pt, fill: muted)[BusyBox · LTP · Cargo · Rustc · CAgent]],
    text(size: 18pt, fill: blue)[↓],
    box(fill: rgb("e6f4f1"), inset: 9pt, radius: 4pt)[#text(size: 15pt, weight: "bold", fill: cyan)[Linux ABI / syscall / trap / uaccess / errno]],
    text(size: 18pt, fill: blue)[↓],
    box(fill: rgb("fff0e4"), inset: 9pt, radius: 4pt)[#text(size: 15pt, weight: "bold", fill: orange)[task / scheduler / mm / VFS / signal / net / timer]],
    text(size: 18pt, fill: blue)[↓],
    box(fill: rgb("f3e9f4"), inset: 9pt, radius: 4pt)[#text(size: 15pt, weight: "bold", fill: red)[lwext4 / page cache / VirtIO / smoltcp / arch]],
  )
]
#v(0.45cm)
#align(center)[#text(size: 13pt, fill: muted)[syscall 入口保持薄；语义落在所属子系统；锁外 I/O、锁内短更新。]]
]

// 4
#ya-slide[
#titlebar("03 · 进程与 Linux 语义", "完善进程模型，承载真实 Linux 程序")
#three(
  card("统一进程模型", [task、线程、fork / clone / exec / wait 形成完整进程链；调度和阻塞共享同一套状态。], color:blue),
  card("动态程序加载", [`PT_INTERP` 经 VFS 打开；处理用户栈、aux 和映射边界；库搜索与重定位由用户态 loader 负责。], color:cyan),
  card("统一资源接口", [signal / sigreturn、futex、file、pipe、socket、eventfd 和 epoll 纳入可组合的 fd / ABI 模型。], color:orange),
)
#v(0.7cm)
#align(center)[#text(size: 14pt, weight: "bold", fill: navy)[用户态现象 → 内核语义 → 错误返回 → 回归测试]]
#v(0.3cm)
#align(center)[#text(size: 13pt, fill: muted)[兼容的核心不是接口存在，而是程序在边界条件下仍能得到正确语义。]]
]

// 5
#ya-slide[
#titlebar("04 · 内存管理", "构建高效且一致的共享地址空间")
#two(
  card("内存机制", [mmap / munmap / mremap、缺页、COW 和地址空间更新由统一 MemorySet 抽象承载；独占 COW 页可就地恢复写权限。], color:blue),
  card("并发协议", [范围化保留旧页帧；remote TLB 广播 mailbox 并收集 ACK；ACK 收敛前不释放可能仍被使用的旧映射。], color:cyan),
)
#v(0.65cm)
#align(center)[
  #text(size: 15pt, weight: "bold", fill: navy)[PTE 更新 → TLB / I-cache shootdown → ACK → 旧帧回收]
  #v(0.35cm)
  #three(pill("RISC-V Sv39", color: cyan), pill("LoongArch PTE / IBar", color: orange), pill("同一生命周期协议", color: blue))
]
#v(0.4cm)
#align(center)[#text(size: 13pt, fill: muted)[正确性不变量：所有可能使用旧映射的 hart 完成失效确认前，旧物理页不能回收。]]
]

// 6
#ya-slide[
#titlebar("05 · 调度与多核", "多个 HART 协同推进任务")
#three(
  card("统一调度", [all-hart ready queue、affinity 过滤和任务状态转换，让可运行任务不被困在 home hart。], color:blue),
  card("远端唤醒", [空闲 hart 通过 IPI 被唤醒；futex、文件等待和设备等待回到统一 park / wake 路径。], color:cyan),
  card("全局时间", [共享 timer / futex 状态由全局 timer 协调；`/proc/uptime` 动态生成，时间口径可追溯。], color:orange),
)
#v(0.7cm)
#align(center)[
  #text(size: 16pt, weight: "bold", fill: navy)[runnable task → 调度 → 阻塞 → IPI / timer 唤醒 → 再次运行]
  #v(0.3cm)
  #text(size: 13pt, fill: muted)[多核性能的前提是先让调度、唤醒和资源生命周期可观察。]
]
]

// 7
#ya-slide[
#titlebar("06 · 文件系统与网络", "用真实设备路径承载用户态程序")
#two(
  card("文件系统", [VFS 统一管理文件、目录、链接和 fd；通过 `lwext4_rust` 接入真实 ext4，并对接 page cache 与 VirtIO block。], color:blue),
  card("网络路径", [以 smoltcp 支撑 TCP / UDP / Unix socket 等用户态路径；VirtIO-net 与设备等待接入统一抽象。], color:cyan),
)
#v(0.65cm)
#three(
  card("缓存复用", [页缓存复用；普通 read 路径收敛；目录局部 stat epoch。], color:cyan),
  card("I/O 合并", [连续冷页 read-ahead；稀疏 range 合并；连续 bcache 写回批处理。], color:blue),
  card("安全边界", [保留 lwext4 C API、journal、bcache callback 和单队列的必要串行域。], color:orange),
)
#v(0.45cm)
#align(center)[#pill("不是取消锁，而是缩短安全串行域、删除重复工作", color: navy)]
]

// 8
#ya-slide[
#titlebar("07 · 双架构与设备", "一套上层语义，如何落到两种硬件？")
#two(
  card("RISC-V64", [启动与异常入口、Sv39 页表、TLB / IPI 和 VirtIO-MMIO 由架构层提供；上层 task / mm / fs 尽量复用。], color:cyan),
  card("LoongArch64", [启动与异常入口、PTE / TLB、IBar 和 PCI VirtIO 由架构层提供；保持同一用户态 ABI 和内核服务接口。], color:orange),
)
#v(0.7cm)
#align(center)[
  #grid(columns: (1fr, 1fr, 1fr), gutter: 10pt,
    pill("启动 / trap", color: blue), pill("页表 / TLB", color: cyan), pill("设备 / 中断", color: orange),
  )
]
#v(0.55cm)
#align(center)[#text(size: 15pt, weight: "bold", fill: navy)[架构差异止于接口边界，不向 task / mm / fs 语义层扩散。]]
]

// 9
#ya-slide[
#titlebar("08 · 核心优化专题", "从“跑不通”到“可定位、可优化”")
#two(
  card("问题定位", [真实 BuildStorm 负载暴露 futex、EXT4、remote shootdown、COW 和 block I/O 热点；perf 计数器与 tick 埋点用于归因。], color:red),
  card("优化方法", [保留底层安全边界，减少缓存探测与小块写回；通过锁外 I/O、范围化协议和批处理降低重复工作。], color:blue),
)
#v(0.7cm)
#align(center)[
  #text(size: 15pt, weight: "bold", fill: navy)[发现耗时异常 → 追踪调用路径 → 加入观测 → 局部优化 → 双架构回归]
  #v(0.35cm)
  #three(pill("可观测", color: cyan), pill("可解释", color: orange), pill("可复现", color: blue))
]
]

// 10
#ya-slide[
#titlebar("09 · 工作总结与验证", "我们如何证明系统能力？")
#three(
  card("语义验证", [libc-test / LTP、TPASS / TFAIL / TBROK、panic、errno 和 summary，关注测试断言而非包装脚本返回码。], color:blue),
  card("真实负载", [BusyBox、Cargo、Rustc、CAgent 和 BuildStorm 连接进程、内存、文件、调度和设备路径。], color:cyan),
  card("双架构回归", [RISC-V64 与 LoongArch64 双侧构建/运行；问题复盘保留现象、根因、修复和验证边界。], color:orange),
)
#v(0.65cm)
#two(
  block(fill: rgb("eaf4fb"), radius: 4pt, inset: 13pt)[
    #text(size: 15pt, weight: "bold", fill: navy)[定向观测]\
    #v(0.25em)
    #text(size: 29pt, weight: "bold", fill: blue)[12 min → 8 min]\
    #text(size: 13pt, fill: muted)[同一尾部 axbuild 单元 · 约 1.50×]
  ],
  card("证据边界", [当前日志没有官方完整 `BUILDSTORM_COMPILE ... ok=true elapsed_s=...` 收尾行；因此不宣称完整 446 crate 成绩。], color:red),
)
]

// 11
#ya-slide[
#titlebar("10 · 发展目标", "从核心路径跑通走向系统语义做深")
#three(
  card("已验证", [双架构启动；进程与地址空间；动态 ELF；signal / sigreturn；COW / mmap；VFS / ext4；网络基础路径。], color:blue),
  card("持续完善", [更多 Linux 语义、LTP 场景、多核边界、文件系统异常恢复、网络复杂场景和页缓存写回。], color:cyan),
  card("明确边界", [完整 io_uring、timerfd、独立 procfs/devtmpfs、真实 loop backing file、完整 namespace/cgroup/LSM 仍是后续方向。], color:red),
)
#v(0.6cm)
#grid(columns: (1fr, 1fr, 1fr), gutter: 10pt,
  card("短期", [双架构自动化矩阵；脏页回写；VirtIO-net 唤醒；stub 分类。], color:blue),
  card("中期", [tmpfs / procfs / devtmpfs；mount namespace；负载均衡；io_uring / AIO。], color:cyan),
  card("长期", [namespace / cgroup 深化；更多 VirtIO 设备；IPv6 / netlink / raw socket。], color:orange),
)
#v(0.45cm)
#align(center)[#pill("主线：减少兼容假设，把已经跑通的路径做深、做稳、做可复现", color: navy)]
]

// 12
#ya-slide[
#align(center + horizon)[
  #text(font: "Ubuntu Mono", size: 35pt, weight: "bold", fill: blue)[Ya2yOS]
  #v(0.35cm)
  #text(size: 34pt, weight: "bold", fill: navy)[谢谢！]
  #v(0.7cm)
  #text(size: 17pt, fill: muted)[问题与讨论]
  #v(0.9cm)
  #text(size: 12pt, fill: muted)[设计文档：`Docs/ya2yos/`　·　问题复盘：`Docs/决赛文档/problem/`]
]
]
