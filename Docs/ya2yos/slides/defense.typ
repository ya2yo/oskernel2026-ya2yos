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
#let chapter(no, title, sub, color: blue) = slide[
  #align(center + horizon)[
    #text(size: 82pt, weight: "bold", fill: color)[#no]
    #v(-0.1em)
    #text(size: 31pt, weight: "bold", fill: navy)[#title]
    #v(0.25em)
    #text(size: 14pt, fill: muted)[#sub]
  ]
]
#let flow-step(label, desc, color: blue, fill: pale-blue) = block(fill: fill, stroke: 0.8pt + color, radius: 4pt, inset: (x: 10pt, y: 9pt))[
  #text(size: 14pt, weight: "bold", fill: color)[#label]
  #v(0.13em)
  #text(size: 11pt, fill: muted)[#desc]
]

// 1 · cover
#slide[
  #block(fill: navy, width: 100%, height: 86%, inset: (x: 1.15cm, y: 0.85cm))[
    #align(center + horizon)[
      #text(size: 11pt, weight: "bold", fill: rgb("7dd3c7"))[YA2YOS  ·  KERNEL ENGINEERING]
      #v(0.35cm)
      #text(size: 53pt, weight: "bold", fill: white)[Ya2yOS]
      #v(0.08cm)
      #text(size: 30pt, weight: "bold", fill: rgb("7dd3c7"))[内核设计与工程实践]
      #v(0.38cm)
      #text(size: 19pt, fill: rgb("d7e6ef"))[Rust 宏内核实现总览]
      #v(0.68cm)
      #grid(columns: (auto, auto, auto), gutter: 10pt,
        tag("RISC-V64", color: teal), tag("LoongArch64", color: orange), tag("Linux ABI", color: blue),
      )
      #v(0.72cm)
      #text(size: 12pt, fill: rgb("b8cad5"))[参赛队员：饶晓杰　　指导老师：杨磊]
      #v(0.13cm)
      #text(size: 10pt, fill: rgb("8fa7b5"))[syscall · task · scheduler · mm · VFS · signal · net · arch]
    ]
  ]
]

// 2 · kernel map
#slide[
  #titlebar("01  ·  KERNEL MAP", "内核实现总览", subtitle: "从 trap 入口开始，所有用户态能力都落到具体子系统")
  #v(0.2cm)
  #grid(columns: (1fr, 0.2fr, 1fr, 0.2fr, 1fr), gutter: 7pt,
    flow-step("arch / trap", "异常入口 · 上下文 · uaccess", color: blue, fill: pale-blue), text(size: 22pt, fill: blue)[→], flow-step("syscall", "参数解码 · fd · errno", color: teal, fill: pale-teal), text(size: 22pt, fill: blue)[→], flow-step("task / mm / fs / net", "内核语义与资源生命周期", color: orange, fill: pale-orange),
  )
  #v(0.45cm)
  #grid(columns: (1fr, 1fr, 1fr, 1fr), gutter: 10pt,
    panel("task", [`TaskControlBlock`、进程树、线程组、fork / clone / exec / wait。], color: blue, fill: pale-blue),
    panel("mm", [`MemorySet`、VMA、缺页、COW、mmap / mremap、remote TLB。], color: teal, fill: pale-teal),
    panel("fs", [`VFS`、`Ext4Inode`、page cache、fd table、pipe、epoll。], color: orange, fill: pale-orange),
    panel("net", [`SocketSet`、TCP / UDP、loopback、VirtIO-net、poll wait。], color: red, fill: pale-red),
  )
]

// 3 · syscall and trap
#slide[
  #titlebar("02  ·  SYSCALL & TRAP", "用户态请求如何进入内核")
  #v(0.28cm)
  #grid(columns: (1fr, 0.18fr, 1fr, 0.18fr, 1fr), gutter: 6pt,
    flow-step("用户寄存器", "syscall number + args", color: blue, fill: pale-blue), text(size: 22pt, fill: blue)[→], flow-step("trap handler", "保存 TrapContext，切换内核栈", color: teal, fill: pale-teal), text(size: 22pt, fill: blue)[→], flow-step("syscall dispatcher", "匹配 Syscall 枚举并分发", color: orange, fill: pale-orange),
  )
  #v(0.55cm)
  #grid(columns: (1fr, 1fr), gutter: 15pt,
    panel("uaccess 边界", [`copy_from_user` / `copy_to_user`、用户地址检查、跨页复制和 EFAULT 错误路径集中在 `mm/uaccess.rs` 与架构汇编实现。], color: blue, fill: pale-blue),
    panel("系统调用覆盖", [`fs`、`mm`、`process`、`signal`、`time`、`net`、`sys` 等分组；fd 参数先做表查找，再交给文件对象或子系统。], color: teal, fill: pale-teal),
  )
  #v(0.5cm)
  #align(center)[#tag("handler 保持薄：解码、拷贝、查 fd、委托语义、传播 errno", color: navy)]
]

// 4 · task
#slide[
  #titlebar("03  ·  TASK & PROCESS", "进程与线程：由 TaskControlBlock 统一承载")
  #v(0.25cm)
  #grid(columns: (1fr, 0.2fr, 1fr, 0.2fr, 1fr), gutter: 7pt,
    flow-step("ProcessControlBlock", "地址空间、父子关系、信号与资源", color: blue, fill: pale-blue), text(size: 22pt, fill: blue)[→], flow-step("TaskControlBlock", "trap context、调度状态、线程私有数据", color: teal, fill: pale-teal), text(size: 22pt, fill: blue)[→], flow-step("FileTable / MemorySet", "共享或复制资源，按 clone flags 决定", color: orange, fill: pale-orange),
  )
  #v(0.53cm)
  #grid(columns: (1fr, 1fr, 1fr), gutter: 12pt,
    panel("创建与退出", [fork / clone 建立子任务；execve 替换地址空间；exit 清理 fd、VMA、锁上下文；waitpid 回收状态。], color: blue, fill: pale-blue),
    panel("阻塞与唤醒", [futex、pipe、socket、epoll 和文件锁统一进入 park / wake；退出路径处理等待者和资源引用。], color: teal, fill: pale-teal),
    panel("资源生命周期", [线程组、父子关系、共享 MemorySet、fd 引用和 signal manager 分离管理，避免 exec / exit 泄漏。], color: orange, fill: pale-orange),
  )
]

// 5 · scheduler
#slide[
  #titlebar("04  ·  SCHEDULER", "调度器：任务状态与多核唤醒")
  #v(0.3cm)
  #align(center)[
    #grid(columns: (1fr, 0.2fr, 1fr, 0.2fr, 1fr, 0.2fr, 1fr), gutter: 4pt,
      flow-step("RUNNING", "当前 hart 执行", color: blue, fill: pale-blue), text(size: 21pt, fill: blue)[→], flow-step("BLOCKED", "futex / I/O / timer", color: teal, fill: pale-teal), text(size: 21pt, fill: blue)[→], flow-step("WAKING", "加入 ready queue", color: orange, fill: pale-orange), text(size: 21pt, fill: blue)[→], flow-step("RUNNABLE", "affinity 过滤后选择", color: red, fill: pale-red),
    )
  ]
  #v(0.5cm)
  #grid(columns: (1fr, 1fr), gutter: 15pt,
    panel("选择任务", [共享 all-hart ready queue；按 affinity 过滤；CFS 保存 vruntime，RR 保留时间片路径；任务切换更新当前 hart 上下文。], color: blue, fill: pale-blue),
    panel("唤醒远端", [目标任务若属于空闲 hart，通过 IPI 通知；futex、文件等待、设备等待都回到统一 wake 路径。], color: teal, fill: pale-teal),
  )
  #v(0.5cm)
  #align(center)[#text(size: 15pt, weight: "bold", fill: navy)[timer tick → 检查可运行任务 → 保存上下文 → 选择下一个 task → restore / return]]
]

// 6 · memory
#slide[
  #titlebar("05  ·  MEMORY MANAGEMENT", "MemorySet：地址空间、VMA 与页表更新")
  #v(0.22cm)
  #grid(columns: (1fr, 0.2fr, 1fr, 0.2fr, 1fr), gutter: 7pt,
    flow-step("VMA", "mmap / munmap / mremap", color: blue, fill: pale-blue), text(size: 22pt, fill: blue)[→], flow-step("page fault", "按需分配、文件页预取、权限检查", color: teal, fill: pale-teal), text(size: 22pt, fill: blue)[→], flow-step("PTE", "map / unmap / COW 写保护", color: orange, fill: pale-orange),
  )
  #v(0.48cm)
  #grid(columns: (1fr, 1fr), gutter: 15pt,
    panel("已实现的映射路径", [`MemorySet` 管理用户 / 内核地址空间；匿名映射、文件映射、MAP_STACK、MAP_GROWSDOWN、共享内存和动态扩展由 `mmap_ops` / `area_ops` 处理。], color: blue, fill: pale-blue),
    panel("COW 与回收", [fork 后私有页写保护；缺页时复制或独占页就地升级；旧页框通过 frame tracker 延迟到引用安全后回收。], color: teal, fill: pale-teal),
  )
  #v(0.42cm)
  #align(center)[#tag("PTE 更新 → remote_tlb mailbox / IPI → ACK 收敛 → 旧映射回收", color: navy)]
]

// 7 · elf
#slide[
  #titlebar("06  ·  ELF LOADER", "从 execve 到动态程序开始运行")
  #v(0.28cm)
  #grid(columns: (1fr, 0.17fr, 1fr, 0.17fr, 1fr, 0.17fr, 1fr), gutter: 4pt,
    flow-step("open", "VFS 打开 ELF", color: blue, fill: pale-blue), text(size: 20pt, fill: blue)[→], flow-step("parse", "ELF / PT_LOAD / PT_INTERP", color: teal, fill: pale-teal), text(size: 20pt, fill: blue)[→], flow-step("map", "MemorySet 建立 VMA", color: orange, fill: pale-orange), text(size: 20pt, fill: blue)[→], flow-step("enter", "用户栈 + aux + trampoline", color: red, fill: pale-red),
  )
  #v(0.5cm)
  #grid(columns: (1fr, 1fr), gutter: 15pt,
    panel("文件页按需读取", [`elf_loader.rs` 解析 program header；文件映射保留 backing inode；缺页时从 page cache / ext4 获取页面，避免一次性读入全部镜像。], color: blue, fill: pale-blue),
    panel("解释器路径", [`PT_INTERP` 通过 VFS 原路径打开；缺失或读取失败保留对应 errno；动态链接、库搜索和重定位交给用户态 loader。], color: teal, fill: pale-teal),
  )
  #v(0.45cm)
  #align(center)[#text(size: 14pt, weight: "bold", fill: navy)[execve 的结果是“新的地址空间 + 可继续触发缺页的文件映射”，而不是一份内核堆里的完整副本。]]
]

// 8 · vfs
#slide[
  #titlebar("07  ·  VFS & EXT4", "文件系统：从路径名到真实块设备")
  #v(0.25cm)
  #grid(columns: (1fr, 0.18fr, 1fr, 0.18fr, 1fr), gutter: 6pt,
    flow-step("path lookup", "mount / dentry / inode", color: blue, fill: pale-blue), text(size: 22pt, fill: blue)[→], flow-step("VFS file", "File / OpenFile / fd table", color: teal, fill: pale-teal), text(size: 22pt, fill: blue)[→], flow-step("Ext4Inode", "lwext4 C API + VirtIO block", color: orange, fill: pale-orange),
  )
  #v(0.48cm)
  #grid(columns: (1fr, 1fr, 1fr), gutter: 11pt,
    panel("目录与命名空间", [openat、`*at` 系列、symlink、rename、mount / umount、cwd 和 proc 路径由 VFS 层统一解析。], color: blue, fill: pale-blue),
    panel("缓存与一致性", [`PageCache` 复用普通文件页；fstat 元数据按 epoch 失效；write / truncate / rename 主动失效相关缓存。], color: teal, fill: pale-teal),
    panel("设备落地", [lwext4 保留 journal、bcache callback 与必要串行域；VirtIO block 提供真实扇区读写。], color: orange, fill: pale-orange),
  )
]

// 9 · fd
#slide[
  #titlebar("08  ·  FD OBJECTS", "统一 fd 模型承载不同内核资源")
  #v(0.23cm)
  #align(center)[#text(size: 16pt, weight: "bold", fill: navy)[FileTable → File / OSFile → poll / read / write / ioctl / close]]
  #v(0.4cm)
  #grid(columns: (1fr, 1fr, 1fr, 1fr), gutter: 10pt,
    panel("管道与事件", [pipe / FIFO ring buffer；eventfd；splice；阻塞读写通过 wait queue 和 waker 重新调度。], color: blue, fill: pale-blue),
    panel("就绪通知", [epoll registry、fd waiter、timeout future；文件、socket、设备统一返回可观察事件。], color: teal, fill: pale-teal),
    panel("进程间机制", [signalfd、System V message queue、memfd_secret、pagemap、timerfd 等实现为可引用文件对象。], color: orange, fill: pale-orange),
    panel("控制与属性", [fcntl dup / locks / lease；ioctl；权限、flags、FD_CLOEXEC 和 close / exit 清理。], color: red, fill: pale-red),
  )
  #v(0.55cm)
  #align(center)[#tag("同一 fd 生命周期：创建 → 安装 FileTable → 引用计数 → 阻塞 / 唤醒 → close 清理", color: navy)]
]

// 10 · signal timer
#slide[
  #titlebar("09  ·  SIGNAL, FUTEX & TIMER", "同步与异步机制共享任务生命周期")
  #v(0.3cm)
  #grid(columns: (1fr, 1fr, 1fr), gutter: 13pt,
    panel("SignalManager", [标准信号与实时信号进入 pending 集；delivery 选择可唤醒任务；rt_sigreturn 校验 frame 并恢复上下文。], color: blue, fill: pale-blue),
    panel("Futex", [按虚拟地址建立 waiter 队列；wait / wake / requeue 处理 bitset、超时和远端唤醒；退出时清理引用。], color: teal, fill: pale-teal),
    panel("GlobalTimer", [单一 hart 维护共享 timer / futex 状态；当前线程 interval timer 仍在本 hart 投递；/proc/uptime 动态生成。], color: orange, fill: pale-orange),
  )
  #v(0.62cm)
  #align(center)[
    #grid(columns: (auto, 0.18fr, auto, 0.18fr, auto), gutter: 7pt,
      tag("pending", color: blue), text(size: 20pt, fill: blue)[→], tag("park", color: teal), text(size: 20pt, fill: blue)[→], tag("delivery / wake", color: orange),
    )
  ]
  #v(0.4cm)
  #align(center)[#text(size: 14pt, fill: muted)[信号、futex 和 I/O 等待都通过 TaskControlBlock 的状态转换回到调度器。]]
]

// 11 · net
#slide[
  #titlebar("10  ·  NETWORK", "smoltcp、socket 与设备收发路径")
  #v(0.25cm)
  #grid(columns: (1fr, 0.18fr, 1fr, 0.18fr, 1fr), gutter: 6pt,
    flow-step("sys_socket", "fd + socket object", color: blue, fill: pale-blue), text(size: 22pt, fill: blue)[→], flow-step("SocketSet", "TCP / UDP 状态机", color: teal, fill: pale-teal), text(size: 22pt, fill: blue)[→], flow-step("Device", "loopback / VirtIO-net RX/TX", color: orange, fill: pale-orange),
  )
  #v(0.52cm)
  #grid(columns: (1fr, 1fr), gutter: 15pt,
    panel("协议栈封装", [`net/service.rs` 持有 smoltcp Interface 与 SocketSet；TCP、UDP、IPv4 / IPv6 和监听表分别封装用户态 socket 语义。], color: blue, fill: pale-blue),
    panel("设备适配", [`Router` 实现 `smoltcp::phy::Device`；RxToken / TxToken 对接网卡缓冲；loopback、Ethernet、VirtIO-net 复用同一协议入口。], color: teal, fill: pale-teal),
  )
  #v(0.45cm)
  #align(center)[#tag("socket readiness → poll / epoll waiter → packet RX/TX → wake task", color: navy)]
]

// 12 · arch and drivers
#slide[
  #titlebar("11  ·  ARCH & DRIVERS", "架构差异收敛在硬件抽象层")
  #v(0.24cm)
  #grid(columns: (1fr, 1fr), gutter: 15pt,
    block(fill: pale-blue, radius: 4pt, inset: 14pt)[#text(size: 21pt, weight: "bold", fill: blue)[RISC-V64]#v(0.25em)#text(size: 12.5pt, fill: ink)[entry.asm / trap / uaccess；Sv39 page table；TLB shootdown；VirtIO-MMIO；timer 与 IPI。]],
    block(fill: pale-orange, radius: 4pt, inset: 14pt)[#text(size: 21pt, weight: "bold", fill: orange)[LoongArch64]#v(0.25em)#text(size: 12.5pt, fill: ink)[entry.asm / trap / uaccess；PTE / TLB；IBar；PCI VirtIO；timer 与 IPI。]],
  )
  #v(0.48cm)
  #grid(columns: (1fr, 1fr, 1fr), gutter: 11pt,
    panel("上下文", [`TaskContext` / `TrapContext` 保存寄存器；switch.S 完成任务切换；用户返回路径恢复架构寄存器。], color: blue, fill: pale-blue),
    panel("设备", [VirtIO block / net、串口、平台中断和板级驱动各自实现 Device trait，上层只看到 block / net / irq 接口。], color: teal, fill: pale-teal),
    panel("共享上层", [task、mm、fs、net、syscall 目录不依赖具体页表指令和启动寄存器。], color: orange, fill: pale-orange),
  )
]

// 13 · implementation index
#slide[
  #titlebar("12  ·  IMPLEMENTATION INDEX", "已经落地的内核能力")
  #v(0.2cm)
  #grid(columns: (1.15fr, 1fr, 1fr), gutter: 10pt,
    panel("进程与执行", [fork / clone / execve / wait；线程组；动态 ELF；signal / sigreturn；futex；seccomp。], color: blue, fill: pale-blue),
    panel("地址空间", [mmap / munmap / mremap；缺页；COW；共享内存；MAP_STACK / GROWSDOWN；remote TLB。], color: teal, fill: pale-teal),
    panel("文件与存储", [VFS；ext4；mount；symlink / rename；page cache；pipe；epoll；fcntl locks；loop / tmp file。], color: orange, fill: pale-orange),
    panel("网络与设备", [TCP / UDP / Unix socket；IPv4 / IPv6；loopback；VirtIO block / net；串口；中断控制器。], color: red, fill: pale-red),
    panel("系统接口", [time、rusage、proc 文件、poll / ppoll、inotify / fanotify、mqueue、eventfd / timerfd。], color: blue, fill: pale-blue),
    panel("实现原则", [Linux ABI 兼容；uaccess 显式检查；锁顺序固定；阻塞点可唤醒；资源释放与引用计数成对。], color: teal, fill: pale-teal),
  )
  #v(0.6cm)
  #align(center)[#text(size: 18pt, weight: "bold", fill: navy)[Ya2yOS 的实现主线：每一个用户态动作，都有对应的内核对象、状态转换和资源回收路径。]]
]

// 14 · closing
#slide[
  #align(center + horizon)[
    #text(size: 48pt, weight: "bold", fill: navy)[Ya2yOS]
    #v(0.18cm)
    #text(size: 28pt, weight: "bold", fill: blue)[内核实现情况]
    #v(0.35cm)
    #text(size: 16pt, fill: muted)[Rust 宏内核 · Linux ABI · RISC-V64 / LoongArch64]
    #v(0.55cm)
    #text(size: 13pt, fill: muted)[谢谢！]
  ]
]
