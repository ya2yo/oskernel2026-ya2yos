#import "../diagrams.typ": flow, relation, sequence

= 内存管理

本章描述当前工作树中 `os/src/mm/`、`os/src/syscall/mm/` 与架构页表代码的已实现行为。
Ya2yOS 同时支持 RISC-V64 和 LoongArch64；二者共用地址空间、VMA、用户复制和
COW 的高层接口，页表格式、内核映射和物理 RAM 布局由 `os/src/arch/` 分别实现。

== 模块边界与基本对象

#table(
  columns: (1.4fr, 3fr),
  table.header([*位置*], [*当前职责*]),
  [`os/src/mm/address.rs`], [物理/虚拟地址与页号类型，以及页边界转换。],
  [`os/src/mm/frame_alloc/`], [伙伴式 CMA 物理页分配、`FrameTracker` 和页缓存接口。],
  [`os/src/mm/group.rs`], [`GROUP_SHARE` 共享组：mmap 共享 VMA 的 groupid 分配与共享帧登记。],
  [`os/src/mm/heap_allocator.rs`], [静态内核堆与 `ContinuousPages`。],
  [`os/src/mm/map_area.rs`], [`MapArea`、映射权限、映射类型与 mmap 文件元数据。],
  [`os/src/mm/memory_set/`], [`MemorySet` 锁封装、ELF 装载、fork/COW、VMA 操作、mmap 与缺页分发。],
  [`os/src/mm/remote_tlb.rs`], [跨 hart TLB shootdown：全局更新锁、per-hart mailbox、IPI 与确认协议。],
  [`os/src/mm/page_fault_handler.rs`], [匿名/文件 mmap 缺页、COW 写保护缺页和文件 EOF 判断。],
  [`os/src/mm/translate.rs`], [用户地址校验、跨页复制、按需分配触发和安全 VA 到 PA 转换。],
  [`os/src/mm/uaccess.rs`], [per-hart uaccess 状态、`Scope`、fixup/retry 与同步内核 fault 分类。],
  [`os/src/mm/shm.rs`], [System V 共享内存段的创建、附加、分离和删除。],
  [`os/src/mm/mmap_bad_address.rs`], [mmap 坏地址表：`if_bad_address` 与坏地址的插入、移除。],
  [`os/src/arch/*/qemu/page_table.rs`], [RISC-V Sv39 与 LoongArch 页表项、激活、COW 与 TLB 操作。],
)

一个 `MemorySet` 代表一个可切换的虚拟地址空间。它以内部 `RwLock` 保护
`MemorySetInner`，后者拥有硬件 `PageTable`、按创建顺序保存的 `Vec<MapArea>` 和
`total_mmap_size` 计数。`Process` 保存 `Arc<MemorySet>`；调用者通过
`get_ref`/`get_mut` 或 `with_ref`/`with_mut` 取得短生命周期的读写保护，不应持有
地址空间锁跨越文件系统、调度或信号递送路径。

```rust
pub struct MemorySet {
    inner: RwLock<MemorySetInner>,
}

pub struct MemorySetInner {
    pub page_table: PageTable,
    pub areas: Vec<MapArea>,
    pub total_mmap_size: usize,
}
```

`MapArea` 是连续虚拟页范围的逻辑描述，记录权限、`Direct` 或 `Framed` 映射方式、
区域用途、每页的 `Arc<FrameTracker>`、mmap 后备文件与偏移、mmap flags 和
`MAP_SHARED` 使用的 group ID。PTE 只保存 VPN 到 PPN 的硬件转换；`data_frames`
则保存帧的 RAII 所有权和共享引用计数，因而是 COW、共享映射和释放物理页的依据。

#figure(
  relation((
    [*MemorySet*\ 锁保护的页表与 VMA 集合],
    [*MapArea*\ 范围、权限、后备对象、帧引用],
    [*FrameTracker*\ 物理页的零填充与 RAII 回收],
    [*PageTable*\ 架构相关的 VPN 到 PPN 转换]
  )),
  caption: [地址空间的高层对象关系。]
)

== 架构与地址布局

两种架构均采用 4 KiB 页面，用户空间上限为 `0x30_0000_0000`。从高地址向下，
布局由 trap context 页、每线程用户栈及 guard page、`MMAP_TOP` 以下的 mmap 区域
构成；ELF 主程序从其 program header 指定的地址装载，初始 brk 位于 ELF 映射末端
的一页 guard page 之后。`USER_STACK_SIZE` 为 8 MiB；RISC-V 内核栈为 4 页，
LoongArch 内核栈为 2 页。

#table(
  columns: (1.2fr, 1.4fr, 2.3fr),
  table.header([*项目*], [*RISC-V64 QEMU*], [*LoongArch64 QEMU*]),
  [物理 RAM], [`0x8000_0000` 起连续 2 GiB], [总计 2 GiB，低端 `0x0000_0000..0x1000_0000` 与高端 `0x8000_0000..0xf000_0000` 两段，中间为 PCI/MMIO hole。],
  [内核堆], [48 MiB 静态 `HeapSpace`], [128 MiB 静态 `HeapSpace`，确保内核镜像留在低端 RAM。],
  [直接映射], [`KERNEL_ADDR_OFFSET = 0xffff_ffc0_0000_0000`], [`KERNEL_ADDR_OFFSET = 0x9000_0000_0000_0000`；内核窗口和 MMIO 由 LoongArch 配置处理。],
  [页表格式], [Sv39；内核物理直映射尽可能使用 1 GiB、2 MiB 大页。], [LoongArch 页表与 TLB 机制；高层 `MapPermission` 映射为 LAPTE flags。],
)

#h(2em)RISC-V 启动页表只覆盖第一个 1 GiB。`init_cma()` 因此先把内核镜像之后、首 GiB
以内的 RAM 加入 CMA；`activate_kernel_space()` 建立完整内核映射后，
`init_cma_late()` 再加入第二个 GiB。这样伙伴分配器写入自身 free-list 元数据时不会
访问启动期尚未映射的物理页。LoongArch 使用 `PHYSICAL_MEMORY_RANGES` 分段纳管，
不会把 PCI/MMIO hole 误作为 RAM。

内核初始化顺序为：初始化静态内核堆，初始化 CMA，激活内核页表，补充 RISC-V 后段
CMA，然后执行 `remap_test()`。RISC-V 的测试检查内核映射权限；LoongArch 当前实现
将该测试作为空操作。

== 映射区域、页表与物理帧

`MapPermission` 统一表示 R/W/X/U。RISC-V 以 `RVPTEFlags` 编码有效、读写执行、
用户、访问、脏和软件 COW 位；LoongArch 以 `LAPTEFlags` 编码有效、PLV、可写、
缓存属性、不可读/不可执行和软件 COW 位。两套实现都将 COW 置于软件可用的 bit 9。

#table(
  columns: (1.1fr, 1.5fr, 2.1fr),
  table.header([*类型*], [*含义*], [*典型来源*]),
  [`Direct`], [VPN 通过内核页号偏移直接得到 PPN。], [RISC-V 内核物理直映射。],
  [`Framed`], [逐页从 CMA/页缓存分配 `FrameTracker`，并保存到 `data_frames`。], [ELF、用户栈、brk、mmap、共享内存和内核动态区域。],
  [`MMIO`], [按 `MMIO_MAP_OFFSET` 计算设备物理页，不建立帧追踪器。], [架构定义的 UART、块设备或 PCI 相关区域。],
)

#h(2em)`FrameTracker::alloc()` 从页缓存获得一页并在构造时清零；最后一个
`Arc<FrameTracker>` 销毁时经页缓存归还。`MapArea::map_one()` 对 framed 页分配 tracker
并建立 PTE，`unmap_one()` 删除 PTE 与相应 tracker。`push()` 立即映射整个区域，
`push_lazily()` 只登记 VMA，`push_with_given_frames()` 将既有的共享帧映射到新的 VMA。

CMA 是全局的伙伴式连续物理页分配器，页缓存以 32/128 页为低、高水位，空时批量
补充 16 页，超过高水位时最多刷回 64 页。RISC-V 启动页表只覆盖首个 1 GiB，
所以 `init_cma()` 先纳管可访问部分，完整内核直映射激活后再由 `init_cma_late()`
加入其余 RAM；LoongArch 则遍历分段物理范围，避免把 PCI/MMIO hole 当作 RAM。
这解决的是启动映射和物理布局差异，不是 NUMA 感知分配。CMA OOM 会使帧分配返回
失败，用户缺页随后无法建立映射并由 trap 层转为相应错误信号。

用户页表由 `PageTable::new_from_kernel()` 创建。RISC-V 路径复制内核高地址部分的根
页表项，使陷入内核后可继续访问内核映射；架构相关激活函数在切换地址空间时写入页表
根并刷新 TLB。该共享的是内核映射结构，而用户 VMA、用户页表下层和用户 `MapArea`
仍属于各自 `MemorySet`。

== 多核与不同架构下的一致性

`MemorySet`、`MapArea`、
`MapPermission`、缺页和 COW 逻辑共用；页表项格式、TLB 指令、权限位和物理 RAM
布局由 `os/src/arch/` 的实现分别适配。RISC-V 使用 Sv39 和内核物理直映射，
LoongArch 使用自己的页表/TLB 机制，并用 `PHYSICAL_MEMORY_RANGES` 跳过 PCI/MMIO
空洞。物理页目前仍由全局 CMA 与页缓存管理，不按 hart、架构或 NUMA 节点分区。

多 hart 共享一个 `MemorySet` 时，`active_harts` 是该地址空间当前可能仍被硬件访问的
hart 位图。用户返回前，`activate_for_user()` 在持有地址空间读锁时安装页表并发布
当前 hart；调度离开时先清除对应位。页表写者据此决定是否广播失效请求。会替换或移除
PPN 的操作（COW、`munmap`、`mremap`、fork/exec 回收等）还会暂时保留旧的
`Arc<FrameTracker>`，直到所有目标 hart 完成确认，避免远端仍持有旧 TLB 时物理页被
重新分配。只改变同一 PPN 的权限或脏位时，旧翻译至多更严格，可以走不保留帧的更新路径。

更新协议由 `remote_tlb::UPDATE_LOCK` 与 `MemorySet` 写锁组成，顺序固定为
`UPDATE_LOCK -> MemorySet 写锁 -> 子模块锁`；禁止在持有 `MemorySet` guard 时反向
获取 `UPDATE_LOCK`，也不在地址空间锁内进入文件系统、调度、网络或信号路径。新建
lazy PTE 的缺页快路径不会留下旧的有效翻译，因此不必广播；真正替换 PPN 或修改权限
则执行 shootdown。发起 hart 先刷新本地 TLB 和指令缓存，再为所有活动远端 hart 准备
带 sequence 的 mailbox，并一次性发送 IPI。目标 hart 在 trap/用户返回等可重入点轮询
mailbox，执行本地 TLB invalidate 与 instruction fence 后写入 acknowledged；等待者
会重新读取活动位图，已脱离该地址空间的 hart 不再阻塞协议。可执行页也必须做指令
同步，以防 hart 复用旧的译码指令流。

因此，异构支持的核心是“共用地址空间生命周期与一致性协议，架构层提供页表、TLB、
IPI 和用户访问原语”，而不是把两种架构强行统一为同一套硬件页表操作。

== ELF、brk 与按需分配

`MemorySetInner::from_elf()` 创建带内核映射的新地址空间，解析 ELF 并映射每个
`PT_LOAD` 段。段权限来自 ELF flags 加用户权限；文件字节被复制到 framed 页面，
`mem_size` 超出 `file_size` 的尾部保留为清零内容。若存在 `PT_INTERP`，加载器还会
映射动态解释器并把实际入口改为解释器入口；`AT_ENTRY` 仍描述主程序入口，auxv 同时
提供 `AT_PHDR`、`AT_PHENT`、`AT_PHNUM`、`AT_PAGESZ`、`AT_BASE` 等启动信息。

ELF 末端与 brk 之间保留一页 guard。brk VMA 从零长度开始，`sys_brk()` 通过
`TaskControlBlock::growproc()` 调整范围；增长不立即分配物理帧，首次访问才处理缺页。
单进程 brk 增长上限为 `MAX_BRK_SIZE = 2 GiB`，架构配置中的用户堆保留范围
`USER_HEAP_SIZE = 2 GiB`。这两个值与独立的 `MAX_MMAP_SIZE = 2 GiB` 预算不同；它们
描述虚拟地址空间限制，不代表启动时已经分配对应物理内存。收缩时，
`MemorySetInner::grow()` 会解除新末端之后已经存在的 PTE 并释放相应 tracker。

`MemorySetInner::handle_page_fault()` 首先查找覆盖 VPN 的 VMA。未映射页的 read、
write 或 fetch fault 在权限允许时走以下路径：`Brk` 与 `Stack` 分配匿名零页；
`Mmap` 进入文件/匿名 mmap 缺页处理。已存在但写保护的页只在 store 或 page-modify
fault 上尝试 COW 或写权限恢复，读/取指权限错误不会被误作 COW 处理。

#figure(
  flow((
    [*CPU 产生用户页异常*],
    [*按 VPN 查找 MapArea 与访问权限*],
    [*未映射：匿名页、文件 mmap 页或共享页分配*],
    [*已映射且写保护：COW 或写权限恢复*],
    [*建立/更新 PTE 后刷新 TLB；不能修复则由 trap 层发送信号*]
  )),
  caption: [当前用户缺页处理的高层分支。]
)

#h(2em)文件 mmap 的 EOF 语义按 backing inode 的当前长度判断。最后一个部分页面可以零填充；若
fault 页的起始文件偏移已在当前 EOF 之外，`mmap_file_page_beyond_eof()` 令 trap 层报告
`SIGBUS`，而不是错误地建立零页。建立或替换 VMA 时会先让 ext4 inode 记录当前长度，
因此 unlink 后仍可使用打开 inode 的长度；后续 write/truncate 更新该长度，文件扩展后新
覆盖的页面不会被过期的 VMA 快照误判为 `SIGBUS`。

== fork 与写时复制

非 `CLONE_VM` 的 clone 通过 `MemorySetInner::from_existed_user()` 构造子地址空间。
它先创建新用户页表，再按 VMA 类型处理父地址空间：`Stack` 与 `Trap` 不直接复制，
由任务创建路径为子线程建立独立资源；`Shm` 和 `MAP_SHARED` 复用已有 frames；ELF、
brk、私有 mmap 等可写 framed 页面则建立软件 COW 关系。

fork 前，匿名或文件后备的 `MAP_SHARED` VMA 会被预先 fault：否则父子之后都可能各自
为同一延迟页分配不同物理帧，破坏共享可见性。共享 VMA 以 `groupid` 关联
`GROUP_SHARE`；首次 fault 把 frame 登记到该组，后续同组 VMA 克隆该 `Arc`。组内最后
一个 `MapArea` 被释放时，group ID 和共享帧一起释放。

对于 COW 页，父子 PTE 均去除写权限、设置软件 COW，并共享 `Arc<FrameTracker>`。
写 fault 时，若当前 frame 已唯一引用，只需恢复 PTE 写权限；若仍被多个 VMA 持有，
则分配新零页、复制旧页内容、把当前 VMA 的 PTE 改指向新页并恢复写权限。此处理同时
覆盖 RISC-V store page fault 与 LoongArch 的 page-modify 相关路径。

#table(
  columns: (1.25fr, 2.8fr),
  table.header([*区域*], [*fork 行为*]),
  [`Stack` / `Trap`], [不由 `from_existed_user()` 复制；子任务建立独立栈与 trap context。],
  [`Elf` / `Brk`], [复制 VMA 元数据和帧引用；可写页变为 COW。],
  [`MAP_PRIVATE`], [按已有已分配页共享后建立 COW，尚未 fault 的页仍保持延迟状态。],
  [`MAP_SHARED`], [预先 materialize 并通过 `GROUP_SHARE`/共享 `Arc<FrameTracker>` 保持父子可见性。],
  [`Shm`], [复用 System V 段保存的帧，不使用 COW。],
)

== mmap、munmap 与 mprotect

`sys_mmap()` 检查长度、对齐、flags、偏移与文件读写权限后，调用 `MemorySet::mmap()`。
匿名映射必须包含 `MAP_ANONYMOUS`；文件映射保存 `OSFile`、文件偏移和映射时的大小
快照。普通映射从 `MMAP_TOP` 向低地址寻找空洞，登记为延迟 `MapArea`，并按虚拟长度
累计到 `total_mmap_size`。该延迟 VMA 预算受 `MAX_MMAP_SIZE = 2 GiB` 限制；实际物理页
仍只在缺页时分配。`MAP_STACK` 使用 `MapAreaType::Stack`；其他 mmap 使用
`MapAreaType::Mmap`。

`MAP_FIXED` 与 `MAP_FIXED_NOREPLACE` 使用调用者指定地址。后者若与既有 VMA 相交，
内部返回失败，syscall 映射为 `EEXIST`；固定映射在需要时调用 VMA 拆分/权限更新路径或
创建新的延迟 VMA。当前固定映射不计入 `total_mmap_size`，这是实现上的可见限制。

`MemorySetInner::munmap()` 只处理 `MapAreaType::Mmap`。对于完整覆盖的 VMA，它解除
页面映射并删除 VMA；对于部分覆盖，则保留前后片段并转移相应 `data_frames`。可写的
`MAP_SHARED` 文件 VMA 在文件仍有链接时，把实际已分配的连续帧范围分块（64 KiB）写回
后备文件，随后刷新 TLB。`mprotect()` 根据边界将 VMA 拆成最多三段，更新目标段的
`map_perm` 和既有 PTE；它不把未分配页面 materialize。系统调用入口要求地址与长度
页对齐，并检查 `addr + len` 溢出。

`madvise()` 先要求整个范围被现有 VMA 覆盖。`MADV_NORMAL`、`RANDOM`、`SEQUENTIAL`
和 `WILLNEED` 完成参数兼容检查；`MADV_DONTNEED` 对匿名 brk 和私有 mmap 释放驻留
帧而保留 VMA，后续访问重新缺页，`MAP_SHARED` 和固定 ELF 映射按实现例外保留。
`mlock`/`munlock`/`mlockall`/`munlockall`/`mlock2` 目前只有参数校验路径：由于没有
swap，已分配页本来就驻留，因此不建立真正的锁页记账和资源限制。

`mremap()` 由 `MemorySet::mremap()` 根据 flags 分派到原地扩展/收缩或可移动迁移路径。完整覆盖的 `Mmap`（以及当前实现允许的 `Shm`）区域可以原地调整；带
`MREMAP_MAYMOVE` 时选择不相交的新范围，先复制所有已驻留页，成功后才卸载旧映射，
未驻留页仍保持 lazy fault。`MAP_SHARED` 的驻留页也会深复制到目标帧，因而当前
语义不是跨地址范围继续共享同一 PPN；`MAP_SHARED_VALIDATE` 明确返回 `ENOSYS`。
`MREMAP_FIXED` 需要同时带 `MREMAP_MAYMOVE`，目标范围不能与源范围重叠，并会先处理
目标区已有映射。所有可能替换/释放 PPN 的路径都通过地址空间更新协议刷新 TLB。
`mincore()` 检查范围覆盖和读权限，并按 PTE 是否存在返回驻留位；内核没有 swap，
已映射页即视为驻留。

#figure(
  sequence(((
    [用户态], [mmap / munmap / mprotect 请求], [syscall 入口]),
    ([syscall 入口], [参数校验、文件权限和用户指针处理], [MemorySet]),
    ([MemorySet], [登记、拆分或移除 VMA；按需写回共享文件页], [页表 / 文件系统])
  )),
  caption: [VMA 系统调用的当前交互。]
)

== uaccess 设计方案

用户指针不能在 syscall 中直接当作内核指针解引用。当前方案把访问分成“检查、按页
转换、架构 fast path”三层。`checked_user_range()` 先拒绝空首地址、非规范虚拟地址
和整数溢出；随后扫描所有覆盖页的 VMA 权限。普通 `copy_from_user()`/
`copy_to_user()` 按页取得 PPN，缺页时调用 `MemorySet::handle_page_fault()`，写入时
还显式处理 present COW PTE，最终以 `EFAULT` 表示地址、权限或缺页失败。

`translate_user_va_safe()` 先用一次安全读触发 lazy allocation，再返回已经存在的
VA 到 PA；只用于已确认映射的内部路径的 direct helper 则不处理缺页、不处理 COW，
调用者必须先保证页面有效。这一划分避免把“查询物理地址”误写成会隐式分配的操作。

对当前 hart 正在运行、且长度不超过 256 字节的小型复制，`translate.rs` 会尝试架构
uaccess fast path。RISC-V 在 `Scope` 期间临时启用 sstatus.SUM，LoongArch 使用
对应的汇编复制原语；两者都由 `Scope` 记录当前 `MemorySet` 和 fault fixup 地址，
退出时恢复 per-hart 状态。更大的缓冲区、非当前地址空间或 fast path 失败时，回到
软件按页转换路径，因而不会让大块 I/O 长时间占用地址空间写者需要的锁。

直接复制引起的同步内核 fault 由 `os/src/mm/uaccess.rs` 的每 hart 状态接管。状态含
`active`、地址空间指针、fixup PC 和一次性 `retry_vpn`：若故障页已经有允许当前
访问的 present PTE，只把它视为本地陈旧 TLB，刷新一次后重试；若是缺页、文件后备
页、COW 或权限不满足，则跳转 fixup。fixup 不会在 trap frame 中进入可能阻塞的
普通缺页解析器，调用方随后在可阻塞的软件路径中重新执行。无法确认是当前任务的
地址空间、用户地址范围之外的内核故障或其他异常则返回 `Unhandled`，保留内核错误
处理路径。这样既避免了直接访问用户内存的安全问题，也避免在同步 fault 中重新取得
任务锁或阻塞文件系统而形成死锁。

== System V 共享内存与用户复制

`shm_create()` 在全局 `ShmManager` 中一次性分配 `Vec<Arc<FrameTracker>>`，返回 key；
`shm_attach()` 把这些既有帧作为 `MapAreaType::Shm` 通过 `push_with_given_frames()` 映射
到当前进程，地址为零时从 `MMAP_TOP` 向下选择地址。`shm_detach()` 要求地址页对齐并按
VMA 起始页移除映射；`shm_drop()` 删除全局段记录。当前 `MemorySet::shm()` 对非零指定
附加地址会 `panic`，因此固定地址 `shmat` 不是可用的兼容返回路径，文档和测试应将其
视为需要修复的危险边界。

所有 syscall 用户指针通过 `copy_from_user`、`copy_to_user` 或其 typed wrapper 访问。
这些函数拒绝空首地址、不可表示的规范虚拟地址和范围溢出，并按页复制；遇到尚未映射
的合法用户页时，会带 Load 或 Store fault 调用 `MemorySet::handle_page_fault()`。写入
路径还会触发 COW 处理。LoongArch 额外使用 VMA 权限检查；两种架构最终都以失败返回
`EFAULT`。

`translate_user_va_safe()` 在地址转换前先通过读取触发需要的懒分配，适用于 futex 等
必须取得物理地址的调用点；`translate_va()` 则只查询现有 PTE，不会隐式分配。内部缺页
处理、写回等已确认映射存在的路径可使用 direct read/write helper，以避免用户复制函数
再次取锁。

== 当前边界

#table(
  columns: (1.5fr, 2.9fr),
  table.header([*主题*], [*当前实现边界*]),
  [物理页回收], [无 swap；`FrameTracker` 最后引用释放后归还 CMA。],
  [多核一致性], [`active_harts`、全局更新锁和 mailbox/ IPI shootdown 保证共享地址空间的旧 TLB 在帧回收前失效；当前没有 per-hart 或 NUMA 本地分配器。],
  [uaccess 快路径], [仅当前 hart 的活动地址空间和不超过 256 字节的小复制使用架构原语；缺页/COW/文件 fault 回退到可阻塞的软件按页路径。],
  [mlock 系列], [无 swap 时已分配页天然驻留；`mlock`/`munlock`/`mlockall`/`munlockall`/`mlock2` 仅做参数校验并 no-op。],
  [异构架构], [RISC-V64 与 LoongArch64 共用 VMA/COW/uaccess 高层协议，但 PTE、权限编码、TLB、IPI 和物理 RAM 分段分别实现；未实现 NUMA 拓扑或不同内存一致性域。],
  [mmap 地址空间], [普通 mmap 有 2 GiB 延迟 VMA 计数上限；固定 mmap 不计入该计数。],
  [mremap], [支持完整 `Mmap`/`Shm` 区域的原地调整和 `MREMAP_MAYMOVE` 迁移；驻留页先复制后提交，`MAP_SHARED_VALIDATE` 与 `MREMAP_DONTUNMAP` 未实现；fixed 必须配合 MAYMOVE。],
  [SysV shmat], [仅自动选址；显式非零地址尚未实现。],
  [文件 mmap EOF], [完整页落在映射时 EOF 外会发 `SIGBUS`；最后一个部分页允许零填充。],
  [跨架构差异], [高层 VMA/COW 接口通用，PTE 格式、TLB 和内核物理映射依 RISC-V/LoongArch 不同。],
)

#text(size: 8.5pt, fill: rgb("536471"))[_实现追溯：_ 本章对应 `os/src/mm/`、`os/src/syscall/mm/`、`os/src/mm/remote_tlb.rs`、`os/src/mm/uaccess.rs` 以及 `os/src/arch/riscv64/`、`os/src/arch/loongarch64/` 的当前实现；异构与多核部分描述的是共享抽象、分架构适配和现有 hart shootdown 协议，不扩展为尚未实现的 NUMA 能力。]
