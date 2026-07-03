//! ELF loading into a [`MemorySetInner`].
//!
//! Contains `from_elf()` which parses an ELF binary and creates the initial
//! user address space (program headers, heap/Brk area), plus the dynamic
//! linker / INTERP handling.

use super::super::map_area::MapType;
use super::{MapArea, MapAreaType, MapPermission, VirtAddr, VirtPageNum};
use crate::arch::memory_layout::{DL_INTERP_OFFSET, PAGE_SIZE, USER_HEAP_SIZE};
use crate::fs::{
    map_dynamic_link_file_directly_map, open_direct, File, Inode, OpenFlags, NONE_MODE,
};
use crate::mm::memory_set::MemorySetInner;
use crate::task::{Aux, AuxType};
use crate::utils::SysErrNo;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use xmas_elf::ElfFile;

const ELF_PROBE_SIZE: usize = 256;

fn read_inode_prefix(inode: &Arc<dyn Inode>, len: usize) -> Result<Vec<u8>, SysErrNo> {
    let read_len = len.min(inode.size());
    let mut data = alloc::vec![0u8; read_len];
    let mut done = 0;
    while done < read_len {
        let read = inode.read_at(done, &mut data[done..])?;
        if read == 0 {
            break;
        }
        done += read;
    }
    data.truncate(done);
    Ok(data)
}

fn program_headers_end(elf: &ElfFile) -> Result<usize, SysErrNo> {
    let ph_offset = elf.header.pt2.ph_offset() as usize;
    let ph_count = elf.header.pt2.ph_count() as usize;
    let ph_entry_size = elf.header.pt2.ph_entry_size() as usize;
    ph_entry_size
        .checked_mul(ph_count)
        .and_then(|size| ph_offset.checked_add(size))
        .ok_or(SysErrNo::ENOEXEC)
}

fn needed_elf_prefix_len(elf_data: &[u8]) -> Result<usize, SysErrNo> {
    let elf = ElfFile::new(elf_data).map_err(|_| SysErrNo::ENOEXEC)?;
    let ph_end = program_headers_end(&elf)?;
    if elf_data.len() < ph_end {
        return Err(SysErrNo::ENOEXEC);
    }

    let mut needed = ph_end;
    for idx in 0..elf.header.pt2.ph_count() {
        let ph = elf.program_header(idx).map_err(|_| SysErrNo::ENOEXEC)?;
        let ph_type = ph.get_type().map_err(|_| SysErrNo::ENOEXEC)?;
        if matches!(
            ph_type,
            xmas_elf::program::Type::Load | xmas_elf::program::Type::Interp
        ) {
            let end = (ph.offset() as usize)
                .checked_add(ph.file_size() as usize)
                .ok_or(SysErrNo::ENOEXEC)?;
            needed = needed.max(end);
        }
    }
    Ok(needed)
}

/// Read only the ELF bytes required by the loader.
///
/// Contest images may contain large static binaries with debug sections after
/// the loadable segments. `execve` only needs the ELF header, program headers,
/// PT_INTERP bytes, and PT_LOAD file ranges, so avoid copying the whole file
/// into the kernel heap.
pub(crate) fn read_elf_load_image(inode: &Arc<dyn Inode>) -> Result<Vec<u8>, SysErrNo> {
    let head = read_inode_prefix(inode, ELF_PROBE_SIZE)?;
    if head.len() < 4 || head[0] != 0x7f || head[1] != b'E' || head[2] != b'L' || head[3] != b'F' {
        return Err(SysErrNo::ENOEXEC);
    }

    let header_elf = ElfFile::new(&head).map_err(|_| SysErrNo::ENOEXEC)?;
    let ph_end = program_headers_end(&header_elf)?;
    let ph_data = if head.len() < ph_end {
        read_inode_prefix(inode, ph_end)?
    } else {
        head
    };
    if ph_data.len() < ph_end {
        return Err(SysErrNo::ENOEXEC);
    }

    let needed = needed_elf_prefix_len(&ph_data)?;
    let image = if ph_data.len() < needed {
        read_inode_prefix(inode, needed)?
    } else {
        ph_data
    };
    if image.len() < needed {
        return Err(SysErrNo::ENOEXEC);
    }
    Ok(image)
}

impl MemorySetInner {
    /// 如果当前 ELF 是动态链接程序，则加载它声明的动态解释器。
    ///
    /// ELF 的 `PT_INTERP` 段通常指向动态链接器路径，例如
    /// `/lib/ld-musl-riscv64.so.1`。这类程序不能直接从自身 entry
    /// point 开始执行，而是要先跳到动态链接器，由动态链接器完成共享库
    /// 重定位后再进入主程序。
    ///
    /// 本函数只处理解释器本身：
    ///
    /// - 没有 `PT_INTERP` 时返回 `Ok(None)`，表示这是静态 ELF，调用者应
    ///   使用主程序自己的 entry point。
    /// - 有 `PT_INTERP` 时，先按竞赛镜像兼容规则映射解释器路径；如果映射
    ///   路径不存在，再按 ELF 中写明的原始路径直接打开。这是为了同时兼容
    ///   `/musl/lib/libc.so` 测试镜像和 Alpine 的 `/lib/ld-musl-*.so.1`。
    /// - 成功后把解释器 ELF 映射到固定的 `DL_INTERP_OFFSET`，并返回解释器
    ///   的实际入口地址 `Some(interp_entry + DL_INTERP_OFFSET)`。
    /// - 解释器存在但读取、解析或映射失败时返回 `Err(())`。这类错误不能被
    ///   当作“静态 ELF”继续执行，否则会从错误地址进入用户态。
    ///
    /// 注意：这里不解析共享库依赖，也不做重定位；这些工作属于用户态动态
    /// 链接器。内核只负责把动态链接器本身放进新地址空间，并通过 auxv 告诉
    /// 它主程序的元信息。
    fn load_dl_interp_if_needed(&mut self, elf: &ElfFile) -> Result<Option<usize>, ()> {
        let elf_header = elf.header;
        let ph_count = elf_header.pt2.ph_count();

        // 先扫描 program header。`PT_INTERP` 是动态链接 ELF 的标志；
        // 没有这个 header 时，主程序可以直接从自己的 entry point 启动。
        let mut interp = None;
        for i in 0..ph_count {
            let ph = elf.program_header(i).unwrap();
            if ph.get_type().unwrap() == xmas_elf::program::Type::Interp {
                let start = ph.offset() as usize;
                let end = start.checked_add(ph.file_size() as usize).ok_or(())?;
                if end > elf.input.len() {
                    return Err(());
                }
                let raw = &elf.input[start..end];
                let path_len = raw.iter().position(|v| *v == 0).unwrap_or(raw.len());
                let path = core::str::from_utf8(&raw[..path_len]).map_err(|_| ())?;
                interp = Some(path.to_string());
                break;
            }
        }

        if let Some(interp) = interp {
            // 先按竞赛测试镜像的兼容规则映射动态链接器路径；如果映射路径
            // 不存在，再按 ELF 原始 `.interp` 路径打开，兼容 Alpine 等标准布局。
            let mapped_interp = map_dynamic_link_file_directly_map(&interp);
            let interp_file = open_direct(mapped_interp, OpenFlags::O_RDONLY, NONE_MODE)
                .ok()
                .and_then(|file| file.file().ok())
                .or_else(|| {
                    // 映射路径和原始路径相同则不用重复打开。
                    if mapped_interp == interp {
                        None
                    } else {
                        open_direct(&interp, OpenFlags::O_RDONLY, NONE_MODE)
                            .ok()
                            .and_then(|file| file.file().ok())
                    }
                })
                .ok_or(())?;
            // 动态解释器本身也是一个 ELF。读入并解析后复用 `map_elf()`，
            // 只是在地址空间中整体平移到 `DL_INTERP_OFFSET`。
            let interp_elf_data = read_elf_load_image(&interp_file.inode).map_err(|_| ())?;
            let interp_elf = xmas_elf::ElfFile::new(&interp_elf_data).map_err(|_| ())?;
            self.map_elf(&interp_elf, DL_INTERP_OFFSET.into())?;

            // 动态链接程序的第一条用户态指令应来自解释器入口。
            Ok(Some(
                interp_elf.header.pt2.entry_point() as usize + DL_INTERP_OFFSET,
            ))
        } else {
            Ok(None)
        }
    }

    /// 将一个 ELF 文件中的所有 `PT_LOAD` 段映射进当前地址空间。
    ///
    /// `PT_LOAD` 是 ELF 中真正需要进入内存的段，通常对应代码段、只读数据段、
    /// 可写数据段以及 `.bss` 所在的内存范围。本函数遍历 program header：
    ///
    /// - 根据 `ph.virtual_addr()` 和传入的 `offset` 计算最终用户虚拟地址。
    ///   主程序使用 `offset = 0`，动态解释器使用 `DL_INTERP_OFFSET`，避免和
    ///   主程序地址范围冲突。
    /// - 根据 ELF 段标志生成页权限：`R/W/X` 加用户态 `U`。
    /// - 用 `MapAreaType::Elf` 建立 framed 映射，并把文件中实际存在的字节
    ///   写入对应页；`mem_size > file_size` 的尾部区域自然保留为零页内容，
    ///   用于承载 `.bss`。
    ///
    /// 返回值为 `(max_end_vpn, header_va)`：
    ///
    /// - `max_end_vpn` 是本次映射的最高结束页号，调用者用它决定 brk/heap
    ///   应该放在 ELF 段之后。
    /// - `header_va` 是第一个 `PT_LOAD` 段的起始虚拟地址。当前 loader 用它
    ///   加上 `e_phoff` 计算 `AT_PHDR`，传给动态链接器和 libc 启动代码。
    fn map_elf(&mut self, elf: &ElfFile, offset: VirtAddr) -> Result<(VirtPageNum, VirtAddr), ()> {
        let elf_header = elf.header;
        let ph_count = elf_header.pt2.ph_count();

        let mut max_end_vpn = offset.floor();
        let mut header_va = 0;
        let mut has_found_header_va = false;

        for i in 0..ph_count {
            let ph = elf.program_header(i).unwrap();
            // 只映射 `PT_LOAD` 段。其他 program header 只提供元信息，
            // 例如 `PT_INTERP` 已在加载动态解释器时处理过。
            if ph.get_type().unwrap() == xmas_elf::program::Type::Load {
                // ELF 段声明的是运行时虚拟地址。主程序不平移，动态解释器
                // 会加上 `DL_INTERP_OFFSET`，避免和主程序地址范围冲突。
                let start_va: VirtAddr = (ph.virtual_addr() as usize + offset.0).into();
                let end_va: VirtAddr =
                    ((ph.virtual_addr() + ph.mem_size()) as usize + offset.0).into();
                if !has_found_header_va {
                    // 第一个 LOAD 段通常覆盖 ELF header 和 program header。
                    // 保存它的地址，后面用于计算 auxv 中的 `AT_PHDR`。
                    header_va = start_va.0;
                    has_found_header_va = true;
                }
                // 页权限来自 ELF 段 flags，再加上 U 表示用户态可访问。
                let mut map_perm = MapPermission::U;
                let ph_flags = ph.flags();
                if ph_flags.is_read() {
                    map_perm |= MapPermission::R;
                }
                if ph_flags.is_write() {
                    map_perm |= MapPermission::W;
                }
                if ph_flags.is_execute() {
                    map_perm |= MapPermission::X;
                }
                let map_area = MapArea::new(
                    start_va,
                    end_va,
                    MapType::Framed,
                    map_perm,
                    MapAreaType::Elf,
                );
                // `push_with_offset()` 以页为单位建立映射，但 ELF 段起始地址
                // 不一定页对齐，因此需要记录文件数据在第一页里的页内偏移。
                let data_offset = start_va.0 - start_va.floor().0 * PAGE_SIZE;
                max_end_vpn = map_area.vpn_range.end();
                // 只拷贝文件里实际存在的 `file_size` 字节。若 `mem_size`
                // 更大，剩余内存保持为 0，正好用于 `.bss`。
                self.push_with_offset(
                    map_area,
                    data_offset,
                    Some(&elf.input[ph.offset() as usize..(ph.offset() + ph.file_size()) as usize]),
                )?;
            }
        }
        Ok((max_end_vpn, header_va.into()))
    }

    /// 从 ELF 字节创建一个新的用户地址空间。
    ///
    /// 这是 `execve()` 和初始进程创建用户地址空间时使用的入口。它会完成：
    ///
    /// - 创建带内核映射的新 `MemorySetInner`。
    /// - 解析主程序 ELF header，并检查 magic。
    /// - 如果主程序是动态链接 ELF，先通过 [`Self::load_dl_interp_if_needed`]
    ///   映射动态解释器，并把最终入口切换为动态解释器入口。
    /// - 通过 [`Self::map_elf`] 映射主程序的所有 `PT_LOAD` 段。
    /// - 在 ELF 段之后预留一页 guard page，再创建初始 brk 区域。
    /// - 构造 auxv。用户态 libc / 动态链接器会读取这些值来获得 program
    ///   header 地址、主程序入口、页大小、动态解释器基址等信息。
    ///
    /// 返回 `(memory_set, user_heap_bottom, entry_point, auxv)`：
    ///
    /// - `memory_set`：已经包含内核映射、主程序 ELF 段、可选动态解释器和 brk
    ///   区域的新地址空间。
    /// - `user_heap_bottom`：brk 初始位置，也是后续 `brk()` 增长的起点。
    /// - `entry_point`：真正写入 trap context 的用户入口。静态 ELF 是主程序
    ///   entry；动态 ELF 是动态解释器 entry。
    /// - `auxv`：放到用户栈上的 auxiliary vector。
    ///
    /// 本函数不负责用户栈、trap context 和 trampoline 的最终布置；这些由
    /// 调用方在拿到 `memory_set` 后继续完成。
    pub fn from_elf(elf_data: &[u8]) -> Result<(Self, usize, usize, Vec<Aux>), ()> {
        let mut auxv = Vec::new();
        // 新用户地址空间仍必须带内核映射；用户态 trap 进入内核后需要这些映射
        // 才能继续执行内核代码。
        let mut memory_set = Self::new_from_kernel();
        let elf = xmas_elf::ElfFile::new(elf_data).unwrap();
        let elf_header = elf.header;
        let magic = elf_header.pt1.magic;
        assert_eq!(magic, [0x7f, 0x45, 0x4c, 0x46], "invalid elf!");
        let ph_count = elf_header.pt2.ph_count();
        // 默认从主程序 entry 启动；若是动态链接 ELF，下面会改成解释器 entry。
        let mut entry_point = elf.header.pt2.entry_point() as usize;

        // 这些 auxv 项描述 program header 表的格式和系统页大小。
        // libc 启动代码和动态链接器会从用户栈读取它们。
        auxv.push(Aux::new(
            AuxType::PHENT,
            elf.header.pt2.ph_entry_size() as usize,
        ));
        auxv.push(Aux::new(AuxType::PHNUM, ph_count as usize));
        auxv.push(Aux::new(AuxType::PAGESZ, PAGE_SIZE as usize));
        if let Some(interp_entry_point) = memory_set.load_dl_interp_if_needed(&elf)? {
            // `AT_BASE` 记录动态解释器的加载基址；动态链接器用它定位自身。
            auxv.push(Aux::new(AuxType::BASE, DL_INTERP_OFFSET));
            // 动态链接程序先进入解释器，由解释器完成重定位后跳回主程序。
            entry_point = interp_entry_point;
        } else {
            // 静态 ELF 没有动态解释器，Linux 语义下 `AT_BASE` 为 0。
            auxv.push(Aux::new(AuxType::BASE, 0));
        }
        auxv.push(Aux::new(AuxType::FLAGS, 0 as usize));
        // `AT_ENTRY` 始终是主程序入口，即使实际 trap context 先跳到解释器。
        auxv.push(Aux::new(
            AuxType::ENTRY,
            elf.header.pt2.entry_point() as usize,
        ));
        // 当前内核还没有完整凭据模型，这里先给用户态提供 root 身份相关 auxv。
        auxv.push(Aux::new(AuxType::UID, 0 as usize));
        auxv.push(Aux::new(AuxType::EUID, 0 as usize));
        auxv.push(Aux::new(AuxType::GID, 0 as usize));
        auxv.push(Aux::new(AuxType::EGID, 0 as usize));
        // 平台字符串、硬件能力、secure exec 等字段先用最小兼容值。
        auxv.push(Aux::new(AuxType::PLATFORM, 0 as usize));
        auxv.push(Aux::new(AuxType::HWCAP, 0 as usize));
        auxv.push(Aux::new(AuxType::CLKTCK, 100 as usize));
        auxv.push(Aux::new(AuxType::SECURE, 0 as usize));
        auxv.push(Aux::new(AuxType::NOTELF, 0x112d as usize));

        // 主程序按 ELF 自己声明的虚拟地址映射，所以 offset 为 0。
        let (max_end_vpn, head_va) = memory_set.map_elf(&elf, VirtAddr(0))?;

        // `AT_PHDR` 指向用户虚拟地址中的 program header 表。
        // `head_va` 是包含 ELF header 的 LOAD 段起点，`e_phoff` 是表内偏移。
        let ph_head_addr = head_va.0 + elf.header.pt2.ph_offset() as usize;
        auxv.push(Aux {
            aux_type: AuxType::PHDR,
            value: ph_head_addr as usize,
        });
        let max_end_va: VirtAddr = max_end_vpn.into();
        let mut user_heap_bottom: usize = max_end_va.into();
        // ELF 映射末尾和 brk 区之间留一页 guard，降低越界访问直接碰到堆的概率。
        user_heap_bottom += PAGE_SIZE; // guard page
        let user_heap_top: usize = user_heap_bottom;
        // 初始 brk 长度为 0；后续 `brk()` 扩展时再懒分配实际物理页。
        memory_set.push_lazily(MapArea::new(
            user_heap_bottom.into(),
            user_heap_top.into(),
            MapType::Framed,
            MapPermission::R | MapPermission::W | MapPermission::U,
            MapAreaType::Brk,
        ));

        Ok((memory_set, user_heap_bottom, entry_point, auxv))
    }
}
