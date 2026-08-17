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
#let panel(title, body, color: blue, fill: pale-blue, height: auto) = {
  let content = [
    #text(size: 15pt, weight: "bold", fill: color)[#title]
    #v(0.22em)
    #text(size: 12.2pt, fill: ink)[#body]
  ]
  if height == auto {
    block(fill: fill, stroke: 0.7pt + border, radius: 4pt, inset: 11pt)[#content]
  } else {
    block(fill: fill, stroke: 0.7pt + border, radius: 4pt, inset: 11pt, height: height)[#content]
  }
}
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
#let compact-panel(title, body, color: blue, fill: pale-blue, height: auto, body-size: 10.5pt) = {
  let content = [
    #text(size: 13pt, weight: "bold", fill: color)[#title]
    #v(0.12em)
    #text(size: body-size, fill: ink)[#body]
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
      #text(size: 12pt, fill: muted)[参赛队员：饶晓杰\ 指导老师：杨磊]
    ]
  ]
]

// 2 · positioning
#slide[
  #titlebar("01  ·  SYSTEM POSITIONING", "系统定位")
  #v(1.15cm)
  #align(center + horizon)[
    #text(size: 25pt, weight: "bold", fill: navy)[Ya2yOS 是一个使用 Rust 语言编写，面向 Linux ABI 的宏内核]
  ]
  #v(0.7cm)
  #align(center + horizon)[#line(length: 58%, stroke: 1.2pt + teal)]
  #v(0.62cm)
  #align(center + horizon)[
    #text(size: 18pt, fill: muted)[项目的主要目标是保证 Linux ABI 语义正确，提高性能，正确运行 Linux 用户态程序]
  ]
]

// 3 · design highlights
#slide[
  #titlebar("01  ·  SYSTEM POSITIONING", "设计亮点")
  #v(0.18cm)
  #grid(columns: (1fr, 1fr, 1fr), gutter: 8pt,
    compact-panel("01  ·  进程管理", [#stack(spacing: 0.25em,
      [- 实现简化的 CFS 多核调度。],
      [- `Process` 和 `Task` 分离进程资源与调度任务。],
      [- `clone` 按 `clone flags` 决定资源共享或复制。],
      [- `execve/exit/wait` 完成映像替换、资源回收与父子状态传递。],
    )], color: blue, fill: pale-blue, height: 136pt, body-size: 12pt),
    compact-panel("02  ·  内存管理", [#stack(spacing: 0.2em,
      [- 实现 `uaccess` 快速内存访问路径。],
      [- 支持写时复制（COW）与懒分配。],
      [- 借助 RemoteTLB 机制实现跨核页表一致性同步。],
    )], color: teal, fill: pale-teal, height: 136pt, body-size: 12pt),
    compact-panel("03  ·  架构管理", [#stack(spacing: 0.2em,
      [- 支持 RISC-V64 与 LoongArch64 双架构。],
      [- 通过架构抽象层隔离 trap、页表等平台差异。],
    )], color: orange, fill: pale-orange, height: 136pt, body-size: 12pt),
  )
  #v(0.12cm)
  #grid(columns: (1fr, 1fr), gutter: 8pt,
    compact-panel("04  ·  文件系统", [#stack(spacing: 0.25em,
      [- 支持细粒度锁，竞争时阻塞唤醒任务。],
      [- 支持 EXT4 文件系统，并通过 VFS 统一 inode / superblock 接口。],
    )], color: red, fill: pale-red, height: 112pt, body-size: 12pt),
    compact-panel("05  ·  设备驱动", [#stack(spacing: 0.2em,
      [- 支持 VirtIO 块设备与网卡驱动。],
      [- 通过 CMA 管理 DMA，统一块设备和网卡接口。],
      [- RISC-V64 使用 MMIO，LoongArch64 使用 PCI transport。],
    )], color: blue, fill: pale-blue, height: 112pt, body-size: 12pt),
  )
]

// 4 · system architecture
#slide[
  #titlebar("02  ·  SYSTEM INTRODUCTION", "系统架构")
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

// Detailed subsystem slides are retained in source but disabled for the five-minute defense.
#if false [
// Process, scheduler, memory, signal, filesystem, and device-driver details.
#slide[
  #titlebar("02  ·  SYSTEM INTRODUCTION", "进程管理：对象、资源与生命周期", subtitle: "以 Process 和 TaskControlBlock 建立进程资源与线程执行边界")
  #v(0.22cm)
  #grid(columns: (1fr, 0.18fr, 1fr, 0.18fr, 1fr), gutter: 6pt,
    flow-step("fork / clone", "按 clone flags 创建或共享资源", color: blue, fill: pale-blue), text(size: 22pt, fill: blue)[→],
    flow-step("PCB + TCB", "进程资源 + 线程上下文", color: teal, fill: pale-teal), text(size: 22pt, fill: blue)[→],
    flow-step("exec / exit / wait", "替换、回收并向父进程报告", color: orange, fill: pale-orange),
  )
  #v(0.44cm)
  #grid(columns: (1fr, 1fr, 1fr), gutter: 11pt,
    panel("进程与线程分层", [`Process` 持有地址空间、父子关系、信号与共享资源；`TaskControlBlock` 保存 TrapContext、调度状态和线程私有数据。], color: blue, fill: pale-blue),
    panel("资源共享语义", [clone flags 决定 `MemorySet`、FileTable、信号处理状态等资源是共享、复制还是新建；线程 affinity 保存在 TCB 而非进程全局。], color: teal, fill: pale-teal),
    panel("主要生命周期路径", [fork / clone 建立执行实体；execve 替换用户态映像；exit 与 waitpid 负责退出资源和状态的传递与回收。], color: orange, fill: pale-orange),
  )
]

// 7 · SMP scheduling
#slide[
  #titlebar("02  ·  SYSTEM INTRODUCTION", "多核调度：让可运行任务到达合适的 Hart", subtitle: "共享 ready queue 与线程 affinity 共同决定任务选择和空闲核唤醒")
  #v(0.22cm)
  #grid(columns: (1fr, 0.16fr, 1fr, 0.16fr, 1fr, 0.16fr, 1fr), gutter: 4pt,
    flow-step("READY", "任务入队", color: blue, fill: pale-blue), text(size: 20pt, fill: blue)[→],
    flow-step("affinity", "过滤不可运行 Hart", color: teal, fill: pale-teal), text(size: 20pt, fill: blue)[→],
    flow-step("all-hart CFS", "选择下一任务", color: orange, fill: pale-orange), text(size: 20pt, fill: blue)[→],
    flow-step("wake idle Hart", "IPI / 平台唤醒", color: red, fill: pale-red),
  )
  #v(0.43cm)
  #grid(columns: (1fr, 1fr, 1fr), gutter: 11pt,
    panel("共享就绪队列", [简化 CFS 使用共享 ready queue，并根据 vruntime 和权重选择任务；任务状态由原子字段或任务锁保护。], color: blue, fill: pale-blue),
    panel("亲和性与唤醒", [TCB 保存 Linux 可见的 CPU affinity mask；调度器保留暂不满足 affinity 的任务，空闲目标 Hart 可由平台唤醒。], color: teal, fill: pale-teal),
    panel("当前边界", [已实现 online-Hart mask、affinity 过滤和 idle-Hart 唤醒；运行中迁移、per-CPU queue、work stealing 和完整负载均衡仍属于后续演进。], color: orange, fill: pale-orange),
  )
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
    panel("缺页与 COW", [file-backed 缺页先在地址空间锁外准备文件页，再在写锁阶段安装；匿名页和 COW 分别走按需分配与写保护复制路径。], color: teal, fill: pale-teal),
    panel("跨核一致性", [相关 PTE 更新遵循 UPDATE_LOCK → MemorySet 写锁顺序；remote TLB 通过专用 MailBox / IPI 收集 ACK，收敛前不释放旧帧。], color: orange, fill: pale-orange),
    panel("用户访问边界", [`copy_from_user` / `copy_to_user` 与页表检查共同保证 uaccess 失败返回 EFAULT，而不是越界访问内核内存。], color: red, fill: pale-red),
  )
]

// 9 · signals
#slide[
  #titlebar("02  ·  SYSTEM INTRODUCTION", "信号机制：常规 handler 路径", subtitle: "pending、mask、signal frame 与 rt_sigreturn 组成已覆盖路径")
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
]

// 10 · file systems
#slide[
  #titlebar("02  ·  SYSTEM INTRODUCTION", "文件系统：VFS 统一路径与 inode 语义", subtitle: "路径解析、缓存和 EXT4 / ProcFS 适配共同承接 Linux 文件 I/O")
  #v(0.22cm)
  #grid(columns: (1fr, 0.16fr, 1fr, 0.16fr, 1fr, 0.16fr, 1fr), gutter: 4pt,
    flow-step("path syscall", "open / stat / mount", color: blue, fill: pale-blue), text(size: 20pt, fill: blue)[→],
    flow-step("VFS", "path / dentry / inode", color: teal, fill: pale-teal), text(size: 20pt, fill: blue)[→],
    flow-step("filesystem", "EXT4 / ProcFS", color: orange, fill: pale-orange), text(size: 20pt, fill: blue)[→],
    flow-step("cache + block", "Page Cache / VirtIO", color: red, fill: pale-red),
  )
  #v(0.42cm)
  #grid(columns: (1fr, 1fr, 1fr), gutter: 11pt,
    panel("VFS 对象模型", [`FileTable` 持有文件描述符；路径解析通过 mount、dentry 和 inode 找到对象，目录、普通文件与符号链接共享统一接口。], color: blue, fill: pale-blue),
    panel("文件系统适配", [`Ext4Inode` 将 lwext4 暴露为 VFS `Inode`；根 superblock 提供 EXT4 挂载、statfs、目录遍历和同步，`/proc` 由内核文件接口承接。], color: teal, fill: pale-teal),
    panel("缓存与 I/O 边界", [Page Cache、dentry cache 和 inode 状态减少重复访问；块设备通过统一接口提交 VirtIO I/O，文件页缺页与写回再与内存管理衔接。], color: orange, fill: pale-orange),
  )
]

// 11 · device drivers
#slide[
  #titlebar("02  ·  SYSTEM INTRODUCTION", "设备适配：把主要差异收敛到块设备与网卡接口", subtitle: "当前以 QEMU、同步块 I/O 和网络 poll 路径为主要验证范围")
  #v(0.22cm)
  #grid(columns: (1fr, 0.16fr, 1fr, 0.16fr, 1fr, 0.16fr, 1fr), gutter: 4pt,
    flow-step("platform bus", "MMIO / PCI", color: blue, fill: pale-blue), text(size: 20pt, fill: blue)[→],
    flow-step("VirtIO transport", "地址转换与队列", color: teal, fill: pale-teal), text(size: 20pt, fill: blue)[→],
    flow-step("block / net impl", "请求与缓冲区", color: orange, fill: pale-orange), text(size: 20pt, fill: blue)[→],
    flow-step("kernel consumer", "VFS / smoltcp", color: red, fill: pale-red),
  )
  #v(0.42cm)
  #grid(columns: (1fr, 1fr, 1fr), gutter: 11pt,
    panel("双架构适配", [RISC-V64 主要使用 VirtIO-MMIO；LoongArch64 通过 PCI transport 适配 VirtIO。上层依赖 `BlockDeviceImpl` / `NetDeviceImpl`，但不同设备与 IRQ 完成语义仍有边界。], color: blue, fill: pale-blue, height: 126pt),
    panel("块设备路径", [块设备封装 VirtIO 队列并以同步访问为主；文件系统经统一块接口提交读写，当前重点是 QEMU 和已有回归路径。], color: teal, fill: pale-teal, height: 126pt),
    panel("网卡路径", [VirtIO-net 维护 RX 描述符和 TX 缓冲，网络服务主要通过 poll_interfaces() 推进；完整 IRQ enable/disable/complete 与实板网络验证属于后续工作。], color: orange, fill: pale-orange, height: 126pt),
  )
]

]

// 5 · incremental work overview
#slide[
  #titlebar("03  ·  KEY INCREMENTAL WORK", "关键增量")
  #v(0.3cm)
  #grid(columns: (1fr, 1fr, 1fr), gutter: 10pt,
    compact-panel("01  ·  SMP 一致性", [简化 CFS、共享 ready queue、线程 affinity 与空闲核唤醒；RemoteTLB 以 MailBox、IPI、ACK 维护跨核页表一致性。], color: blue, fill: pale-blue, height: 92pt),
    compact-panel("02  ·  内存与文件并发", [`uaccess` 为常见短复制提供架构快路径；COW 与懒分配减少不必要的物理页复制；EXT4 使用可等待的细粒度锁。], color: teal, fill: pale-teal, height: 92pt),
    compact-panel("03  ·  网络与 ABI", [适配 StarryOS 网络抽象并接入 smoltcp 与 VirtIO-net；syscall 按 task、mm、fs、net 等领域拆分，能力以回归结果为准。], color: orange, fill: pale-orange, height: 92pt),
  )
]

// 6 · SMP scheduling and RemoteTLB
#slide[
  #titlebar("03  ·  KEY INCREMENTAL WORK", "简化 CFS 与多核一致性")
  #v(0.22cm)
  #grid(columns: (1fr, 0.16fr, 1fr, 0.16fr, 1fr, 0.16fr, 1fr), gutter: 4pt,
    flow-step("READY", "任务入共享队列", color: blue, fill: pale-blue), text(size: 20pt, fill: blue)[→],
    flow-step("affinity", "过滤不可运行 Hart", color: teal, fill: pale-teal), text(size: 20pt, fill: blue)[→],
    flow-step("all-hart CFS", "选择下一任务", color: orange, fill: pale-orange), text(size: 20pt, fill: blue)[→],
    flow-step("wake_hart", "唤醒空闲目标核", color: red, fill: pale-red),
  )
  #v(0.42cm)
  #grid(columns: (1fr, 1fr, 1fr), gutter: 11pt,
    panel("多核运行基础", [维护 online-Hart mask；相关 PTE 更新通过专用 remote TLB MailBox / IPI 收集 ACK，保证目标 Hart 完成本地失效。], color: blue, fill: pale-blue),
    panel("简化 CFS 可运行性", [共享 ready queue 按 vruntime 和权重选择任务；TCB 持有线程 affinity，暂不满足条件的任务保留在队列而不是错误执行。], color: teal, fill: pale-teal),
    panel("唤醒与边界", [任务变为 Ready 后按 affinity 通知可运行 Hart；空闲核可被平台唤醒。work stealing 和完整负载均衡仍是后续工作。], color: orange, fill: pale-orange),
  )
]

// Network and syscall implementation details remain available in source.
#if false [
// StarryOS network port
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
    panel("移植范围", [`os/src/net` 与 `drivers/net` 保留并适配 StarryOS 的 `NetDriverOps`、`NetBuf`、socket 封装和 smoltcp 服务组织。], color: blue, fill: pale-blue),
    panel("Ya2yOS 集成边界", [TCP、UDP 和 Unix socket 提供主要 fd 路径；poll waiter 将已实现的协议状态转为任务就绪事件，网络服务当前以 poll 为主。], color: teal, fill: pale-teal),
    panel("设备资源协议", [VirtIO-net 为 RX 描述符预置缓冲，TX 从共享池取用；协议栈消费后通过 recycle 归还队列，完整中断和实板网络验证仍在推进。], color: orange, fill: pale-orange),
  )
]

]

// 7 · uaccess and filesystem locking
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
    panel("uaccess 快路径边界", [`translate.rs` 仅在当前活动地址空间、用户范围合法且长度满足限制时调用架构复制；其他场景回退逐页安全路径并传播 EFAULT。], color: blue, fill: pale-blue, height: 93pt),
    panel("安全与异常路径", [跨页、延迟分配、文件页和 COW 回到通用映射路径；uaccess scope 与 trap fixup 处理内核态访问异常，最终向 syscall 传播 EFAULT。], color: teal, fill: pale-teal),
    panel("细化用户可见文件锁", [`file_lock` 提供 POSIX、OFD、BSD flock 与 lease 的主要接口；当前以基本语义、F_SETLKW 等定向等待路径和 close/exit 回收为主，底层锁表仍是简化实现。], color: orange, fill: pale-orange),
    panel("保守的内部锁边界", [lwext4 C API / bcache 的内部并发仍采用可等待的准入保护；先保证一致性与锁序，再逐步评估资源级细粒度并发。], color: red, fill: pale-red, height: 93pt),
  )
]

// Syscall implementation details remain available in source.
#if false [
// Syscall enrichment and refactoring
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
    panel("syscall 功能扩展", [接入 clone3、rseq、epoll / poll 等入口，并按当前测试范围实现部分常用语义；key、inotify / fanotify、timerfd 等接口需按具体实现和验证结果区分。], color: blue, fill: pale-blue),
    panel("按领域拆分入口", [`syscall` 划分 task、mm、fs、net、sync、io_mpx、ipc、time、sys 等目录；handler 负责解码、复制、查表和 errno，语义不堆积在大分发函数。], color: teal, fill: pale-teal),
    panel("提高内聚度", [signal 拆为 delivery/pending/frame/timer；MemorySet 拆分 handle、pagefault、fork_clone 等；网络服务、驱动和文件锁也按资源职责拆分。], color: orange, fill: pale-orange),
  )
]

]

// 8 · roadmap
#slide[
  #titlebar("04  ·  ROADMAP", "发展规划", subtitle: "以可追溯的构建、启动和回归结果推进后续工作")
  #v(0.36cm)
  #grid(columns: (1fr, 1fr, 1fr), gutter: 10pt,
    panel("01  ·  VFS 与 I/O", [扩展文件系统类型，围绕 Page Cache、批量读写和块设备路径提升文件 I/O。], color: blue, fill: pale-blue, height: 105pt),
    panel("02  ·  网络与实机", [补齐网络协议和设备事件路径，完成开发板启动、块设备、网卡与压力回归。], color: teal, fill: pale-teal, height: 105pt),
  )
]

// 9 · closing
#slide[
  #align(center + horizon)[
    #text(size: 48pt, weight: "bold", fill: navy)[Ya2yOS]
    #v(0.18cm)
    #text(size: 28pt, weight: "bold", fill: blue)[内核设计与工程实践]
    #v(0.35cm)
    #text(size: 25pt, fill: muted)[谢谢！]
  ]
]
