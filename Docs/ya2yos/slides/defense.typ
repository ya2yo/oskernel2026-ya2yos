// Ya2yOS 答辩演示稿。构建：typst compile --root . defense.typ ya2yos-defense.pdf
// PowerPoint 导出：python3 export_pptx.py
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
#let compact-panel(title, body, color: blue, fill: pale-blue, height: auto) = {
  let content = [
    #text(size: 13pt, weight: "bold", fill: color)[#title]
    #v(0.12em)
    #text(size: 10.5pt, fill: ink)[#body]
  ]
  if height == auto {
    block(fill: fill, stroke: 0.7pt + border, radius: 4pt, inset: 8pt)[#content]
  } else {
    block(fill: fill, stroke: 0.7pt + border, radius: 4pt, inset: 8pt, height: height)[#content]
  }
}
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
    panel("03  ·  关键增量", [多核运行、CFS 调度、StarryOS 网络移植、uaccess、文件锁、syscall 扩展与模块重构。], color: orange, fill: pale-orange),
    panel("04  ·  发展规划", [扩展文件系统类型、提升文件 I/O、持续丰富网络模块，并完成开发板实机运行与验证。], color: red, fill: pale-red),
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
    compact-panel("01  ·  Rust 所有权贯穿生命周期", [任务、地址空间、文件和 socket 以引用计数与显式所有权管理；fork / exec / exit / close 的共享、复制和回收边界清晰。], color: blue, fill: pale-blue, height: 80pt),
    compact-panel("02  ·  统一任务状态机", [RUNNING → BLOCKED → WAKING → RUNNABLE；futex、pipe、socket、epoll、文件锁和定时器共享 park / wake。], color: teal, fill: pale-teal, height: 80pt),
    compact-panel("03  ·  uaccess 快路径", [`copy_from_user` / `copy_to_user` 优先走对齐、单页和权限已知的快速复制；异常路径保留 EFAULT 语义。], color: orange, fill: pale-orange, height: 80pt),
    compact-panel("04  ·  MailBox 核间通信", [TLB shootdown、任务唤醒和同步请求通过 MailBox 发布，由 IPI 通知目标 Hart 并收集 ACK。], color: red, fill: pale-red, height: 80pt),
    compact-panel("05  ·  架构差异硬件层收敛", [RISC-V64 与 LoongArch64 的 trap、页表 / TLB、timer、IPI 和 VirtIO 由架构层适配，上层共享内核语义。], color: blue, fill: pale-blue, height: 80pt),
    compact-panel("06  ·  正确性与性能并重", [先保证 Linux 语义、锁序和可唤醒路径，再通过页缓存、read_at、COW、调度和可观测性优化。], color: teal, fill: pale-teal, height: 80pt),
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

// 12 · incremental work overview
#chapter-cover("03", "关键增量工作", "并发运行 · Linux 语义 · 网络移植 · 工程结构", color: orange, fill: pale-orange)
#slide[
  #titlebar("03  ·  KEY INCREMENTAL WORK", "关键增量工作：围绕并发、兼容与工程化", subtitle: "七项工作共同把可启动的内核基础推进为能承载真实用户态路径的系统")
  #v(0.18cm)
  #grid(columns: (1fr, 1fr, 1fr, 1fr), gutter: 8pt,
    compact-panel("01  ·  多核运行", [维护 online-Hart 状态、核间唤醒和共享地址空间的跨核可见性。], color: blue, fill: pale-blue),
    compact-panel("02  ·  CFS 调度", [共享 ready queue、线程 affinity 过滤与 idle-Hart 唤醒形成闭环。], color: teal, fill: pale-teal),
    compact-panel("03  ·  StarryOS 网络", [移植网络模块与 `NetDriverOps` 抽象，接入 smoltcp 与 VirtIO-net。], color: orange, fill: pale-orange),
    compact-panel("04  ·  uaccess", [`copy_from_user` / `copy_to_user` 提供架构快路径、跨页安全路径和 EFAULT 语义。], color: red, fill: pale-red),
    compact-panel("05  ·  文件锁", [细化 POSIX/OFD/lease 锁、阻塞等待、死锁检测和 close/exit 回收。], color: teal, fill: pale-teal),
    compact-panel("06  ·  syscall", [扩展 task、mm、fs、net、同步与 I/O multiplexing 的 Linux 接口。], color: orange, fill: pale-orange),
    compact-panel("07  ·  模块重构", [以职责拆分 signal、MemorySet、syscall、网络服务和驱动，提升内聚度。], color: blue, fill: pale-blue),
    grid.cell[],
  )
  #v(0.32cm)
  #align(center)[#tag("增量主线：先让多核与资源正确运行，再扩展 ABI 覆盖并收敛模块边界", color: navy)]
]

// 13 · SMP and CFS
#slide[
  #titlebar("03  ·  KEY INCREMENTAL WORK", "实现多核运行与 CFS 调度", subtitle: "从任务入队到目标 Hart 执行，在线拓扑、affinity、队列和唤醒路径必须保持一致")
  #v(0.22cm)
  #grid(columns: (1fr, 0.16fr, 1fr, 0.16fr, 1fr, 0.16fr, 1fr), gutter: 4pt,
    flow-step("READY", "任务入共享队列", color: blue, fill: pale-blue), text(size: 20pt, fill: blue)[→],
    flow-step("affinity", "过滤不可运行 Hart", color: teal, fill: pale-teal), text(size: 20pt, fill: blue)[→],
    flow-step("all-hart CFS", "选择下一任务", color: orange, fill: pale-orange), text(size: 20pt, fill: blue)[→],
    flow-step("wake_hart", "唤醒空闲目标核", color: red, fill: pale-red),
  )
  #v(0.42cm)
  #grid(columns: (1fr, 1fr, 1fr), gutter: 11pt,
    panel("多核运行基础", [维护 online-Hart mask；共享地址空间的 PTE 更新通过 remote TLB MailBox / IPI 收集 ACK，避免其他 Hart 使用失效映射。], color: blue, fill: pale-blue),
    panel("CFS 可运行性", [CFS 在共享 ready queue 中选择任务；TCB 持有 Linux 可见的 CPU affinity，暂不满足条件的任务保留在队列而不是错误执行。], color: teal, fill: pale-teal),
    panel("唤醒与边界", [任务变为 ready 后按 affinity 通知可运行 Hart；空闲核可被平台唤醒。运行中迁移、per-CPU queue 与 work stealing 仍是后续工作。], color: orange, fill: pale-orange),
  )
  #v(0.48cm)
  #align(center)[#tag("SMP 的完成标准不是“启动多个核”，而是任务放置、TLB 一致性与唤醒路径均正确", color: navy)]
]

// 14 · StarryOS network port
#slide[
  #titlebar("03  ·  KEY INCREMENTAL WORK", "移植 StarryOS 网络模块并接入 Ya2yOS", subtitle: "保留 StarryOS 的网络抽象优势，在 Ya2yOS 的任务、fd 和双架构驱动边界中重新落地")
  #v(0.22cm)
  #grid(columns: (1fr, 0.16fr, 1fr, 0.16fr, 1fr, 0.16fr, 1fr), gutter: 4pt,
    flow-step("StarryOS net", "NetDriverOps / NetBuf", color: blue, fill: pale-blue), text(size: 20pt, fill: blue)[→],
    flow-step("Ya2yOS adapter", "fd / poll / waiter", color: teal, fill: pale-teal), text(size: 20pt, fill: blue)[→],
    flow-step("smoltcp service", "SocketSet / interface", color: orange, fill: pale-orange), text(size: 20pt, fill: blue)[→],
    flow-step("VirtIO-net", "RX / TX / recycle", color: red, fill: pale-red),
  )
  #v(0.42cm)
  #grid(columns: (1fr, 1fr, 1fr), gutter: 11pt,
    panel("移植范围", [`os/src/net` 与 `drivers/net` 明确来自 StarryOS；移植的核心是 `NetDriverOps`、`NetBuf`、socket 封装和 smoltcp 服务组织。], color: blue, fill: pale-blue),
    panel("Ya2yOS 集成", [TCP、UDP 和 Unix socket 以文件描述符暴露；poll waiter 将协议状态转为任务就绪事件，监听表参与 SYN 到达后的连接建立。], color: teal, fill: pale-teal),
    panel("设备资源协议", [VirtIO-net 为 RX 描述符预置缓冲，TX 从共享池取用；协议栈消费后通过 recycle 接口归还队列，避免收发缓冲生命周期不清。], color: orange, fill: pale-orange),
  )
  #v(0.48cm)
  #align(center)[#tag("移植不是复制目录：必须把网络对象、任务唤醒和设备缓冲区所有权接入本内核语义", color: navy)]
]

// 15 · uaccess and filesystem locking
#slide[
  #titlebar("03  ·  KEY INCREMENTAL WORK", "uaccess 双路径与文件系统锁细化", subtitle: "用户指针访问与文件并发控制都必须同时满足常见路径效率和异常路径的 Linux 语义")
  #v(0.22cm)
  #grid(columns: (1fr, 0.16fr, 1fr, 0.16fr, 1fr, 0.16fr, 1fr), gutter: 4pt,
    flow-step("user pointer", "范围与溢出检查", color: blue, fill: pale-blue), text(size: 20pt, fill: blue)[→],
    flow-step("fast path", "架构 uaccess 复制", color: teal, fill: pale-teal), text(size: 20pt, fill: blue)[→],
    flow-step("safe path", "跨页 / COW / 缺页", color: orange, fill: pale-orange), text(size: 20pt, fill: blue)[→],
    flow-step("Linux result", "复制成功或 EFAULT", color: red, fill: pale-red),
  )
  #v(0.4cm)
  #grid(columns: (1fr, 1fr), gutter: 13pt,
    panel("copy_from_user / copy_to_user 快路径", [`translate.rs` 在长度、用户范围和当前地址空间满足条件时调用架构 `copy_from_user` / `copy_to_user`；短复制避免逐页通用翻译开销。], color: blue, fill: pale-blue),
    panel("安全与异常路径", [跨页、延迟分配、文件页和 COW 回到通用映射路径；uaccess scope 与 trap fixup 处理内核态访问异常，最终向 syscall 传播 EFAULT。], color: teal, fill: pale-teal),
    panel("细化用户可见文件锁", [`file_lock` 分离 POSIX record lock、BSD flock、OFD lock 与 lease；`F_SETLKW` 支持等待图死锁检测、waker 注册和 close/exit 自动释放。], color: orange, fill: pale-orange),
    panel("保守的内部锁边界", [lwext4 C API / bcache 的内部并发仍采用可等待的准入保护；先保证一致性与锁序，再逐步评估资源级细粒度并发。], color: red, fill: pale-red),
  )
]

// 16 · syscall enrichment and refactoring
#slide[
  #titlebar("03  ·  KEY INCREMENTAL WORK", "丰富 syscall，并重构模块边界", subtitle: "扩大 ABI 覆盖面时，入口保持薄，核心语义与高风险状态机回到各自子系统")
  #v(0.22cm)
  #grid(columns: (1fr, 0.16fr, 1fr, 0.16fr, 1fr, 0.16fr, 1fr), gutter: 4pt,
    flow-step("Linux ABI", "number + arguments", color: blue, fill: pale-blue), text(size: 20pt, fill: blue)[→],
    flow-step("thin handler", "uaccess / fd / errno", color: teal, fill: pale-teal), text(size: 20pt, fill: blue)[→],
    flow-step("domain module", "task / mm / fs / net", color: orange, fill: pale-orange), text(size: 20pt, fill: blue)[→],
    flow-step("kernel object", "state / lock / lifetime", color: red, fill: pale-red),
  )
  #v(0.42cm)
  #grid(columns: (1fr, 1fr, 1fr), gutter: 11pt,
    panel("syscall 功能扩展", [覆盖 clone3、rseq、key、IPC、xattr、inotify / fanotify、socket 消息收发、epoll / poll、timerfd 等更多真实用户态路径。], color: blue, fill: pale-blue),
    panel("按领域拆分入口", [`syscall` 划分 task、mm、fs、net、sync、io_mpx、ipc、time、sys 等目录；handler 负责解码、复制、查表和 errno，语义不堆积在大分发函数。], color: teal, fill: pale-teal),
    panel("提高内聚度", [signal 拆为 delivery/pending/frame/timer；MemorySet 拆分 handle、pagefault、fork_clone 等；网络服务、驱动和文件锁也按资源职责拆分。], color: orange, fill: pale-orange),
  )
  #v(0.48cm)
  #align(center)[#tag("工程目标：扩展接口不制造巨型模块，让每条 ABI 路径都落到可维护的对象、锁和生命周期", color: navy)]
]

// 14 · roadmap
#chapter-cover("04", "发展规划", "文件系统扩展 · I/O 优化 · 网络完善 · 开发板实机运行", color: red, fill: pale-red)
#slide[
  #titlebar("04  ·  ROADMAP", "发展规划：从可运行走向更完整的系统能力", subtitle: "围绕存储、网络与实机部署持续扩展，所有优化以语义正确和可复核验证为前提")
  #v(0.3cm)
  #grid(columns: (1fr, 1fr), gutter: 14pt,
    panel("01  ·  支持更多文件系统", [在统一 VFS、dentry/inode 和 mount 语义下接入更多文件系统类型；完善不同文件系统的路径解析、权限、元数据和挂载参数兼容性。], color: blue, fill: pale-blue),
    panel("02  ·  提高文件系统 I/O 速率", [围绕 PageCache 命中、顺序/批量读写、跨页复制和块设备提交路径减少重复工作；使用同配置、成功收尾的基准和文件系统回归验证优化收益。], color: teal, fill: pale-teal),
    panel("03  ·  继续丰富网络模块", [补齐 socket 选项、协议语义、路由与设备事件处理；持续加强 TCP / UDP / Unix socket 与 poll / epoll、VirtIO-net 之间的一致性。], color: orange, fill: pale-orange),
    panel("04  ·  在开发板上成功运行", [完成实机启动、内存与中断初始化、块设备/网卡驱动和串口观测；在开发板上跑通用户态程序、文件 I/O、网络通信与压力回归。], color: red, fill: pale-red),
  )
  #v(0.6cm)
  #align(center)[#tag("路线：VFS 扩展 → I/O 优化 → 网络完善 → 实机验证，逐步形成可用且可验证的操作系统", color: navy)]
]

// 15 · closing
#slide[
  #align(center + horizon)[
    #text(size: 48pt, weight: "bold", fill: navy)[Ya2yOS]
    #v(0.18cm)
    #text(size: 28pt, weight: "bold", fill: blue)[内核设计与工程实践]
    #v(0.35cm)
    #text(size: 25pt, fill: muted)[谢谢！]
  ]
]
