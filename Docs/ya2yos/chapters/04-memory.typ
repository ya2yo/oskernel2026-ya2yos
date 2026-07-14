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
  [`os/src/mm/heap_allocator.rs`], [静态内核堆与 `ContinuousPages`。],
  [`os/src/mm/map_area.rs`], [`MapArea`、映射权限、映射类型与 mmap 文件元数据。],
  [`os/src/mm/memory_set/`], [`MemorySet` 锁封装、ELF 装载、fork/COW、VMA 操作、mmap 与缺页分发。],
  [`os/src/mm/page_fault_handler.rs`], [匿名/文件 mmap 缺页、COW 写保护缺页和文件 EOF 判断。],
  [`os/src/mm/translate.rs`], [用户地址校验、跨页复制、按需分配触发和安全 VA 到 PA 转换。],
  [`os/src/mm/shm.rs`], [System V 共享内存段的创建、附加、分离和删除。],
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
    [*MemorySet*\锁保护的页表与 VMA 集合],
    [*MapArea*\范围、权限、后备对象、帧引用],
    [*FrameTracker*\物理页的零填充与 RAII 回收],
    [*PageTable*\架构相关的 VPN 到 PPN 转换]
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

RISC-V 启动页表只覆盖第一个 1 GiB。`init_cma()` 因此先把内核镜像之后、首 GiB
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

`FrameTracker::alloc()` 从页缓存获得一页并在构造时清零；最后一个
`Arc<FrameTracker>` 销毁时经页缓存归还。`MapArea::map_one()` 对 framed 页分配 tracker
并建立 PTE，`unmap_one()` 删除 PTE 与相应 tracker。`push()` 立即映射整个区域，
`push_lazily()` 只登记 VMA，`push_with_given_frames()` 将既有的共享帧映射到新的 VMA。

用户页表由 `PageTable::new_from_kernel()` 创建。RISC-V 路径复制内核高地址部分的根
页表项，使陷入内核后可继续访问内核映射；架构相关激活函数在切换地址空间时写入页表
根并刷新 TLB。该共享的是内核映射结构，而用户 VMA、用户页表下层和用户 `MapArea`
仍属于各自 `MemorySet`。

== ELF、brk 与按需分配

`MemorySetInner::from_elf()` 创建带内核映射的新地址空间，解析 ELF 并映射每个
`PT_LOAD` 段。段权限来自 ELF flags 加用户权限；文件字节被复制到 framed 页面，
`mem_size` 超出 `file_size` 的尾部保留为清零内容。若存在 `PT_INTERP`，加载器还会
映射动态解释器并把实际入口改为解释器入口；`AT_ENTRY` 仍描述主程序入口，auxv 同时
提供 `AT_PHDR`、`AT_PHENT`、`AT_PHNUM`、`AT_PAGESZ`、`AT_BASE` 等启动信息。

ELF 末端与 brk 之间保留一页 guard。brk VMA 从零长度开始，`sys_brk()` 通过
`TaskControlBlock::growproc()` 调整范围；增长不立即分配物理帧，首次访问才处理缺页。
单进程 brk 增长上限为 `MAX_BRK_SIZE = 512 MiB`，虚拟保留范围为
`USER_HEAP_SIZE = 512 MiB`。收缩时，`MemorySetInner::grow()` 会解除新末端之后已经
存在的 PTE 并释放相应 tracker。

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

文件 mmap 的 EOF 语义由 VMA 创建时的 `mapped_file_size` 快照维持。最后一个部分页面
可以零填充；若 fault 页的起始文件偏移已在快照 EOF 之外，
`mmap_file_page_beyond_eof()` 令 trap 层报告 `SIGBUS`，而不是错误地建立零页。该快照
还避免了文件 unlink 后 ext4 路径式元数据无法表示已映射文件长度的问题。

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
累计到 `total_mmap_size`。该计数受 `MAX_MMAP_SIZE = 512 MiB` 限制，以防无界 VMA 在
后续缺页时耗尽 CMA。`MAP_STACK` 使用 `MapAreaType::Stack`；其他 mmap 使用
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

`mremap()` 目前只实现 `MREMAP_MAYMOVE` 的“解除旧映射后再新建映射”路径；
`MREMAP_FIXED` 和 `MREMAP_DONTUNMAP` 返回 `ENOSYS`。`mincore()` 检查范围覆盖和读
权限，并按 PTE 是否已存在向用户返回驻留位；内核没有 swap，已映射页即视为驻留。

#figure(
  sequence(((
    [用户态], [mmap / munmap / mprotect 请求], [syscall 入口]),
    ([syscall 入口], [参数校验、文件权限和用户指针处理], [MemorySet]),
    ([MemorySet], [登记、拆分或移除 VMA；按需写回共享文件页], [页表 / 文件系统])
  )),
  caption: [VMA 系统调用的当前交互。]
)

== System V 共享内存与用户复制

`shm_create()` 在全局 `ShmManager` 中一次性分配 `Vec<Arc<FrameTracker>>`，返回 key；
`shm_attach()` 把这些既有帧作为 `MapAreaType::Shm` 通过 `push_with_given_frames()` 映射
到当前进程，地址为零时从 `MMAP_TOP` 向下选择地址。`shm_detach()` 要求地址页对齐并按
VMA 起始页移除映射；`shm_drop()` 删除全局段记录。当前 `MemorySet::shm()` 对非零指定
附加地址会 panic，因此固定地址 `shmat` 不应表述为已支持能力。

所有 syscall 用户指针通过 `copy_from_user`、`copy_to_user` 或其 typed wrapper 访问。
这些函数拒绝空首地址、不可表示的规范虚拟地址和范围溢出，并按页复制；遇到尚未映射
的合法用户页时，会带 Load 或 Store fault 调用 `MemorySet::handle_page_fault()`。写入
路径还会触发 COW 处理。LoongArch 额外使用 VMA 权限检查；两种架构最终都以失败返回
`EFAULT`，而不是直接解引用用户虚拟地址。

`translate_user_va_safe()` 在地址转换前先通过读取触发需要的懒分配，适用于 futex 等
必须取得物理地址的调用点；`translate_va()` 则只查询现有 PTE，不会隐式分配。内部缺页
处理、写回等已确认映射存在的路径可使用 direct read/write helper，以避免用户复制函数
再次取锁。

== 当前边界

#table(
  columns: (1.5fr, 2.9fr),
  table.header([*主题*], [*当前实现边界*]),
  [物理页回收], [无 swap；`FrameTracker` 最后引用释放后归还 CMA。],
  [mmap 地址空间], [普通 mmap 有 512 MiB 计数上限；固定 mmap 不计入该计数。],
  [mremap], [仅 `MREMAP_MAYMOVE` 的重建式路径；固定和 DONTUNMAP 未实现。],
  [SysV shmat], [仅自动选址；显式非零地址尚未实现。],
  [文件 mmap EOF], [完整页落在映射时 EOF 外会发 `SIGBUS`；最后一个部分页允许零填充。],
  [跨架构差异], [高层 VMA/COW 接口通用，PTE 格式、TLB 和内核物理映射依 RISC-V/LoongArch 不同。],
)
