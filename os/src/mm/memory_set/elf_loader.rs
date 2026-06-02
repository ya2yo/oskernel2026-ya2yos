//! ELF loading into a [`MemorySetInner`].
//!
//! Contains `from_elf()` which parses an ELF binary and creates the initial
//! user address space (program headers, heap/Brk area), plus the dynamic
//! linker / INTERP handling.

use super::super::map_area::MapType;
use crate::mm::memory_set::MemorySetInner;
use super::{MapArea, MapAreaType, MapPermission, VirtAddr, VirtPageNum};
use crate::arch::memory_layout::{DL_INTERP_OFFSET, PAGE_SIZE, USER_HEAP_SIZE};
use crate::fs::{map_dynamic_link_file_directly_map, open, File, OpenFlags, NONE_MODE};
use crate::task::{Aux, AuxType};
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use xmas_elf::ElfFile;

impl MemorySetInner {
    fn load_dl_interp_if_needed(&mut self, elf: &ElfFile) -> Option<usize> {
        let elf_header = elf.header;
        let ph_count = elf_header.pt2.ph_count();

        let mut is_dl = false;
        for i in 0..ph_count {
            let ph = elf.program_header(i).unwrap();
            if ph.get_type().unwrap() == xmas_elf::program::Type::Interp {
                is_dl = true;
                break;
            }
        }

        if is_dl {
            let section = elf.find_section_by_name(".interp").unwrap();
            let mut interp = String::from_utf8(section.raw_data(&elf).to_vec()).unwrap();
            interp = interp.strip_suffix("\0").unwrap_or(&interp).to_string();

            let interp = map_dynamic_link_file_directly_map(&interp);

            let interp_inode = open(&interp, OpenFlags::O_RDONLY, NONE_MODE)
                .unwrap()
                .file()
                .ok();
            let interp_file = interp_inode.unwrap();
            let interp_elf_data = interp_file.inode.read_all().unwrap();
            let interp_elf = xmas_elf::ElfFile::new(&interp_elf_data).unwrap();
            self.map_elf(&interp_elf, DL_INTERP_OFFSET.into()).ok()?;

            Some(interp_elf.header.pt2.entry_point() as usize + DL_INTERP_OFFSET)
        } else {
            None
        }
    }

    fn map_elf(&mut self, elf: &ElfFile, offset: VirtAddr) -> Result<(VirtPageNum, VirtAddr), ()> {
        let elf_header = elf.header;
        let ph_count = elf_header.pt2.ph_count();

        let mut max_end_vpn = offset.floor();
        let mut header_va = 0;
        let mut has_found_header_va = false;

        for i in 0..ph_count {
            let ph = elf.program_header(i).unwrap();
            if ph.get_type().unwrap() == xmas_elf::program::Type::Load {
                let start_va: VirtAddr = (ph.virtual_addr() as usize + offset.0).into();
                let end_va: VirtAddr =
                    ((ph.virtual_addr() + ph.mem_size()) as usize + offset.0).into();
                if !has_found_header_va {
                    header_va = start_va.0;
                    has_found_header_va = true;
                }
                let mut map_perm = MapPermission::U;
                let ph_flags = ph.flags();
                if ph_flags.is_read() { map_perm |= MapPermission::R; }
                if ph_flags.is_write() { map_perm |= MapPermission::W; }
                if ph_flags.is_execute() { map_perm |= MapPermission::X; }
                let map_area = MapArea::new(
                    start_va, end_va,
                    MapType::Framed, map_perm, MapAreaType::Elf,
                );
                let data_offset = start_va.0 - start_va.floor().0 * PAGE_SIZE;
                max_end_vpn = map_area.vpn_range.end();
                self.push_with_offset(
                    map_area, data_offset,
                    Some(&elf.input[ph.offset() as usize..(ph.offset() + ph.file_size()) as usize]),
                )?;
            }
        }
        Ok((max_end_vpn, header_va.into()))
    }

    /// Include sections in elf and trampoline and TrapContext and user stack,
    /// also returns user_sp and entry point.
    /// 不包括用户栈和Trap
    pub fn from_elf(elf_data: &[u8]) -> Result<(Self, usize, usize, Vec<Aux>), ()> {
        let mut auxv = Vec::new();
        let mut memory_set = Self::new_from_kernel();
        let elf = xmas_elf::ElfFile::new(elf_data).unwrap();
        let elf_header = elf.header;
        let magic = elf_header.pt1.magic;
        assert_eq!(magic, [0x7f, 0x45, 0x4c, 0x46], "invalid elf!");
        let ph_count = elf_header.pt2.ph_count();
        let mut entry_point = elf.header.pt2.entry_point() as usize;

        auxv.push(Aux::new(AuxType::PHENT, elf.header.pt2.ph_entry_size() as usize));
        auxv.push(Aux::new(AuxType::PHNUM, ph_count as usize));
        auxv.push(Aux::new(AuxType::PAGESZ, PAGE_SIZE as usize));
        if let Some(interp_entry_point) = memory_set.load_dl_interp_if_needed(&elf) {
            auxv.push(Aux::new(AuxType::BASE, DL_INTERP_OFFSET));
            entry_point = interp_entry_point;
        } else {
            auxv.push(Aux::new(AuxType::BASE, 0));
        }
        auxv.push(Aux::new(AuxType::FLAGS, 0 as usize));
        auxv.push(Aux::new(AuxType::ENTRY, elf.header.pt2.entry_point() as usize));
        auxv.push(Aux::new(AuxType::UID, 0 as usize));
        auxv.push(Aux::new(AuxType::EUID, 0 as usize));
        auxv.push(Aux::new(AuxType::GID, 0 as usize));
        auxv.push(Aux::new(AuxType::EGID, 0 as usize));
        auxv.push(Aux::new(AuxType::PLATFORM, 0 as usize));
        auxv.push(Aux::new(AuxType::HWCAP, 0 as usize));
        auxv.push(Aux::new(AuxType::CLKTCK, 100 as usize));
        auxv.push(Aux::new(AuxType::SECURE, 0 as usize));
        auxv.push(Aux::new(AuxType::NOTELF, 0x112d as usize));

        let (max_end_vpn, head_va) = memory_set.map_elf(&elf, VirtAddr(0))?;

        let ph_head_addr = head_va.0 + elf.header.pt2.ph_offset() as usize;
        auxv.push(Aux { aux_type: AuxType::PHDR, value: ph_head_addr as usize });
        let max_end_va: VirtAddr = max_end_vpn.into();
        let mut user_heap_bottom: usize = max_end_va.into();
        user_heap_bottom += PAGE_SIZE; // guard page
        let user_heap_top: usize = user_heap_bottom;
        memory_set.push_lazily(MapArea::new(
            user_heap_bottom.into(), user_heap_top.into(),
            MapType::Framed, MapPermission::R | MapPermission::W | MapPermission::U,
            MapAreaType::Brk,
        ));

        Ok((memory_set, user_heap_bottom, entry_point, auxv))
    }
}
