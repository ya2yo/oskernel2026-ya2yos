# LoongArch exec 段读取页缓存化与帧清零首触成本分析

## 背景

`log.ans`（cagent kernel 用例，loongarch64）显示单用例约 2s，其中
`exec_duration from_elf` 稳定占 180~230ms，`exec_loader_duration interp_map`
占 140~155ms（9 次动态 exec，每次约 15~17ms）。维护者此前已确认动态解释器必须
保持 eager framed 映射（见 buildstorm-main-elf-eager-mapping-regression.md），
因此优化目标是 eager 路径本身，而不是把解释器改成 lazy。

## 现象

`interp_map` 内部首次分阶段计时显示：

- `setup`（分配帧 + 建页表）：约 130~142ms / 29 段，是最主要开销；
- `read`（lwext4 文件读取）：约 30~37ms；
- `copy`（页拷贝）：约 1.5ms。

重复执行动态程序时，同一解释器文件每次 exec 都重新进入 lwext4 读取路径。

## 分析

### 1. setup 慢的根因是“新页首次写”

进一步拆分帧分配发现 `FrameTracker::new` 的清零循环不是指令数问题：

- 同一页首次**读**只需约 8us；
- 同一页首次**写**约 105~127us；
- 同一页第二次**写**只需约 7us。

与 TLB 无关（首次读很快）；与清零指令宽度无关（u64 循环与 `ptr::write_bytes`
结果相同）；与 `invtlb` 频率无关（批量刷新后 setup 不变）。结合宿主环境
（约 7.8GiB 物理内存、36GiB 客户机超额分配、swap 已占用 3GiB），结论是 QEMU
匿名内存的宿主 COW/换页成本：每个从未写过的 guest 页首次写触发宿主页故障。
这是环境主导的固定成本，guest 侧无法消除，只能减少需要触碰的页数。

### 2. 逐页 invtlb 与 trap 出入口 invtlb 均非本次瓶颈

- loongarch64 `page_table::map_by_pte_flags` 每映射一页执行一次全量 `invtlb`；
  批量刷新（一次 `invtlb` 收尾）后 setup 无变化。
- `trap.S` 每次陷入/返回各执行一次全量 `invtlb`；移除后用例仍 PASS 但总耗时无
  变化。该刷新属于 init 提交继承的防御代码，涉及跨进程 TLB 一致性，未保留移除。

### 3. exec 段读取可复用内核文件页缓存

`push_elf_segment_from_file` 直接调用 `inode.read_at`，走 lwext4 C 层；内核的
`FILE_PAGE_CACHE`（mmap/read 共用）未被 exec 路径使用。解释器与未对齐主程序段
每次 exec 都被重读。把整段 64KiB 分块改为“先查 `FILE_PAGE_CACHE`，未命中才走
lwext4 并发布完整页”后，重复 exec 直接命中内核缓存。

注意：`Ext4Inode::read_at_impl` 顶部检查的 `read_cached_at` 是 lwext4 自带字节
缓存，不是内核 `FILE_PAGE_CACHE`；只向内核页缓存发布数据并不会改变 read_at_impl
的路径，必须在 exec 读取循环里先查内核缓存。

## 修复

- `os/src/mm/memory_set/elf_loader.rs`：`push_elf_segment_from_file` 的每 64KiB
  分块先尝试 `FILE_PAGE_CACHE.read_cached_at`；未全部命中时走原 `inode.read_at`
  （保留兼容补丁语义），再用 `insert_read_range` 发布完整覆盖页。
- `os/src/arch/loongarch64/qemu/page_table.rs` 与
  `os/src/arch/riscv64/qemu/page_table.rs`：新增 `map_no_flush` 与 `flush_tlb_all`，
  `map_by_pte_flags` 拆出无刷新变体。
- `os/src/mm/map_area.rs`：`MapArea::map` / `map_given_frames` 批量映射后只做一次
  TLB 刷新，替代每页一次全量刷新。

## 验证

- `make`（RISC-V + LoongArch64 release）与 `make log`（loongarch64 debug）通过。
- `make perf` + `timeout 600s make run > log.ans 2>&1` 连续两次：
  `END cagent kernel pass`、`shutdown!`，无 panic。
- ext4 读取从 571 次 / 6.29MiB 降至 555 次 / 5.25MiB；块设备请求 974→958，
  字节 10.15MiB→9.10MiB，证明重复 exec 的段读取已命中内核页缓存。
- 端到端 guest 时间在既有噪声带内（1.7~2.1s），未声明加速比例；`from_elf` 仍被
  宿主页故障主导。BuildStorm/LTP 动态用例未在本轮运行。
