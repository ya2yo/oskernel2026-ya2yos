//! `MemorySetInner` 的虚拟内存区域（VMA）管理操作。
//!
//! 本模块负责 `MapArea` 的创建、查找、插入、拆分、合并、删除以及页面内容
//! 复制等基础操作，并维护 `areas` 按起始 VPN 排序的核心不变量。这里处理的
//! 是地址空间布局和页表映射本身；ELF 装载、fork 地址空间复制、mmap 参数
//! 解析等更高层逻辑分别位于其他子模块。
//!
//! 大多数操作将“逻辑区域”与“实际页表映射”分开处理：eager 路径立即分配并
//! 映射物理页，lazy 路径只登记 `MapArea`，待缺页异常发生时再分配页面。区域
//! 插入通常会合并相邻且属性完全兼容的匿名私有映射，但涉及拆分的变换路径
//! 可以显式跳过合并，以避免破坏调用者正在使用的区域索引。

use alloc::{sync::Arc, vec::Vec};

use super::MemorySetInner;
use crate::{
    arch::{
        memory_layout::{MMAP_TOP, PAGE_SIZE, USER_HEAP_SIZE},
        page_table::PageTable,
    },
    mm::{
        map_area::MapType, FrameTracker, MapArea, MapAreaType, MapPermission, VPNRange, VirtAddr,
        VirtPageNum,
    },
    syscall::MmapFlags,
};

impl MemorySetInner {
    /// 创建一个仅包含全新页表的空用户地址空间。
    ///
    /// 该构造函数不预先加入任何区域，也不复制内核映射，适用于需要完全
    /// 独立建立地址空间内容的场景。mmap 搜索提示初始化为 [`MMAP_TOP`]。
    pub fn new_bare() -> Self {
        Self {
            page_table: PageTable::new(),
            areas: Vec::new(),
            total_mmap_size: 0,
            mmap_hint: MMAP_TOP,
        }
    }

    /// 创建一个已包含内核映射的空地址空间。
    ///
    /// 与 [`Self::new_bare`] 的区别仅在于页表由 [`PageTable::new_from_kernel`]
    /// 初始化，因此新地址空间可以直接访问内核所需的共享映射。
    pub fn new_from_kernel() -> Self {
        Self {
            page_table: PageTable::new_from_kernel(),
            areas: Vec::new(),
            total_mmap_size: 0,
            mmap_hint: MMAP_TOP,
        }
    }

    /// 立即分配并插入一个基于物理帧的逻辑区域。
    ///
    /// `start_va..end_va` 会被转换为页范围并立即建立页表映射。调用者必须
    /// 保证新区域不与已有区域重叠；底层插入失败时该方法会忽略错误，原因是
    /// 当前调用点属于内核内部初始化路径，按设计不预期出现分配失败。
    pub fn insert_framed_area(
        &mut self,
        start_va: VirtAddr,
        end_va: VirtAddr,
        permission: MapPermission,
        area_type: MapAreaType,
    ) {
        self.push(
            MapArea::new(start_va, end_va, MapType::Framed, permission, area_type),
            None,
        )
        .ok();
    }

    /// 插入一个暂不建立页表映射的懒分配区域。
    ///
    /// 此方法只登记区域范围和权限，不立即分配物理页；实际页面由缺页处理
    /// 路径在首次访问时分配并映射。适合堆、栈或其他允许按需提交的用户区域。
    pub fn lazy_insert_framed_area(
        &mut self,
        start_va: VirtAddr,
        end_va: VirtAddr,
        permission: MapPermission,
        area_type: MapAreaType,
    ) {
        self.push_lazily(MapArea::new(
            start_va,
            end_va,
            MapType::Framed,
            permission,
            area_type,
        ));
    }

    /// 删除起始 VPN 等于 `start_vpn` 的逻辑区域。
    ///
    /// 删除前会先解除该区域覆盖的所有页表映射并释放其帧跟踪对象，然后从
    /// 有序区域列表中移除区域。如果没有匹配区域，该操作不产生任何影响。
    pub fn remove_area_with_start_vpn(&mut self, start_vpn: VirtPageNum) {
        if let Some((idx, area)) = self
            .areas
            .iter_mut()
            .enumerate()
            .find(|(_, area)| area.vpn_range.start() == start_vpn)
        {
            area.unmap(&mut self.page_table);
            self.areas.remove(idx);
        }
    }

    /// 在 `hint` 以下查找空洞并立即插入一个基于物理帧的区域。
    ///
    /// `size` 会按页向上取整，返回实际选择的半开地址区间
    /// `(start_va, end_va)`。如果地址空间不足，底层查找可能返回零地址；
    /// 调用者应结合返回值判断分配是否成功。
    pub fn insert_framed_area_with_hint(
        &mut self,
        hint: usize,
        size: usize,
        map_perm: MapPermission,
        area_type: MapAreaType,
    ) -> (usize, usize) {
        let start_va = self.find_insert_addr(hint, size);
        let end_va = start_va + size;
        self.insert_framed_area(
            VirtAddr::from(start_va),
            VirtAddr::from(end_va),
            map_perm,
            area_type,
        );
        (start_va, end_va)
    }

    /// 在 `hint` 以下查找空洞并懒惰插入一个基于物理帧的区域。
    ///
    /// 只登记逻辑区域，不立即分配页面；返回实际选择的半开地址区间
    /// `(start_va, end_va)`。地址不足时返回的起始地址为零。
    pub fn lazy_insert_framed_area_with_hint(
        &mut self,
        hint: usize,
        size: usize,
        map_perm: MapPermission,
        area_type: MapAreaType,
    ) -> (usize, usize) {
        let start_va = self.find_insert_addr(hint, size);
        let end_va = start_va + size;
        self.lazy_insert_framed_area(
            VirtAddr::from(start_va),
            VirtAddr::from(end_va),
            map_perm,
            area_type,
        );
        (start_va, end_va)
    }

    /// 查找一个结束地址不超过 `hint` 的空闲虚拟地址范围。
    ///
    /// `areas` 始终按起始 VPN 排序，因此可以从上到下搜索：候选范围发生
    /// 冲突时，将候选位置移动到冲突 VMA 下方，再继续检查更低区域，避免每次
    /// 碰撞都重新扫描已经排除的部分。返回值按页对齐；无法找到或算术溢出时
    /// 返回零。为兼容历史布局，已占用 VMA 与新区域之间保留一个保护页。
    pub fn find_insert_addr(&self, hint: usize, size: usize) -> usize {
        let pages = match size.checked_add(PAGE_SIZE - 1) {
            Some(size) => size / PAGE_SIZE,
            None => return 0,
        };
        if pages == 0 {
            return 0;
        }
        let mut end_vpn = VirtAddr::from(hint).floor();
        let mut start_vpn = match end_vpn.0.checked_sub(pages) {
            Some(start) => VirtPageNum(start),
            None => return 0,
        };

        for area in self.areas.iter().rev() {
            let (area_start, area_end) = area.vpn_range.range();
            if area_end <= start_vpn {
                break;
            }
            if area_start >= end_vpn {
                continue;
            }

            // Keep the historical one-page separation below an occupied VMA.
            // Besides matching the old recursive search, this protects the
            // guard page expected by MAP_GROWSDOWN stacks.
            let candidate_end = match area_start.0.checked_sub(1) {
                Some(end) => VirtPageNum(end),
                None => return 0,
            };
            end_vpn = candidate_end;
            start_vpn = match end_vpn.0.checked_sub(pages) {
                Some(start) => VirtPageNum(start),
                None => return 0,
            };
        }
        VirtAddr::from(start_vpn).0
    }

    /// 查找非 `MAP_FIXED` mmap 映射的地址。
    ///
    /// 优先从上一次成功分配的位置 [`Self::mmap_hint`] 向下搜索；该位置无
    /// 可用空间时，再从完整的地址空间上界 [`MMAP_TOP`] 重新搜索。找到地址
    /// 后更新提示，供后续 mmap 调用复用。
    pub(crate) fn find_mmap_addr(&mut self, size: usize) -> usize {
        let hint = self.mmap_hint.max(PAGE_SIZE).min(MMAP_TOP);
        let mut addr = self.find_insert_addr(hint, size);
        if addr == 0 && hint != MMAP_TOP {
            addr = self.find_insert_addr(MMAP_TOP, size);
        }
        if addr != 0 {
            self.mmap_hint = addr;
        }
        addr
    }

    /// 在非 `MAP_FIXED` 映射中尝试直接采用调用者提供的地址提示。
    ///
    /// 方法会将起始地址向下页对齐、将结束地址向上覆盖完整页，并检查整个
    /// 范围是否位于用户 mmap 上界内且不与现有 VMA 重叠。提示为零、范围溢出
    /// 或发生冲突时返回 `None`；成功时更新 [`Self::mmap_hint`] 并返回对齐后的
    /// 起始地址。虽然 Linux 在提示冲突时可以另选地址，但某些 mremap 路径
    /// 需要新近预留的空洞被准确采用。
    pub(crate) fn try_mmap_hint(&mut self, hint: usize, size: usize) -> Option<usize> {
        if hint == 0 {
            return None;
        }
        let start = hint & !(PAGE_SIZE - 1);
        let end = start.checked_add(size)?;
        if start == 0 || end > MMAP_TOP {
            return None;
        }
        let start_vpn = VirtAddr::from(start).floor();
        let end_vpn = VirtAddr::from(end).ceil();
        if self.areas.iter().any(|area| {
            let (area_start, area_end) = area.vpn_range.range();
            area_start < end_vpn && start_vpn < area_end
        }) {
            return None;
        }
        self.mmap_hint = start;
        Some(start)
    }

    /// 查找起始地址按 `align` 字节对齐的空闲 mmap 范围。
    ///
    /// `align` 必须是非零二次幂；搜索时会为对齐余量扩大候选范围，再验证
    /// 对齐后的真实区间仍位于搜索上界和 [`MMAP_TOP`] 内。成功后更新 mmap
    /// 搜索提示，失败或发生整数溢出时返回零。
    pub(crate) fn find_mmap_addr_aligned(&mut self, size: usize, align: usize) -> usize {
        if align == 0 || !align.is_power_of_two() {
            return 0;
        }
        let search_size = match size.checked_add(align - 1) {
            Some(value) => value,
            None => return 0,
        };
        let hint = self.mmap_hint.max(PAGE_SIZE).min(MMAP_TOP);
        for search_hint in [hint, MMAP_TOP] {
            if search_hint != MMAP_TOP && search_hint == hint && hint == MMAP_TOP {
                continue;
            }
            let candidate = self.find_insert_addr(search_hint, search_size);
            if candidate == 0 {
                continue;
            }
            let Some(addr_base) = candidate.checked_add(align - 1) else {
                continue;
            };
            let addr = addr_base & !(align - 1);
            let Some(end) = addr.checked_add(size) else {
                continue;
            };
            if end > search_hint || end > MMAP_TOP {
                continue;
            }
            self.mmap_hint = addr;
            return addr;
        }
        0
    }

    /// 查找范围恰好等于 `[l, r)` 的 VMA。
    ///
    /// 参数使用半开 VPN 区间；找到时返回该区域的可变引用，未找到时返回
    /// `None`。调用者应注意，可变借用期间不能同时修改 `areas` 列表。
    pub fn find_area_by_range(&mut self, l: VirtPageNum, r: VirtPageNum) -> Option<&mut MapArea> {
        let target = (l, r);
        self.areas
            .iter_mut()
            .find(|area| area.vpn_range.range() == target)
    }

    /// 调整进程 `brk` 区域并返回新的堆指针。
    ///
    /// 正增长只为新增且未被其他固定映射占用的子区间登记懒分配 Brk VMA，
    /// 从而保留用户可见的连续 `brk` 指针，同时允许 `MAP_FIXED` 在堆范围内
    /// 留下空洞。负增长会移除收缩范围内的 Brk 区域和页表映射。若新地址
    /// 越过堆上界、低于堆底或发生地址溢出，返回 `None`。
    pub fn grow(
        &mut self,
        grow_size: isize,
        user_heappoint: usize,
        user_heapbottom: usize,
    ) -> Option<usize> {
        let new_addr = user_heappoint.checked_add_signed(grow_size)?;
        let new_vpn: VirtPageNum = VirtAddr::from(new_addr).ceil();
        let old_vpn: VirtPageNum = VirtAddr::from(user_heappoint).ceil();
        if grow_size > 0 {
            let user_vpn_top: VirtPageNum = ((user_heapbottom + USER_HEAP_SIZE) / PAGE_SIZE).into();
            if new_vpn >= user_vpn_top {
                return None;
            }
            if old_vpn >= new_vpn {
                return Some(new_addr);
            }

            // A MAP_FIXED mapping may occupy part of the logical brk span.
            // Keep the user-visible brk pointer independent from those holes:
            // only materialize Brk VMAs in the newly grown, currently free
            // subranges. This yields the Linux layout "brk | mmap | brk".
            let mut free_ranges = Vec::new();
            let mut cursor = old_vpn;
            for area in &self.areas {
                if area.area_type == MapAreaType::Brk {
                    continue;
                }
                let (start, end) = area.vpn_range.range();
                if end <= cursor || start >= new_vpn {
                    continue;
                }
                if cursor < start {
                    free_ranges.push((cursor, start.min(new_vpn)));
                }
                if end > cursor {
                    cursor = end.min(new_vpn);
                }
                if cursor >= new_vpn {
                    break;
                }
            }
            if cursor < new_vpn {
                free_ranges.push((cursor, new_vpn));
            }
            for (start, end) in free_ranges {
                if start < end {
                    self.push_lazily(MapArea::new(
                        VirtAddr::from(start),
                        VirtAddr::from(end),
                        MapType::Framed,
                        MapPermission::R | MapPermission::W | MapPermission::U,
                        MapAreaType::Brk,
                    ));
                }
            }
        } else if grow_size < 0 {
            if new_addr < user_heapbottom {
                return None;
            }
            self.remove_brk_range(new_vpn, old_vpn);
        }
        Some(new_addr)
    }

    /// 仅删除指定范围覆盖的 Brk 区域部分，并保留无关的固定映射。
    ///
    /// 该方法会解除被覆盖 VPN 的页表映射，并根据删除位置将原 VMA 完整删除、
    /// 截短为左半段或右半段，必要时拆成两个区域。逻辑 `brk` 指针由
    /// `TaskInner` 持有，因此调用者可以借此在堆布局中创建或收缩空洞。
    pub(crate) fn remove_brk_range(&mut self, start_vpn: VirtPageNum, end_vpn: VirtPageNum) {
        if start_vpn >= end_vpn {
            return;
        }
        while let Some(idx) = self.areas.iter().position(|area| {
            if area.area_type != MapAreaType::Brk {
                return false;
            }
            let (start, end) = area.vpn_range.range();
            start < end_vpn && end > start_vpn
        }) {
            let (area_start, area_end) = self.areas[idx].vpn_range.range();
            let removed_start = area_start.max(start_vpn);
            let removed_end = area_end.min(end_vpn);
            for vpn in VPNRange::new(removed_start, removed_end) {
                self.areas[idx].unmap_one(&mut self.page_table, vpn);
            }

            if area_start >= start_vpn && area_end <= end_vpn {
                self.areas.remove(idx);
            } else if area_start < start_vpn && area_end <= end_vpn {
                self.areas[idx].vpn_range = VPNRange::new(area_start, start_vpn);
            } else if area_start >= start_vpn && area_end > end_vpn {
                self.areas[idx].vpn_range = VPNRange::new(end_vpn, area_end);
            } else {
                let mut right_area = MapArea::from_another(&self.areas[idx]);
                right_area.vpn_range = VPNRange::new(end_vpn, area_end);
                let right_keys: Vec<VirtPageNum> = self.areas[idx]
                    .data_frames
                    .range(end_vpn..)
                    .map(|(vpn, _)| *vpn)
                    .collect();
                for vpn in right_keys {
                    if let Some(frame) = self.areas[idx].data_frames.remove(&vpn) {
                        right_area.data_frames.insert(vpn, frame);
                    }
                }
                self.areas[idx].vpn_range = VPNRange::new(area_start, start_vpn);
                self.insert_area_sorted_unmerged(right_area);
            }
        }
    }

    /// 将快照页面复制到懒分配区域，按需建立目标页映射。
    ///
    /// `source_pages` 提供源 VPN 与物理帧的配对。目标区域必须已经登记且
    /// 起始 VPN 匹配；目标页尚未映射时先通过 `map_one` 分配，随后复制完整
    /// 页面内容。超出目标区域或目标页分配失败的项目会被跳过。
    pub fn lazy_clone_area(
        &mut self,
        start_vpn: VirtPageNum,
        source_pages: &[(VirtPageNum, Arc<FrameTracker>)],
    ) {
        let (areas, this_page_table) = (&mut self.areas, &mut self.page_table);
        let this_area = if let Some(area) = areas
            .iter_mut()
            .find(|area| area.vpn_range.start() == start_vpn)
        {
            area
        } else {
            return;
        };
        for (vpn, source_frame) in source_pages {
            if !this_area.vpn_range.contains_vpn(*vpn) {
                continue;
            }

            let dst_ppn = match this_page_table.translate(*vpn) {
                Some(ppn) => ppn,
                None => match this_area.map_one(this_page_table, *vpn) {
                    Some(ppn) => ppn,
                    None => continue,
                },
            };

            dst_ppn
                .bytes_array_mut()
                .copy_from_slice(source_frame.ppn.bytes_array());
        }
    }

    /// 按起始 VPN 插入区域，但不与相邻区域合并。
    ///
    /// 返回插入后的索引。VMA 拆分等变换路径在中间状态需要保持两个区域
    /// 独立，若此时合并会撤销拆分结果，并可能使调用者持有的区域索引失效。
    pub(super) fn insert_area_sorted_unmerged(&mut self, map_area: MapArea) -> usize {
        let start_vpn = map_area.vpn_range.start();
        let index = self
            .areas
            .binary_search_by_key(&start_vpn, |area| area.vpn_range.start())
            .unwrap_or_else(|index| index);
        self.areas.insert(index, map_area);
        index
    }

    /// 按起始 VPN 插入区域，并合并兼容的相邻匿名私有 VMA。
    ///
    /// `areas` 的排序和相邻区域合并由该辅助函数统一维护，普通分配路径
    /// 因而不会重新引入无序区域。只有映射类型、权限、区域类型、mmap 标志
    /// 等属性完全一致且两侧均为匿名私有映射时才允许合并。
    fn insert_area_sorted(&mut self, map_area: MapArea) {
        let index = self.insert_area_sorted_unmerged(map_area);
        self.merge_adjacent_areas(index);
    }

    /// 判断两个相邻区域是否可以安全合并。
    ///
    /// 除了地址连续外，映射类型、权限、区域类型和 mmap 标志必须一致；
    /// 带有组标识或文件后端的区域不会合并，以避免改变共享、文件映射等语义。
    fn can_merge_areas(left: &MapArea, right: &MapArea) -> bool {
        left.vpn_range.end() == right.vpn_range.start()
            && left.map_type == right.map_type
            && left.map_perm == right.map_perm
            && left.area_type == right.area_type
            && left.mmap_flags == right.mmap_flags
            && left.groupid == 0
            && right.groupid == 0
            && left.mmap_file.is_anonymous()
            && right.mmap_file.is_anonymous()
            && left.mmap_flags.contains(MmapFlags::MAP_PRIVATE)
            && left.mmap_flags.contains(MmapFlags::MAP_ANONYMOUS)
    }

    /// 从给定索引开始反复合并左右两侧的兼容相邻区域。
    ///
    /// 每次合并都会迁移右侧区域持有的页帧记录，并继续检查新的邻接关系，
    /// 直到左右两侧都不存在可合并区域。
    fn merge_adjacent_areas(&mut self, mut index: usize) {
        loop {
            if index > 0 && Self::can_merge_areas(&self.areas[index - 1], &self.areas[index]) {
                let mut right = self.areas.remove(index);
                let left = &mut self.areas[index - 1];
                left.vpn_range = VPNRange::new(left.vpn_range.start(), right.vpn_range.end());
                left.data_frames.append(&mut right.data_frames);
                index -= 1;
                continue;
            }
            if index + 1 < self.areas.len()
                && Self::can_merge_areas(&self.areas[index], &self.areas[index + 1])
            {
                let mut right = self.areas.remove(index + 1);
                let left = &mut self.areas[index];
                left.vpn_range = VPNRange::new(left.vpn_range.start(), right.vpn_range.end());
                left.data_frames.append(&mut right.data_frames);
                continue;
            }
            break;
        }
    }

    /// 立即分配 `MapArea` 覆盖的页面并将其插入地址空间。
    ///
    /// 映射成功后，如果提供了 `data`，会从区域起始位置复制初始化内容；
    /// 任一步骤失败都会返回 `Err(())`，成功区域会按起始 VPN 排序并尝试合并。
    pub(crate) fn push(&mut self, mut map_area: MapArea, data: Option<&[u8]>) -> Result<(), ()> {
        map_area.map(&mut self.page_table)?;
        if let Some(data) = data {
            map_area.copy_data(&mut self.page_table, data, 0);
        }
        self.insert_area_sorted(map_area);
        Ok(())
    }

    /// 立即分配 `MapArea`，并从指定 `offset` 开始复制初始化数据。
    ///
    /// `offset` 是区域内部的字节偏移，适用于需要将文件或镜像内容放置在
    /// 映射区域中间的场景。映射和数据复制失败时返回 `Err(())`。
    pub(crate) fn push_with_offset(
        &mut self,
        mut map_area: MapArea,
        offset: usize,
        data: Option<&[u8]>,
    ) -> Result<(), ()> {
        map_area.map(&mut self.page_table)?;
        if let Some(data) = data {
            map_area.copy_data(&mut self.page_table, data, offset);
        }
        self.insert_area_sorted(map_area);
        Ok(())
    }

    /// 使用已分配的物理帧插入一个区域。
    ///
    /// 该路径不会重新分配页面，而是把 `frames` 交给 `MapArea` 建立映射，
    /// 适用于 fork、共享内存或其他已经准备好页帧所有权的复制流程。插入后
    /// 仍会维护区域排序并尝试合并兼容邻接区域。
    pub(crate) fn push_with_given_frames(
        &mut self,
        mut map_area: MapArea,
        frames: Vec<Arc<FrameTracker>>,
    ) {
        map_area.map_given_frames(&mut self.page_table, frames);
        self.insert_area_sorted(map_area);
    }

    /// 仅登记一个 `MapArea`，不立即建立页面映射。
    ///
    /// 该方法用于懒分配区域：页面在后续缺页处理时才会实际分配。区域会
    /// 按起始 VPN 插入，并与属性兼容的相邻匿名私有区域合并。
    pub fn push_lazily(&mut self, map_area: MapArea) {
        self.insert_area_sorted(map_area);
    }
}
