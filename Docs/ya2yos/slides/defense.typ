// Ya2yOS 答辩演示稿。构建：typst compile --root . defense.typ ya2yos-defense.pdf
#import "@preview/touying:0.7.4": *
#import themes.simple: *

#let navy = rgb("102a43")
#let blue = rgb("1677b8")
#let teal = rgb("16817a")
#let orange = rgb("d97732")
#let red = rgb("b23a48")
#let ink = rgb("1d2833")
#let muted = rgb("52606d")
#let border = rgb("d8e2e8")
#let pale-blue = rgb("eaf4fb")
#let pale-teal = rgb("e8f5f2")
#let pale-orange = rgb("fff1e6")
#let pale-red = rgb("fbecee")
#let white = rgb("ffffff")
#let body = ("WenQuanYi Zen Hei", "Libertinus Serif")
#let mono = ("DejaVu Sans Mono", "WenQuanYi Zen Hei Mono")

#set document(title: "Ya2yOS 内核设计与工程实践答辩", author: "Ya2yOS")
#set text(font: body, size: 17pt, fill: ink, lang: "zh")
#set par(leading: 0.78em, spacing: 0.35em, justify: false)
#show heading: set text(font: body, weight: "bold", fill: navy)
#show heading.where(level: 1): set text(size: 27pt)
#show raw: set text(font: mono, size: 11.5pt, lang: "en")
#set raw(block: true)
#show raw.where(block: true): it => block(fill: pale-blue, inset: 8pt, radius: 3pt)[#it]
#set list(indent: 1.25em, body-indent: 0.4em, spacing: 0.23em)

#show: simple-theme.with(
  aspect-ratio: "16-9",
  config-page(margin: (x: 1.05cm, y: 0.7cm), fill: white),
  header: context block(width: 100%, height: 1.2cm)[#align(right + horizon)[#move(dy: 0.8cm)[#image(read("../../img/SCUT_LOGO.webp", encoding: none), format: "webp", width: 0.85cm, height: 0.85cm, fit: "contain")]]],
  footer: none,
  footer-right: context align(right)[#text(size: 8.5pt, fill: muted)[Ya2yOS  ·  2026-08  ·  #utils.slide-counter.display()]],
  primary: blue,
)

#let titlebar(kicker, title, subtitle: none) = {
  text(size: 10pt, weight: "bold", fill: blue)[#kicker]
  v(0.08em)
  text(size: 27pt, weight: "bold", fill: navy)[#title]
  if subtitle != none { v(0.16em); text(size: 12.5pt, fill: muted)[#subtitle] }
  v(0.45em)
  line(length: 100%, stroke: 1.2pt + blue)
}
#let tag(label, color: blue, fill: auto) = {
  let bg = if fill == auto { color } else { fill }
  box(fill: bg, radius: 3pt, inset: (x: 8pt, y: 3.5pt))[#text(size: 10.5pt, weight: "bold", fill: white)[#label]]
}
#let stat(value, label, color: blue) = block(fill: white, stroke: 1pt + color, radius: 4pt, inset: 10pt)[
  #text(size: 28pt, weight: "bold", fill: color)[#value]
  #v(0.12em)
  #text(size: 10.5pt, fill: muted)[#label]
]
#let panel(title, body, color: blue, fill: pale-blue) = block(fill: fill, stroke: 0.7pt + border, radius: 4pt, inset: 11pt)[
  #text(size: 15pt, weight: "bold", fill: color)[#title]
  #v(0.22em)
  #text(size: 12.2pt, fill: ink)[#body]
]
#let chapter-cover(no, title, subtitle, color: blue, fill: pale-blue) = slide[
  #align(center + horizon)[
    #block(fill: fill, stroke: 1.2pt + color, radius: 8pt, width: 78%, inset: 24pt)[
      #text(size: 12pt, weight: "bold", fill: color)[CHAPTER  #no]
      #v(0.35em)
      #text(size: 38pt, weight: "bold", fill: navy)[#title]
      #v(0.28em)
      #text(size: 15pt, fill: muted)[#subtitle]
      #v(0.6em)
      #line(length: 34%, stroke: 1.2pt + color)
    ]
  ]
]
#let compact-panel(title, body, color: blue, fill: pale-blue) = block(fill: fill, stroke: 0.7pt + border, radius: 4pt, inset: 8pt)[
  #text(size: 13pt, weight: "bold", fill: color)[#title]
  #v(0.12em)
  #text(size: 10.5pt, fill: ink)[#body]
]
#let flow-step(label, desc, color: blue, fill: pale-blue) = block(fill: fill, stroke: 0.8pt + color, radius: 4pt, inset: (x: 10pt, y: 9pt))[
  #text(size: 14pt, weight: "bold", fill: color)[#label]
  #v(0.13em)
  #text(size: 11pt, fill: muted)[#desc]
]

// Architecture-only helpers. The layout mirrors the five-module overview in
// the defense material and stays within one 16:9 slide.
#let arch-cols = (1fr, 1fr, 1.1fr, 1fr, 1fr)
#let arch-gap = 5pt
#let arch-user = rgb("eaeff5")
#let arch-sys = rgb("eeded9")
#let arch-proc-head = rgb("fbdcdb")
#let arch-proc-bg = rgb("fde8e8")
#let arch-mem-head = rgb("fdebd0")
#let arch-mem-bg = rgb("ffedd5")
#let arch-fs-head = rgb("fef08a")
#let arch-fs-bg = rgb("fef9c3")
#let arch-dev-head = rgb("dcfce7")
#let arch-dev-bg = rgb("e8f5e9")
#let arch-net-head = rgb("f3e8ff")
#let arch-net-bg = rgb("faf5ff")
#let arch-node(body, fill: white, stroke: 0.5pt + rgb("444444"), height: 17pt, text-color: ink, size: 7.3pt, inset: 1.5pt) = rect(
  width: 100%,
  height: height,
  fill: fill,
  stroke: stroke,
  radius: 2pt,
  inset: inset,
)[#align(center + horizon)[#text(size: size, fill: text-color)[#body]]]
#let arch-divider() = line(length: 100%, stroke: (dash: "dashed", thickness: 0.6pt, paint: rgb("8fa3b5")))
#let arch-module(body, fill, stroke) = rect(
  width: 100%,
  height: 76pt,
  fill: fill,
  stroke: stroke,
  radius: 2pt,
  inset: 3pt,
)[#body]
#let arch-label(label) = align(center + horizon)[#text(size: 7.5pt, weight: "bold", fill: muted)[#label]]

// 1 · cover
#slide[
  #block(fill: pale-blue, stroke: 1.2pt + border, radius: 8pt, width: 100%, height: 86%, inset: (x: 1.15cm, y: 0.85cm))[
    #align(center + horizon)[
      #text(size: 11pt, weight: "bold", fill: teal)[YA2YOS  ·  KERNEL ENGINEERING]
      #v(0.35cm)
      #text(size: 53pt, weight: "bold", fill: navy)[Ya2yOS]
      #v(0.08cm)
      #text(size: 30pt, weight: "bold", fill: blue)[内核设计与工程实践]
      #v(0.38cm)
      #text(size: 19pt, fill: muted)[Rust 宏内核实现总览]
      #v(0.68cm)
      #grid(columns: (auto, auto, auto), gutter: 10pt,
        tag("RISC-V64", color: teal), tag("LoongArch64", color: orange), tag("Linux ABI", color: blue),
      )
      #v(0.72cm)
      #text(size: 12pt, fill: muted)[参赛队员：饶晓杰　　指导老师：杨磊]
      #v(0.13cm)
      #text(size: 10pt, fill: muted)[syscall · task · scheduler · mm · VFS · signal · net · arch]
    ]
  ]
]

// 2 · contents
#slide[
  #titlebar("CONTENTS", "目录", subtitle: "从系统定位出发，沿实现路径走向可复核成果")
  #v(0.42cm)
  #grid(columns: (1fr, 1fr), gutter: 14pt,
    panel("01  ·  系统定位", [Rust 宏内核、Linux ABI、RISC-V64 / LoongArch64；面向真实 Linux 用户态路径与工具链负载。], color: blue, fill: pale-blue),
    panel("02  ·  系统介绍", [从系统架构图出发，依次介绍进程管理、多核调度、内存管理、信号机制、网络模块和设备驱动。], color: teal, fill: pale-teal),
    panel("03  ·  当前成果", [测例与回归证据；任务调度、多核异构；文件缓存、用户可见锁与内部锁边界。], color: orange, fill: pale-orange),
    panel("04  ·  发展规划", [完整 BuildStorm 闭环、运行中迁移、双架构压力回归，以及可证明的资源级并发。], color: red, fill: pale-red),
  )
  #v(0.62cm)
  #align(center)[#tag("关键词：兼容边界 · 状态机 · 并发控制 · 证据边界", color: navy)]
]

// 3 · positioning
#chapter-cover("01", "系统定位", "Rust 宏内核 · Linux ABI · RISC-V64 / LoongArch64", color: blue, fill: pale-blue)
#slide[
  #titlebar("01  ·  SYSTEM POSITIONING", "项目定位：Ya2yOS 是什么？", subtitle: "面向真实 Linux 用户态负载的 Rust 宏内核")
  #v(0.25cm)
  #grid(columns: (1fr, 0.18fr, 1fr, 0.18fr, 1fr), gutter: 6pt,
    flow-step("Rust", "内核主体 · 类型安全 · 显式所有权", color: blue, fill: pale-blue), text(size: 22pt, fill: blue)[→],
    flow-step("宏内核", "task · mm · fs · net · signal 共享地址空间", color: teal, fill: pale-teal), text(size: 22pt, fill: blue)[→],
    flow-step("Linux ABI", "syscall · errno · fd · signal · mmap", color: orange, fill: pale-orange),
  )
  #v(0.48cm)
  #grid(columns: (1fr, 1fr, 1fr), gutter: 11pt,
    panel("我们要解决什么问题", [让 glibc / musl、BusyBox、LTP 子集和 Rust 工具链等真实用户态程序，在自研内核上沿 Linux ABI 路径运行，而不是停留在“能启动、能调用”的接口演示。], color: blue, fill: pale-blue),
    panel("系统边界", [Ya2yOS 直接运行在 QEMU / 板级硬件上，不是用户态模拟器；以 Linux 用户可见语义为兼容边界，关注 syscall、errno、fd、阻塞 / 唤醒和资源释放。], color: teal, fill: pale-teal),
    panel("目标平台与负载", [支持 RISC-V64 / LoongArch64，面向高并发 fork / exec / mmap、真实 ext4、SMP 共享地址空间和双架构 QEMU 验证场景。], color: orange, fill: pale-orange),
  )
  #v(0.42cm)
  #align(center)[#tag("定位：用 Rust 构建可运行、可扩展、可验证的 Linux ABI 宏内核", color: navy)]
]

// 4 · design highlights
#slide[
  #titlebar("01  ·  SYSTEM POSITIONING", "设计亮点：把复杂性收敛到可解释的内核路径", subtitle: "不仅实现接口，更统一对象、状态和资源生命周期")
  #v(0.18cm)
  #grid(columns: (1fr, 1fr, 1fr), gutter: 8pt,
    compact-panel("01  ·  Rust 所有权贯穿生命周期", [任务、地址空间、文件和 socket 以引用计数与显式所有权管理；fork / exec / exit / close 的共享、复制和回收边界清晰。], color: blue, fill: pale-blue),
    compact-panel("02  ·  统一任务状态机", [RUNNING → BLOCKED → WAKING → RUNNABLE；futex、pipe、socket、epoll、文件锁和定时器共享 park / wake。], color: teal, fill: pale-teal),
    compact-panel("03  ·  uaccess 快路径", [`copy_from_user` / `copy_to_user` 优先走对齐、单页和权限已知的快速复制；异常路径保留 EFAULT 语义。], color: orange, fill: pale-orange),
    compact-panel("04  ·  MailBox 核间通信", [TLB shootdown、任务唤醒和同步请求通过 MailBox 发布，由 IPI 通知目标 Hart 并收集 ACK。], color: red, fill: pale-red),
    compact-panel("05  ·  架构差异硬件层收敛", [RISC-V64 与 LoongArch64 的 trap、页表 / TLB、timer、IPI 和 VirtIO 由架构层适配，上层共享内核语义。], color: blue, fill: pale-blue),
    compact-panel("06  ·  正确性与性能并重", [先保证 Linux 语义、锁序和可唤醒路径，再通过页缓存、read_at、COW、调度和可观测性优化。], color: teal, fill: pale-teal),
  )
  #v(0.22cm)
  #align(center)[#tag("设计主线：薄入口 → 快路径 → MailBox / IPI 协议 → 内核对象 → 状态转换 → 资源回收", color: navy)]
]

// 5 · system introduction
#chapter-cover("02", "系统介绍", "从系统架构图出发，沿进程、调度、内存、信号、网络与驱动走完整实现路径", color: teal, fill: pale-teal)
#slide[
  #titlebar("02  ·  SYSTEM INTRODUCTION", "系统架构", subtitle: "五类内核组件由 Linux ABI 统一入口衔接，并通过 HAL 收敛架构与设备差异")
  #v(0.03cm)
  #grid(columns: (1fr, 68pt), column-gutter: 8pt, row-gutter: 2pt,
    arch-node([User Applications: glibc · musl · BusyBox · LTP · Rustc / Cargo], fill: arch-user, stroke: 0.6pt + blue, height: 20pt, size: 8.2pt),
    arch-label("User Space"),
    grid.cell(colspan: 2)[#arch-divider()],
    grid(columns: arch-cols, gutter: arch-gap,
      align(center)[#text(size: 8pt, fill: blue)[↓]], align(center)[#text(size: 8pt, fill: blue)[↓]],
      align(center)[#text(size: 8pt, fill: blue)[↓ syscall]], align(center)[#text(size: 8pt, fill: blue)[↓]], align(center)[#text(size: 8pt, fill: blue)[↓]],
    ), [],
    arch-node([Linux ABI / POSIX Syscalls Interface], fill: arch-sys, stroke: 0.6pt + teal, height: 20pt, size: 8.2pt),
    arch-label("Ya2yOS Kernel"),
    grid(columns: arch-cols, gutter: arch-gap,
      arch-node([Process Management], fill: arch-proc-head, stroke: 0.5pt + rgb("fca5a5"), height: 20pt),
      arch-node([Memory Management], fill: arch-mem-head, stroke: 0.5pt + rgb("fed7aa"), height: 20pt),
      arch-node([File Systems], fill: arch-fs-head, stroke: 0.5pt + rgb("fde047"), height: 20pt),
      arch-node([Device Management], fill: arch-dev-head, stroke: 0.5pt + rgb("86efac"), height: 20pt),
      arch-node([Network], fill: arch-net-head, stroke: 0.5pt + rgb("d8b4fe"), height: 20pt),
    ), arch-label("components"),
    grid(columns: arch-cols, gutter: arch-gap,
      arch-module(stack(spacing: 3pt, arch-node([Task Manager]), arch-node([Process Group]), arch-node([Scheduler / Load Balance], fill: rgb("ea3838"), stroke: none, text-color: white)), arch-proc-bg, 0.5pt + rgb("fca5a5")),
      arch-module(stack(spacing: 3pt, arch-node([MemorySet / VMA]), arch-node([COW / Page Fault]), arch-node([SLAB Allocator], fill: rgb("fdba74"), stroke: none)), arch-mem-bg, 0.5pt + rgb("fed7aa")),
      arch-module(stack(spacing: 3pt, arch-node([VFS], fill: rgb("f59e0b"), stroke: none), grid(columns: (1fr, 1fr), gutter: 3pt, arch-node([Ext4], height: 16pt), arch-node([ProcFS], height: 16pt)), grid(columns: (1fr, 1fr), gutter: 3pt, arch-node([Page Cache], height: 16pt), arch-node([Dentry Cache], height: 16pt))), arch-fs-bg, 0.5pt + rgb("fde047")),
      arch-module(stack(spacing: 3pt, arch-node([DevFS]), arch-node([Block / Char Device]), arch-node([VirtIO / IRQ])), arch-dev-bg, 0.5pt + rgb("86efac")),
      arch-module(stack(spacing: 3pt, grid(columns: (1fr, 1fr), gutter: 3pt, arch-node([TCP], height: 17pt), arch-node([UDP], height: 17pt)), arch-node([Unix Socket]), arch-node([smoltcp / NetDevice])), arch-net-bg, 0.5pt + rgb("d8b4fe")),
    ), arch-label("software support"),
    grid(columns: arch-cols, gutter: arch-gap,
      arch-node([PLIC / Timer], fill: arch-proc-head, stroke: 0.5pt + rgb("fca5a5")),
      arch-node([Frame Allocator], fill: arch-mem-head, stroke: 0.5pt + rgb("fed7aa")),
      grid(columns: (1fr, 1fr), gutter: 3pt, arch-node([MMIO], fill: arch-fs-head, stroke: 0.5pt + rgb("fde047")), arch-node([PCI], fill: arch-fs-head, stroke: 0.5pt + rgb("fde047"))),
      arch-node([Device Tree], fill: arch-dev-head, stroke: 0.5pt + rgb("86efac")),
      arch-node([VirtIO Net], fill: arch-net-head, stroke: 0.5pt + rgb("d8b4fe")),
    ), arch-label("hardware support"),
    arch-node([Hardware Abstraction Layer: trap · page table / TLB · timer · IPI · MailBox], fill: arch-sys, stroke: 0.6pt + teal, height: 20pt, size: 8pt), [],
    grid.cell(colspan: 2)[#arch-divider()],
    grid(columns: arch-cols, gutter: arch-gap,
      align(center)[#text(size: 8pt, fill: blue)[↓]], align(center)[#text(size: 8pt, fill: blue)[↓]], align(center)[#text(size: 8pt, fill: blue)[↓]], align(center)[#text(size: 8pt, fill: blue)[↓]], align(center)[#text(size: 8pt, fill: blue)[↓]],
    ), [],
    grid(columns: arch-cols, gutter: arch-gap,
      arch-node([RISC-V64], fill: arch-user, stroke: 0.5pt + blue), arch-node([LoongArch64], fill: arch-user, stroke: 0.5pt + teal), arch-node([RAM], fill: arch-user, stroke: 0.5pt + orange), arch-node([VirtIO Block / Net], fill: arch-user, stroke: 0.5pt + teal), arch-node([Serial / Interrupt], fill: arch-user, stroke: 0.5pt + red),
    ), arch-label("hardwares"),
  )
]

// 6 · process management
#slide[
  #titlebar("02  ·  SYSTEM INTRODUCTION", "进程管理：对象、资源与生命周期", subtitle: "以 ProcessControlBlock 和 TaskControlBlock 建立进程语义与线程执行边界")
  #v(0.22cm)
  #grid(columns: (1fr, 0.18fr, 1fr, 0.18fr, 1fr), gutter: 6pt,
    flow-step("fork / clone", "按 clone flags 创建或共享资源", color: blue, fill: pale-blue), text(size: 22pt, fill: blue)[→],
    flow-step("PCB + TCB", "进程资源 + 线程上下文", color: teal, fill: pale-teal), text(size: 22pt, fill: blue)[→],
    flow-step("exec / exit / wait", "替换、回收并向父进程报告", color: orange, fill: pale-orange),
  )
  #v(0.44cm)
  #grid(columns: (1fr, 1fr, 1fr), gutter: 11pt,
    panel("进程与线程分层", [`ProcessControlBlock` 持有地址空间、父子关系、信号与共享资源；`TaskControlBlock` 保存 TrapContext、调度状态和线程私有数据。], color: blue, fill: pale-blue),
    panel("资源共享语义", [clone flags 决定 `MemorySet`、FileTable、信号处理状态等资源是共享、复制还是新建；线程 affinity 保存在 TCB 而非进程全局。], color: teal, fill: pale-teal),
    panel("完整生命周期", [fork / clone 建立执行实体；execve 替换用户态映像；exit 清理 fd、VMA 与等待关系；waitpid 回收退出状态。], color: orange, fill: pale-orange),
  )
  #v(0.48cm)
  #align(center)[#tag("对象边界清晰：进程拥有资源，线程承载执行，退出路径负责成对回收", color: navy)]
]

// 7 · SMP scheduling
#slide[
  #titlebar("02  ·  SYSTEM INTRODUCTION", "多核调度：让可运行任务到达合适的 Hart", subtitle: "共享 CFS 就绪队列与线程 affinity 共同决定任务选择和远端唤醒")
  #v(0.22cm)
  #grid(columns: (1fr, 0.16fr, 1fr, 0.16fr, 1fr, 0.16fr, 1fr), gutter: 4pt,
    flow-step("READY", "任务入队", color: blue, fill: pale-blue), text(size: 20pt, fill: blue)[→],
    flow-step("affinity", "过滤不可运行 Hart", color: teal, fill: pale-teal), text(size: 20pt, fill: blue)[→],
    flow-step("all-hart CFS", "选择下一任务", color: orange, fill: pale-orange), text(size: 20pt, fill: blue)[→],
    flow-step("wake idle Hart", "IPI / 平台唤醒", color: red, fill: pale-red),
  )
  #v(0.43cm)
  #grid(columns: (1fr, 1fr, 1fr), gutter: 11pt,
    panel("共享就绪队列", [CFS 从同一 ready queue 选择任务；任务状态由原子字段或任务锁保护，避免队列选择与状态转换脱节。], color: blue, fill: pale-blue),
    panel("亲和性与唤醒", [TCB 保存 Linux 可见的 CPU affinity mask；调度器保留暂不满足 affinity 的任务，空闲目标 Hart 可由平台唤醒。], color: teal, fill: pale-teal),
    panel("当前边界", [已实现 online-Hart mask、affinity 过滤和 idle-Hart 唤醒；运行中迁移、per-CPU queue 与 work stealing 仍属于后续演进。], color: orange, fill: pale-orange),
  )
  #v(0.48cm)
  #align(center)[#tag("调度目标：可运行性正确优先，再通过拓扑与队列策略提升并行度", color: navy)]
]

// 8 · memory management
#slide[
  #titlebar("02  ·  SYSTEM INTRODUCTION", "内存管理：从 VMA 到跨核页表一致性", subtitle: "MemorySet 管理地址空间；缺页、COW 与 TLB shootdown 共同维护映射语义")
  #v(0.22cm)
  #grid(columns: (1fr, 0.16fr, 1fr, 0.16fr, 1fr, 0.16fr, 1fr), gutter: 4pt,
    flow-step("VMA", "mmap / munmap / mremap", color: blue, fill: pale-blue), text(size: 20pt, fill: blue)[→],
    flow-step("page fault", "按需分配与权限检查", color: teal, fill: pale-teal), text(size: 20pt, fill: blue)[→],
    flow-step("COW", "共享页写入时复制", color: orange, fill: pale-orange), text(size: 20pt, fill: blue)[→],
    flow-step("remote TLB", "IPI 与 ACK 收敛", color: red, fill: pale-red),
  )
  #v(0.4cm)
  #grid(columns: (1fr, 1fr), gutter: 13pt,
    panel("地址空间对象", [`MemorySet` 以 RwLock 包装 `MemorySetInner`；VMA、页表与映射生命周期集中管理，文件映射保留 backing inode。], color: blue, fill: pale-blue),
    panel("缺页与 COW", [文件页先在锁外获取，再在写锁阶段安装；私有可写映射在 fork 后保留 COW 语义，写入时才复制物理页。], color: teal, fill: pale-teal),
    panel("跨核一致性", [PTE 替换遵循 UPDATE_LOCK → MemorySet 写锁顺序；remote TLB 经 MailBox / IPI 收集 ACK，收敛前不释放旧帧。], color: orange, fill: pale-orange),
    panel("用户访问边界", [`copy_from_user` / `copy_to_user` 与页表检查共同保证 uaccess 失败返回 EFAULT，而不是越界访问内核内存。], color: red, fill: pale-red),
  )
]

// 9 · signals
#slide[
  #titlebar("02  ·  SYSTEM INTRODUCTION", "信号机制：从 pending 集合到用户态处理函数", subtitle: "信号投递、选择、用户栈 frame 构造与 rt_sigreturn 构成完整闭环")
  #v(0.22cm)
  #grid(columns: (1fr, 0.16fr, 1fr, 0.16fr, 1fr, 0.16fr, 1fr), gutter: 4pt,
    flow-step("kill / timer", "生成 SigInfo", color: blue, fill: pale-blue), text(size: 20pt, fill: blue)[→],
    flow-step("pending + mask", "选择可投递信号", color: teal, fill: pale-teal), text(size: 20pt, fill: blue)[→],
    flow-step("signal frame", "保存 UserContext", color: orange, fill: pale-orange), text(size: 20pt, fill: blue)[→],
    flow-step("rt_sigreturn", "校验并恢复现场", color: red, fill: pale-red),
  )
  #v(0.42cm)
  #grid(columns: (1fr, 1fr, 1fr), gutter: 11pt,
    panel("投递与唤醒", [信号写入目标线程或线程组的 pending 集；若任务处于可中断阻塞状态，投递路径将其置为可运行，以便及时消费。], color: blue, fill: pale-blue),
    panel("选择语义", [pending 模块在 trap 返回前选择未被 mask 的信号；标准信号重复到达时保留 pending 位与首次 `siginfo_t`，不无限排队。], color: teal, fill: pale-teal),
    panel("用户态 frame", [有 handler 时在用户栈构造 signal frame；`rt_sigreturn` 校验 frame magic 与布局后恢复寄存器、PC 和栈指针，非法 frame 返回 EINVAL。], color: orange, fill: pale-orange),
  )
  #v(0.48cm)
  #align(center)[#tag("信号既是异步通知，也是从任务状态、用户内存到 trap 返回路径的完整协议", color: navy)]
]

// 10 · network
#slide[
  #titlebar("02  ·  SYSTEM INTRODUCTION", "网络模块：socket 语义落到 smoltcp 与网卡收发", subtitle: "系统调用创建文件描述符；协议状态机与底层设备通过统一 poll / RX / TX 路径衔接")
  #v(0.22cm)
  #grid(columns: (1fr, 0.16fr, 1fr, 0.16fr, 1fr, 0.16fr, 1fr), gutter: 4pt,
    flow-step("socket fd", "TCP / UDP / Unix", color: blue, fill: pale-blue), text(size: 20pt, fill: blue)[→],
    flow-step("SocketSet", "连接与缓冲状态", color: teal, fill: pale-teal), text(size: 20pt, fill: blue)[→],
    flow-step("smoltcp service", "协议栈 poll", color: orange, fill: pale-orange), text(size: 20pt, fill: blue)[→],
    flow-step("Router / NIC", "RX / TX token", color: red, fill: pale-red),
  )
  #v(0.42cm)
  #grid(columns: (1fr, 1fr, 1fr), gutter: 11pt,
    panel("Linux socket 边界", [TCP、UDP 与 Unix socket 以 fd 方式暴露给用户态；poll waiter 将连接、可读写和错误状态转化为任务可观察的就绪事件。], color: blue, fill: pale-blue),
    panel("协议栈封装", [`Service` 持有 smoltcp Interface；`SocketSet` 管理协议 socket，监听表在 SYN 到达时参与连接建立。], color: teal, fill: pale-teal),
    panel("设备适配", [`Router` 实现 smoltcp 的 Device 抽象；RX / TX token 对接 loopback、Ethernet 与 VirtIO-net，实现从数据包到任务唤醒的路径。], color: orange, fill: pale-orange),
  )
  #v(0.48cm)
  #align(center)[#tag("socket readiness → poll waiter → packet RX / TX → wake task", color: navy)]
]

// 11 · device drivers
#slide[
  #titlebar("02  ·  SYSTEM INTRODUCTION", "设备驱动：把架构差异收敛为块设备与网卡接口", subtitle: "RISC-V64 与 LoongArch64 选择不同总线传输方式，上层文件系统和协议栈共享设备语义")
  #v(0.22cm)
  #grid(columns: (1fr, 0.16fr, 1fr, 0.16fr, 1fr, 0.16fr, 1fr), gutter: 4pt,
    flow-step("platform bus", "MMIO / PCI", color: blue, fill: pale-blue), text(size: 20pt, fill: blue)[→],
    flow-step("VirtIO transport", "地址转换与队列", color: teal, fill: pale-teal), text(size: 20pt, fill: blue)[→],
    flow-step("block / net impl", "请求与缓冲区", color: orange, fill: pale-orange), text(size: 20pt, fill: blue)[→],
    flow-step("kernel consumer", "VFS / smoltcp", color: red, fill: pale-red),
  )
  #v(0.42cm)
  #grid(columns: (1fr, 1fr, 1fr), gutter: 11pt,
    panel("双架构适配", [RISC-V64 使用 VirtIO-MMIO 绑定；LoongArch64 通过 PCI transport 发现和绑定 VirtIO 能力。上层仅依赖 `BlockDeviceImpl` / `NetDeviceImpl`。], color: blue, fill: pale-blue),
    panel("块设备路径", [块设备实现封装 VirtIO 队列与同步访问；文件系统经统一块接口提交读写，不依赖具体 MMIO 寄存器或 PCI 配置空间。], color: teal, fill: pale-teal),
    panel("网卡路径", [VirtIO-net 预置接收描述符并维护发送缓冲；队列访问串行化，随后将收发事件交给网络模块的 RX / TX 路径。], color: orange, fill: pale-orange),
  )
  #v(0.48cm)
  #align(center)[#tag("平台发现与传输层可变，块设备 / 网卡接口稳定，上层内核语义保持共享", color: navy)]
]

// 10 · achievements overview
#chapter-cover("03", "当前成果", "测例证据 · 任务调度 · 多核异构 · 文件操作与锁", color: orange, fill: pale-orange)
#slide[
  #titlebar("03  ·  CURRENT RESULTS", "当前成果：从接口实现走向可复核证据", subtitle: "通过事实、定向观测和未完成边界分开陈述")
  #v(0.32cm)
  #grid(columns: (1fr, 1fr, 1fr, 1fr), gutter: 9pt,
    stat("16", "access02 核心断言 TPASS", color: blue),
    stat("566", "splice07 passed / 0 failed", color: teal),
    stat("156", "fanotify01 passed / 0 failed", color: orange),
    stat("1.50x", "定向 axbuild 观测", color: red),
  )
  #v(0.4cm)
  #grid(columns: (1fr, 1fr), gutter: 13pt,
    panel("已验证的回归", [`abort01 2/0/0`、`open14 3/0/0`、`fcntl01/11/13/33/34` 已有通过记录；`splice07` 另有 25 项 skipped，按 summary 原样保留。], color: blue, fill: pale-blue),
    panel("证据边界", [`fcntl DUPFD/pipe size` 只到 ext4 mount 失败，未进入 LTP；BuildStorm 只有 `ok=true elapsed_s=X` 才是完整 446 crate 成功判据，当前不宣称全量成绩。], color: red, fill: pale-red),
  )
]

// 11 · scheduling and heterogeneous
#slide[
  #titlebar("03  ·  CURRENT RESULTS", "任务调度与多核异构：让拓扑真实可见")
  #v(0.28cm)
  #grid(columns: (1fr, 1fr), gutter: 14pt,
    panel("已落地", [在线 HART mask；共享 all-hart CFS ready queue；affinity 过滤和 idle-hart 唤醒；ppoll 无事件不忙让出；全局 timer/futex/task timeout 每 10ms 由单一 hart 维护。], color: blue, fill: pale-blue),
    panel("双架构事实", [RISC-V 与 LoongArch64 release 构建通过；评测配置已启动 8 Hart，并出现并行编译队列；动态 `/proc/uptime` 为测例提供真实 guest 时间。], color: teal, fill: pale-teal),
    panel("仍待完成", [运行中任务迁移、通用 reschedule IPI、per-CPU queue / work stealing 和完整同配置 A/B 仍属于后续工作。], color: orange, fill: pale-orange),
    panel("优化口径", [累计 wait/hold、Hart 计数用于定位结构性变化，不等同 wall-clock；性能结论仍以同配置、成功收尾的 elapsed_s 为准。], color: red, fill: pale-red),
  )
  #v(0.55cm)
  #align(center)[#tag("已完成：SMP 可见性与唤醒路径　|　未完成：运行中迁移与负载均衡", color: navy)]
]

// 12 · file locks
#slide[
  #titlebar("03  ·  CURRENT RESULTS", "文件操作与锁：区分用户语义和内部并发边界")
  #v(0.27cm)
  #grid(columns: (1fr, 1fr), gutter: 14pt,
    panel("用户可见锁：已落地", [POSIX record lock 支持区间拆分/合并、`F_GETLK`、`F_SETLKW` 阻塞、死锁检测和 close/exit 释放；OFD lock 按 open file description 区分 owner。对应 `fcntl11/14/34` 等已有通过记录。], color: blue, fill: pale-blue),
    panel("内部 EXT4：保守正确", [lwext4 C API / bcache 仍由可等待的挂载级排他准入保护；已收敛只读 `read_at`、64 KiB 跨页合并、干净文件页缓存复用和重复 path/metadata 工作。], color: teal, fill: pale-teal),
    panel("为什么不直接并发化", [allocator、目录、journal 等 lwext4 资源尚无完整 Linux 等价锁序证明；撤回 shared admission，避免以死锁或一致性换吞吐。], color: orange, fill: pale-orange),
    panel("当前结论", [“用户可见文件锁已完成多项语义回归”不等于“内部 EXT4 已完成资源级细粒度并发锁”；后者必须经过压力、e2fsck、LTP 和同配置 A/B。], color: red, fill: pale-red),
  )
]

// 13 · buildstorm evidence
#slide[
  #titlebar("03  ·  CURRENT RESULTS", "BuildStorm：定向优化有效，但全量成绩仍待闭环")
  #v(0.33cm)
  #grid(columns: (1fr, 0.24fr, 1fr), gutter: 8pt,
    block(fill: pale-orange, stroke: 1pt + orange, radius: 5pt, inset: 14pt)[#align(center)[#text(size: 17pt, weight: "bold", fill: orange)[定向观测]#v(0.35em)#text(size: 32pt, weight: "bold", fill: navy)[12 min → 8 min]#v(0.28em)#text(size: 11.5pt, fill: muted)[尾部 axbuild；约缩短 33.3%，约 1.50x；不是 446 crate 全量成绩。]]],
    align(center + horizon)[#text(size: 27pt, weight: "bold", fill: blue)[→]],
    block(fill: pale-red, stroke: 1pt + red, radius: 5pt, inset: 14pt)[#align(center)[#text(size: 17pt, weight: "bold", fill: red)[正式判据]#v(0.35em)#text(size: 16pt, weight: "bold", fill: navy)[BUILDSTORM_COMPILE]#v(0.28em)#text(size: 11.5pt, fill: muted)[必须出现 `ok=true elapsed_s=X`；当前材料没有该完整收尾标记。]]],
  )
  #v(0.55cm)
  #grid(columns: (1fr, 1fr, 1fr), gutter: 11pt,
    panel("正确性先行", [工具链路径、fork/exec/mmap/COW、rename、epoll、journal 等阻断项先修复，避免把故障伪装成性能。], color: blue, fill: pale-blue),
    panel("优化主线", [页缓存复用、read_at、稀疏写合并、EXT4 入口收敛、all-hart 调度和动态 uptime 共同缩短热路径。], color: teal, fill: pale-teal),
    panel("证据纪律", [`BUILDSTORM_BEGIN`、`Building N/446`、累计 wait/hold 或 timeout 都不能单独作为成功或加速证明。], color: orange, fill: pale-orange),
  )
]

// 14 · roadmap
#chapter-cover("04", "发展规划", "完整验证闭环 · 任务迁移 · 资源级并发 · 双架构回归", color: red, fill: pale-red)
#slide[
  #titlebar("04  ·  ROADMAP", "发展规划：从兼容面走向更深的系统语义")
  #v(0.3cm)
  #grid(columns: (1fr, 1fr), gutter: 14pt,
    panel("01  ·  完整测例闭环", [完成官方 BuildStorm 446 crate 的 `BUILDSTORM_COMPILE ok=true elapsed_s=X`；在相同镜像、架构、内存、SMP 和冷/热缓存条件下完成 A/B。], color: blue, fill: pale-blue),
    panel("02  ·  调度与异构", [补齐运行中任务迁移、通用 reschedule IPI、per-CPU queue / work stealing；建立 RISC-V / LoongArch64 自动化压力矩阵。], color: teal, fill: pale-teal),
    panel("03  ·  文件系统并发", [为 allocator、目录、journal 等资源建立可证明的锁序和生命周期模型，再评估从挂载级准入向资源级细粒度并发演进。], color: orange, fill: pale-orange),
    panel("04  ·  验证护栏", [压力测试、e2fsck、文件系统 LTP、跨架构回归和失败可重试路径全部纳入发布前证据链。], color: red, fill: pale-red),
  )
  #v(0.6cm)
  #align(center)[#tag("目标：让“已经跑通”进一步成为“语义正确、并发可控、证据可复核”", color: navy)]
]

// 15 · closing
#slide[
  #align(center + horizon)[
    #text(size: 48pt, weight: "bold", fill: navy)[Ya2yOS]
    #v(0.18cm)
    #text(size: 28pt, weight: "bold", fill: blue)[内核设计与工程实践]
    #v(0.35cm)
    #text(size: 16pt, fill: muted)[系统定位 · 系统介绍 · 当前成果 · 发展规划]
    #v(0.55cm)
    #text(size: 13pt, fill: muted)[谢谢！]
  ]
]
