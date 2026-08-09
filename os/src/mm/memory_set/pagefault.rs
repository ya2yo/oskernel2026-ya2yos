//! 用户地址空间的缺页异常处理逻辑。
//!
//! 本模块负责处理 mmap 文件页、匿名页、栈向下增长以及写时复制相关的缺页
//! 异常。文件页的准备工作可以在外层释放地址空间锁后完成，实际页表更新则在
//! [`MemorySetInner`] 的锁保护范围内执行。

use super::{MapArea, MapAreaType, MapPermission, MemorySetInner, VPNRange, VirtPageNum};
use crate::arch::page_table::PageTable;
use crate::fs::FilePage;
use crate::mm::page_fault_handler::{
    lazy_page_fault, mmap_read_page_fault, mmap_write_page_fault, write_protect_page_fault,
};
use crate::syscall::MmapFlags;
use crate::trap::trap_types::*;
use alloc::sync::Arc;

const STACK_GUARD_GAP_PAGES: usize = 256;

/// 处理 mmap VMA 中尚未建立页表映射的缺页异常。
///
/// 根据异常类型和区域权限选择文件读缺页或写缺页路径。文件 EOF 之后的完整页
/// 不会被错误地按匿名零页分配，而是由底层缺页处理报告总线错误。
fn handle_mmap_not_present_page_fault(
    page_table: &mut PageTable,
    area: &mut MapArea,
    vpn: VirtPageNum,
    scause: Trap,
    prepared: Option<&Arc<FilePage>>,
) -> bool {
    // 文件 VMA 可以合法地覆盖文件 EOF 之后的字节，但访问完全位于 EOF 之后的
    // 页必须产生 SIGBUS，不能将其当作按需分配的零页处理。
    match scause {
        Trap::Exception(Exception::LoadPageFault) => {
            area.map_perm.contains(MapPermission::R)
                && mmap_read_page_fault(vpn.into(), page_table, area, prepared)
        }
        Trap::Exception(Exception::FetchInstructionPageFault) => {
            area.map_perm.contains(MapPermission::X)
                && mmap_read_page_fault(vpn.into(), page_table, area, prepared)
        }
        Trap::Exception(Exception::PagePrivilegeIllegal) => {
            if area
                .map_perm
                .intersects(MapPermission::R | MapPermission::X)
            {
                mmap_read_page_fault(vpn.into(), page_table, area, prepared)
            } else {
                area.map_perm.contains(MapPermission::W)
                    && mmap_write_page_fault(vpn.into(), page_table, area, prepared)
            }
        }
        _ => {
            area.map_perm.contains(MapPermission::W)
                && mmap_write_page_fault(vpn.into(), page_table, area, prepared)
        }
    }
}

impl MemorySetInner {
    /// Return whether a present COW fault needs a new physical frame.
    ///
    /// The answer is sampled while the caller holds the address-space write
    /// lock, before it pins the old frame for a remote invalidation.  Taking
    /// that pin first would make an otherwise exclusive frame look shared to
    /// `Arc::strong_count`, forcing an unnecessary copy-on-write split.
    pub(crate) fn cow_fault_requires_frame_copy(
        &self,
        vpn: VirtPageNum,
        scause: Trap,
    ) -> Option<bool> {
        if matches!(
            scause,
            Trap::Exception(Exception::LoadPageFault | Exception::FetchInstructionPageFault)
        ) || !self.page_table.is_cow_page(vpn)
        {
            return None;
        }

        self.areas
            .iter()
            .filter(|area| {
                matches!(
                    area.area_type,
                    MapAreaType::Elf | MapAreaType::Brk | MapAreaType::Mmap | MapAreaType::Stack
                )
            })
            .find(|area| area.vpn_range.contains_vpn(vpn))
            .map(|area| {
                area.data_frames
                    .get(&vpn)
                    .map(|frame| Arc::strong_count(frame) > 1)
                    // A COW PTE without a tracked frame is retained for
                    // compatibility with forked brk mappings. The page-table
                    // handler already uses the same conservative copy path.
                    .unwrap_or(true)
            })
    }

    /// 处理用户地址空间中的缺页异常。
    ///
    /// 先尝试处理尚未建立页表映射的延迟分配、文件映射和栈增长，再处理写保护
    /// 缺页（例如写时复制）。返回 `true` 表示异常已经修复，返回 `false` 表示
    /// 当前地址空间无法处理该异常。
    pub fn handle_page_fault(
        &mut self,
        vpn: VirtPageNum,
        scause: Trap,
        prepared: Option<Arc<FilePage>>,
    ) -> bool {
        if self.handle_not_present_page_fault(vpn, scause, prepared.as_ref()) {
            return true;
        }
        self.handle_write_protect_page_fault(vpn, scause)
    }

    /// 处理目标页尚未建立页表映射的缺页异常。
    ///
    /// 根据 VMA 类型分别执行 mmap 文件页载入、匿名页延迟分配，或按需扩展
    /// `MAP_GROWSDOWN` 匿名映射。`prepared` 是调用者在释放外层锁期间准备好的
    /// 文件页缓存页。
    fn handle_not_present_page_fault(
        &mut self,
        vpn: VirtPageNum,
        scause: Trap,
        prepared: Option<&Arc<FilePage>>,
    ) -> bool {
        let ppn = self.page_table.translate(vpn);
        if !ppn.is_none() {
            return false;
        }
        // 文件映射。
        if let Some(area) = self
            .areas
            .iter_mut()
            .filter(|area| area.area_type == MapAreaType::Mmap)
            .find(|area| {
                let (start, end) = area.vpn_range.range();
                start <= vpn && vpn < end
            })
        {
            return handle_mmap_not_present_page_fault(
                &mut self.page_table,
                area,
                vpn,
                scause,
                prepared,
            );
        }
        // brk、固定栈，或注册为延迟加载的 ELF BSS 尾部。
        if let Some(area) = self
            .areas
            .iter_mut()
            .filter(|area| {
                area.area_type == MapAreaType::Brk
                    || area.area_type == MapAreaType::Stack
                    || area.area_type == MapAreaType::Elf
            })
            .find(|area| {
                let (start, end) = area.vpn_range.range();
                start <= vpn && vpn < end
            })
        {
            let allowed = match scause {
                Trap::Exception(Exception::LoadPageFault) => {
                    area.map_perm.contains(MapPermission::R)
                }
                Trap::Exception(Exception::FetchInstructionPageFault) => {
                    area.map_perm.contains(MapPermission::X)
                }
                Trap::Exception(Exception::PagePrivilegeIllegal) => area
                    .map_perm
                    .intersects(MapPermission::R | MapPermission::W | MapPermission::X),
                _ => area.map_perm.contains(MapPermission::W),
            };
            return allowed && lazy_page_fault(vpn.into(), &mut self.page_table, area);
        }

        // Linux 在访问匿名 MAP_GROWSDOWN VMA 的保护页时会向下扩展该 VMA。
        // 不能跨越其他 VMA 扩展，并且要与最近的下方映射保持默认的 256 页栈
        // 保护间隔。
        if self
            .areas
            .iter()
            .any(|area| area.vpn_range.contains_vpn(vpn))
        {
            return false;
        }
        let Some((growdown_idx, growdown_start, growdown_end)) = self
            .areas
            .iter()
            .enumerate()
            .filter(|(_, area)| {
                area.area_type == MapAreaType::Mmap
                    && area.mmap_flags.contains(MmapFlags::MAP_GROWSDOWN)
                    && area.mmap_flags.contains(MmapFlags::MAP_PRIVATE)
                    && area.mmap_file.file.is_none()
                    && vpn < area.vpn_range.start()
            })
            .map(|(idx, area)| (idx, area.vpn_range.start(), area.vpn_range.end()))
            .min_by_key(|(_, start, _)| start.0)
        else {
            return false;
        };

        let overlaps_growth = self.areas.iter().enumerate().any(|(idx, area)| {
            if idx == growdown_idx {
                return false;
            }
            let (start, end) = area.vpn_range.range();
            start < growdown_start && vpn < end
        });
        if overlaps_growth {
            return false;
        }

        let lower_vma_end = self
            .areas
            .iter()
            .enumerate()
            .filter(|(idx, area)| *idx != growdown_idx && area.vpn_range.end() <= vpn)
            .map(|(_, area)| area.vpn_range.end())
            .max_by_key(|end| end.0);
        if lower_vma_end.is_some_and(|end| vpn.0 - end.0 < STACK_GUARD_GAP_PAGES) {
            return false;
        }

        let (page_table, areas) = (&mut self.page_table, &mut self.areas);
        let area = &mut areas[growdown_idx];
        if !handle_mmap_not_present_page_fault(page_table, area, vpn, scause, prepared) {
            return false;
        }
        area.vpn_range = VPNRange::new(vpn, growdown_end);
        true
    }

    /// 处理写保护页引起的缺页异常，例如写时复制或恢复可写权限。
    fn handle_write_protect_page_fault(&mut self, vpn: VirtPageNum, scause: Trap) -> bool {
        // 只有存储或页修改异常可以通过写时复制或恢复写权限修复；加载和取指
        // 权限异常必须继续报告为 SIGSEGV。
        if scause == Trap::Exception(Exception::LoadPageFault)
            || scause == Trap::Exception(Exception::FetchInstructionPageFault)
        {
            return false;
        }
        if let Some(area) = self
            .areas
            .iter_mut()
            .filter(|area| {
                area.area_type == MapAreaType::Elf
                    || area.area_type == MapAreaType::Brk
                    || area.area_type == MapAreaType::Mmap
                    || area.area_type == MapAreaType::Stack
            })
            .find(|area| {
                let (start, end) = area.vpn_range.range();
                start <= vpn && vpn < end
            })
        {
            if write_protect_page_fault(vpn.into(), &mut self.page_table, area) {
                return true;
            }
        }
        false
    }
}
