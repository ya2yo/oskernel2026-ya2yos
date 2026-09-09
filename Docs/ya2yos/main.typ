// Ya2yOS 内核设计文档（Typst 入口）
// 对外发布建议：typst compile --pdf-standard a-2u main.typ ya2yos-kernel-design.pdf

#let doc-version = "0.7"
#let doc-date = datetime(year: 2026, month: 9, day: 9)
#let source-snapshot = "HEAD abecd9e654b（2026-08-22）"
#let ink = rgb("161616")
#let muted = rgb("555555")
#let line = rgb("9a9a9a")
#let paper-gray = rgb("f4f4f4")

// Font roles. The requested Songti/Heiti/Kaiti and Computer Modern families
// are not installed on every build host, so each role keeps a deterministic
// available fallback with complete Chinese coverage.
#let body-font = ("Libertinus Serif", "WenQuanYi Zen Hei")
#let heading-font = ("WenQuanYi Zen Hei", "Libertinus Serif")
#let note-font = ("WenQuanYi Zen Hei", "Libertinus Serif")
#let code-font = ("DejaVu Sans Mono", "WenQuanYi Zen Hei Mono")
#let cover-latin-font = "New Computer Modern"

#let body-size = 10.5pt
#let heading-1-size = 20pt
#let heading-2-size = 15pt
#let heading-3-size = 12.5pt
#let code-size = 9.2pt
#let note-size = 9.5pt
#let caption-size = 8.6pt

#set document(
  title: "Ya2yOS 内核设计文档",
  author: "饶晓杰",
  date: doc-date,
  keywords: ("Ya2yOS", "Rust", "kernel", "RISC-V", "LoongArch", "CFS", "scheduler"),
)
#set page(
  paper: "a4",
  margin: (top: 2.54cm, bottom: 2.3cm, x: 2.54cm),
  header: align(right, text(font: body-font, size: 8pt, fill: muted)[Ya2yOS Kernel Design · #doc-version]),
  footer: context align(center, text(font: body-font, size: 8pt, fill: muted)[#counter(page).display("1")]),
  numbering: "1",
)
#set text(font: body-font, size: body-size, fill: ink, lang: "zh")
#set par(justify: true, leading: 0.72em, first-line-indent: 2em, spacing: 0.65em)
#set heading(numbering: (..nums) => {
  if nums.len() == 1 {
    numbering("第一章", nums.at(0))
  } else {
    numbering("1.1", ..nums)
  }
})
#show heading: set text(font: heading-font, fill: rgb("000000"), weight: "bold", lang: "zh")
#show heading: it => [#it #h(2em)]
#show heading.where(level: 1): it => [
  #pagebreak(weak: true)
  #set block(above: 0.8em, below: 1.1em)
  #align(center)[#text(font: heading-font, size: heading-1-size, weight: "bold")[#it]]
]
#show heading.where(level: 2): set text(font: heading-font, size: heading-2-size, weight: "bold")
#show heading.where(level: 2): set block(above: 1.1em, below: 0.45em)
#show heading.where(level: 3): set text(font: heading-font, size: heading-3-size, weight: "bold")
#show heading.where(level: 3): set block(above: 0.8em, below: 0.3em)
#show raw: set text(font: code-font, size: code-size, lang: "en")
#set raw(block: true, lang: "en")

#show raw.where(block: true): it => {
  set block(above: 0.55em, below: 0.65em, inset: (x: 0.6em, y: 0.4em), fill: rgb("e6e6e6"))
  pad(x: 1.3em, it)
}


#show link: set text(fill: ink)
#set table(stroke: line, inset: 6pt)
#show figure.caption: set text(font: body-font, size: caption-size, fill: muted)
#set figure(supplement: [图])
#set list(indent: 1.4em, body-indent: 0.5em, spacing: 0.25em)
#set enum(indent: 1.4em, body-indent: 0.5em, spacing: 0.25em)

#let callout(title, body) = block(
  inset: (x: 1.8em, y: 0.65em),
  radius: 3pt,
  fill: paper-gray,
  stroke: (top: 0.45pt + line, bottom: 0.45pt + line, left: 1.2pt + ink),
  {
    set text(font: note-font, size: note-size, style: "italic")
    set par(justify: true, leading: 0.72em, first-line-indent: 0em, spacing: 0.25em)
    [*#title* #body]
  },
)

#let source(path) = text(font: note-font, size: 8.7pt, style: "italic", fill: muted)[实现追溯：`#path`]

#align(center)[
  #image("../img/华南理工大学.png")
  #v(3.6cm)
  #text(font: cover-latin-font, size: 29pt, weight: "bold")[Ya2yOS]
  #v(0.7cm)
  #text(font: heading-font, size: 20pt, weight: "bold")[内核设计文档]
  #v(1.5cm)
  #text(size: 20pt)[参赛队员：饶晓杰\ 指导老师：杨磊]
  #v(0.5cm)
  #text(font: "Libertinus Serif", size: 10.5pt)[Rust · RISC-V 64 · LoongArch64]
  #v(2.7cm)
  #text(size: 9.5pt)[版本：#doc-version]
  #v(0.25cm)
  #text(size: 9.5pt)[代码快照：#source-snapshot]
  #v(0.25cm)
  #text(size: 9.5pt)[发布日期：#doc-date.display("[year]-[month]-[day]")]
]

#pagebreak()
#align(center)[
  #text(font: body-font, size: 16pt, weight: "bold")[摘要]
]

Ya2yOS 是一个使用 Rust 语言实现、面向 Linux 用户态兼容的实验性操作系统内核，当前支持 RISC-V 64 与 LoongArch64 QEMU 平台。本文档从可复核的实现视角阐述内核的启动与异常入口、地址空间和页面生命周期、进程线程与编译期可选的 CFS/RR 调度、信号、Linux 风格系统调用与 VFS、网络栈和 VirtIO 设备接入，并给出模块边界、关键不变量、验证约定和已知边界。

#v(0.5em)
*关键词*：操作系统内核；Rust；Linux ABI；CFS；进程调度；虚拟内存；VFS；RISC-V；LoongArch


#v(1em)
#text(font: heading-font, size: 15pt, weight: "bold")[版本与阅读约定]

#table(
  columns: (5.2em, 1fr),
  table.header([*项目*], [*说明*]),
  [文档版本], [#doc-version],
  [适用范围], [Ya2yOS 当前工作树的内核实现],
  [主体源码], [`os/src/`；构建入口为仓库根目录 `Makefile`],
  [术语约定], [代码标识符、Linux ABI 名称和路径保持原文；其他叙述使用中文],
  [可追溯性], [各章给出关键目录；末章提供源码—章节索引与外部参考文献],
)

#pagebreak()
#outline(title: [目录], depth: 2)
#pagebreak()

#include "diagrams.typ"
#include "chapters/01-overview.typ"
#include "chapters/02-boot-arch.typ"
#include "chapters/03-task.typ"
#include "chapters/04-memory.typ"
#include "chapters/05-signal.typ"
#include "chapters/06-network.typ"
#include "chapters/07-devices.typ"
#include "chapters/08-filesystem.typ"
#include "chapters/09-lwext4.typ"
#include "chapters/10-ai-usage.typ"
#include "chapters/11-conclusion.typ"

#pagebreak()
= 实现追溯与参考资料

== 源码—章节索引

#table(
  columns: (1fr, 1.5fr, 4.8em),
  table.header([*主题*], [*主要实现位置*], [*本文位置*]),
  [内核入口与初始化], [`os/src/main.rs`], [第 2 章],
  [架构实现], [`os/src/arch/riscv64/`、`os/src/arch/loongarch64/`], [第 2 章],
  [进程、调度与 futex], [`os/src/task/`、`os/src/task/scheduler/`、`os/src/timer/`], [第 3 章],
  [页表、VMA 与用户复制], [`os/src/mm/`、`os/src/sync/remote_tlb.rs`], [第 4 章],
  [信号动作、pending、frame 与 trampoline], [`os/src/signal/`、`os/src/syscall/signal.rs`、`os/src/arch/*/qemu/`], [第 5 章],
  [syscall ABI 与实现分发], [`os/src/syscall/`（fs/mm/task/net/ipc/io_mpx/sys/sync）], [第 3--8 章],
  [socket 和协议栈封装], [`os/src/net/`、`os/src/drivers/net/`], [第 6--7 章],
  [VirtIO、IRQ 与平台设备], [`os/src/drivers/virtio/`、`os/src/arch/irq/`], [第 7 章],
  [VFS、ext4、proc、pipe 与挂载], [`os/src/fs/`、`os/src/drivers/disk.rs`], [第 8 章],
  [lwext4 磁盘文件系统引擎], [`crates/lwext4_rust/`、`os/src/fs/ext4_lw/`], [第 9 章],
  [时间、同步与性能诊断], [`os/src/timer/`、`os/src/sync/`、`os/src/utils/`、`os/src/utils/perf/`], [第 3--5、11 章],
)

== 参考资料

#bibliography("references.bib", title: none, style: "ieee", full: true)
