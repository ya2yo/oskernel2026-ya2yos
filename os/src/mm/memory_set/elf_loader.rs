//! ELF loading into a [`MemorySetInner`].
//!
//! Contains `from_elf()` which parses an ELF binary and creates the initial
//! user address space (program headers, heap/Brk area), plus the dynamic
//! linker / INTERP handling.

use super::super::map_area::MapType;
use super::{MapArea, MapAreaType, MapPermission, VirtAddr, VirtPageNum};
use crate::arch::memory_layout::{DL_INTERP_OFFSET, PAGE_SIZE, USER_HEAP_SIZE};
#[cfg(feature = "perf")]
use crate::arch::time::get_ticks;
use crate::fs::{open_direct, File, Inode, OSFile, OpenFlags, FILE_PAGE_CACHE, NONE_MODE};
use crate::mm::memory_set::MemorySetInner;
use crate::syscall::MmapFlags;
use crate::task::{Aux, AuxType};
use crate::utils::SysErrNo;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use xmas_elf::ElfFile;

#[cfg(target_arch = "loongarch64")]
const HWCAP_LOONGARCH_UAL: usize = 1 << 2;

/// Return the Linux-compatible hardware capabilities exposed through auxv.
///
/// QEMU's LoongArch TCG backend requires the UAL bit before it can safely use
/// its generated unaligned host memory accesses.
fn elf_hwcap() -> usize {
    #[cfg(target_arch = "loongarch64")]
    {
        return if loongArch64::cpu::get_ual() {
            HWCAP_LOONGARCH_UAL
        } else {
            0
        };
    }

    #[cfg(not(target_arch = "loongarch64"))]
    0
}

const ELF_PROBE_SIZE: usize = 256;
/// Bound temporary storage while coalescing an unaligned ELF segment's
/// formerly page-at-a-time reads. This is large enough to amortize lwext4's
/// global read gate without allowing one executable segment to allocate an
/// unbounded kernel buffer.
const ELF_SEGMENT_READ_CHUNK: usize = 64 * 1024;

fn read_inode_prefix(
    inode: &Arc<dyn Inode>,
    len: usize,
    file_size: usize,
) -> Result<Vec<u8>, SysErrNo> {
    let read_len = len.min(file_size);
    let mut data = alloc::vec![0u8; read_len];
    let mut done = 0;
    while done < read_len {
        let read = inode.read_at(done, &mut data[done..])?;
        #[cfg(feature = "perf")]
        crate::utils::perf::record_inode_read_source(
            crate::utils::perf::InodeReadSource::Other,
            read,
        );
        if read == 0 {
            break;
        }
        done += read;
    }
    data.truncate(done);
    Ok(data)
}

/// Extend an already-read prefix without issuing a second read from offset 0.
///
/// `execve` first reads a small probe to distinguish ELF from a script.  ELF
/// parsing may then require the program headers and the loadable ranges.  The
/// old implementation reread the complete prefix for every extension, which
/// inflated both filesystem traffic and the execve critical path.
fn extend_inode_prefix(
    inode: &Arc<dyn Inode>,
    data: &mut Vec<u8>,
    len: usize,
    file_size: usize,
) -> Result<(), SysErrNo> {
    let target = len.min(file_size);
    if data.len() >= target {
        return Ok(());
    }

    let old_len = data.len();
    data.resize(target, 0);
    let mut done = old_len;
    while done < target {
        let read = inode.read_at(done, &mut data[done..target])?;
        #[cfg(feature = "perf")]
        crate::utils::perf::record_inode_read_source(
            crate::utils::perf::InodeReadSource::Other,
            read,
        );
        if read == 0 {
            break;
        }
        done += read;
    }
    data.truncate(done);
    Ok(())
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

fn needed_elf_metadata_len(elf: &ElfFile) -> Result<usize, SysErrNo> {
    let mut needed = program_headers_end(elf)?;
    for idx in 0..elf.header.pt2.ph_count() {
        let ph = elf.program_header(idx).map_err(|_| SysErrNo::ENOEXEC)?;
        if ph.get_type().map_err(|_| SysErrNo::ENOEXEC)? != xmas_elf::program::Type::Interp {
            continue;
        }
        let end = (ph.offset() as usize)
            .checked_add(ph.file_size() as usize)
            .ok_or(SysErrNo::ENOEXEC)?;
        needed = needed.max(end);
    }
    Ok(needed)
}

/// Validate every file range the ELF loader can consume without copying the
/// segment contents into a temporary image.
fn validate_elf_load_ranges(elf: &ElfFile, file_size: usize) -> Result<(), SysErrNo> {
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
            if end > file_size {
                return Err(SysErrNo::ENOEXEC);
            }
        }
    }
    Ok(())
}

/// Read the ELF header, program-header table, and optional `PT_INTERP` path.
///
/// Loadable segment bytes deliberately remain in the executable file.  Aligned
/// segments can then become private file-backed VMAs; uncommon unaligned ones
/// are read directly into their newly allocated user pages by the loader.
fn read_elf_metadata(inode: &Arc<dyn Inode>) -> Result<Vec<u8>, SysErrNo> {
    let file_size = inode.fstat().st_size.max(0) as usize;
    read_elf_metadata_with_prefix(inode, &[], file_size)
}

/// Extend an ELF probe from `execve` into only the metadata needed to build
/// the initial address space.  Keeping the already-read probe avoids a second
/// read from offset zero while avoiding a kernel-heap copy of every PT_LOAD.
pub(crate) fn read_elf_metadata_with_prefix(
    inode: &Arc<dyn Inode>,
    prefix: &[u8],
    file_size: usize,
) -> Result<Vec<u8>, SysErrNo> {
    let mut metadata = if prefix.is_empty() {
        read_inode_prefix(inode, ELF_PROBE_SIZE, file_size)?
    } else {
        prefix.to_vec()
    };
    if metadata.len() < 4
        || metadata[0] != 0x7f
        || metadata[1] != b'E'
        || metadata[2] != b'L'
        || metadata[3] != b'F'
    {
        return Err(SysErrNo::ENOEXEC);
    }

    let header_elf = ElfFile::new(&metadata).map_err(|_| SysErrNo::ENOEXEC)?;
    let ph_end = program_headers_end(&header_elf)?;
    extend_inode_prefix(inode, &mut metadata, ph_end, file_size)?;
    if metadata.len() < ph_end {
        return Err(SysErrNo::ENOEXEC);
    }

    let elf = ElfFile::new(&metadata).map_err(|_| SysErrNo::ENOEXEC)?;
    validate_elf_load_ranges(&elf, file_size)?;
    let needed = needed_elf_metadata_len(&elf)?;
    extend_inode_prefix(inode, &mut metadata, needed, file_size)?;
    if metadata.len() < needed {
        return Err(SysErrNo::ENOEXEC);
    }
    Ok(metadata)
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
    /// - 有 `PT_INTERP` 时，优先按 ELF 中写明的原始路径直接打开；只有原始
    ///   路径不存在时才按竞赛镜像兼容规则回退。这既保留 `/musl/lib/libc.so`
    ///   测试镜像兼容性，也不会把 Debian/Alpine 的原生解释器替换成旧副本。
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
            // Linux executes the interpreter named by PT_INTERP.  Library
            // search and compatibility aliases are userspace loader work;
            // replacing this pathname in the kernel can mix incompatible
            // loader/libc ABIs from different rootfs layouts.
            let interp_file = open_direct(&interp, OpenFlags::O_RDONLY, NONE_MODE)
                .ok()
                .and_then(|file| file.file().ok())
                .ok_or(())?;
            // 动态解释器在重定位期间会修改自身状态。保持它的 PT_LOAD 段为
            // eager framed 映射，避免解释器尚未就绪时进入文件页/COW 缺页路径。
            #[cfg(feature = "perf")]
            let interp_read_begin = get_ticks();
            let interp_elf_data = read_elf_metadata(&interp_file.inode).map_err(|_| ())?;
            #[cfg(feature = "perf")]
            crate::utils::perf::record_exec_interp_read_duration(
                get_ticks().saturating_sub(interp_read_begin),
            );
            let interp_elf = xmas_elf::ElfFile::new(&interp_elf_data).map_err(|_| ())?;
            #[cfg(feature = "perf")]
            let interp_map_begin = get_ticks();
            self.map_elf_eager_file(&interp_elf, DL_INTERP_OFFSET.into(), &interp_file)?;
            #[cfg(feature = "perf")]
            crate::utils::perf::record_exec_interp_map_duration(
                get_ticks().saturating_sub(interp_map_begin),
            );

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

    /// Register a main executable's PT_LOAD segments as demand-paged private
    /// file mappings.  The dynamic interpreter uses the eager path below:
    /// it modifies its relocation state before user-space fault handling is
    /// fully available. Main executable text and read-only data can instead
    /// use the file page cache on first access, while writable pages remain
    /// private through the normal COW path.
    fn map_elf_lazy_file(
        &mut self,
        elf: &ElfFile,
        offset: VirtAddr,
        file: &Arc<OSFile>,
    ) -> Result<(VirtPageNum, VirtAddr), ()> {
        let ph_count = elf.header.pt2.ph_count();
        let mut max_end_vpn = offset.floor();
        let mut header_va = 0;
        let mut has_found_header_va = false;

        for i in 0..ph_count {
            let ph = elf.program_header(i).map_err(|_| ())?;
            if ph.get_type().map_err(|_| ())? != xmas_elf::program::Type::Load {
                continue;
            }

            let start_va: VirtAddr = (ph.virtual_addr() as usize + offset.0).into();
            let end_va: VirtAddr = ((ph.virtual_addr() + ph.mem_size()) as usize + offset.0).into();
            let file_size = ph.file_size() as usize;
            let mem_size = ph.mem_size() as usize;
            if file_size > mem_size {
                return Err(());
            }
            if !has_found_header_va {
                header_va = start_va.0;
                has_found_header_va = true;
            }

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

            let page_offset = start_va.0 - start_va.floor().0 * PAGE_SIZE;
            let can_lazy_map = page_offset == 0 && (ph.offset() as usize) % PAGE_SIZE == 0;
            if !can_lazy_map {
                // Preserve the exact zero-before/after-segment semantics for
                // unusual unaligned ELF segments.
                let map_area = MapArea::new(
                    start_va,
                    end_va,
                    MapType::Framed,
                    map_perm,
                    MapAreaType::Elf,
                );
                max_end_vpn = max_end_vpn.max(map_area.vpn_range.end());
                self.push_elf_segment_from_file(
                    map_area,
                    page_offset,
                    file,
                    ph.offset() as usize,
                    file_size,
                )?;
                continue;
            }

            // A file-backed VMA always exposes complete pages.  Do not map
            // the partial final page directly from the executable: bytes
            // after p_filesz belong to the zero-initialized BSS, while the
            // file can contain section headers or other unrelated data there.
            let full_file_size = file_size / PAGE_SIZE * PAGE_SIZE;
            let full_file_end = start_va.0.checked_add(full_file_size).ok_or(())?;
            let full_file_end_vpn = VirtAddr::from(full_file_end).ceil();
            if full_file_size != 0 {
                let file_area = MapArea::new_mmap(
                    start_va,
                    full_file_end_vpn.into(),
                    MapType::Framed,
                    map_perm,
                    MapAreaType::Mmap,
                    Some(file.clone()),
                    ph.offset() as usize,
                    MmapFlags::MAP_PRIVATE,
                );
                max_end_vpn = max_end_vpn.max(file_area.vpn_range.end());
                self.push_lazily(file_area);
            }

            let mem_end_vpn = end_va.ceil();
            let final_page_file_size = file_size - full_file_size;
            if final_page_file_size != 0 {
                let final_page_end_vpn = VirtPageNum(full_file_end_vpn.0.checked_add(1).ok_or(())?);
                let final_page_area = MapArea::new(
                    full_file_end.into(),
                    final_page_end_vpn.into(),
                    MapType::Framed,
                    map_perm,
                    MapAreaType::Elf,
                );
                max_end_vpn = max_end_vpn.max(final_page_area.vpn_range.end());
                self.push_elf_segment_from_file(
                    final_page_area,
                    0,
                    file,
                    (ph.offset() as usize)
                        .checked_add(full_file_size)
                        .ok_or(())?,
                    final_page_file_size,
                )?;
                if final_page_end_vpn < mem_end_vpn {
                    let bss_area = MapArea::new(
                        final_page_end_vpn.into(),
                        mem_end_vpn.into(),
                        MapType::Framed,
                        map_perm,
                        MapAreaType::Elf,
                    );
                    max_end_vpn = max_end_vpn.max(bss_area.vpn_range.end());
                    self.push_lazily(bss_area);
                }
                continue;
            }

            if full_file_end_vpn < mem_end_vpn {
                let bss_area = MapArea::new(
                    full_file_end_vpn.into(),
                    mem_end_vpn.into(),
                    MapType::Framed,
                    map_perm,
                    MapAreaType::Elf,
                );
                max_end_vpn = max_end_vpn.max(bss_area.vpn_range.end());
                self.push_lazily(bss_area);
            }
        }
        Ok((max_end_vpn, header_va.into()))
    }

    /// Eagerly map a dynamic interpreter's PT_LOAD segments from its backing
    /// file. The interpreter writes relocation state while starting, before
    /// its own file-backed fault path is reliable.
    fn map_elf_eager_file(
        &mut self,
        elf: &ElfFile,
        offset: VirtAddr,
        file: &Arc<OSFile>,
    ) -> Result<(VirtPageNum, VirtAddr), ()> {
        let ph_count = elf.header.pt2.ph_count();
        let mut max_end_vpn = offset.floor();
        let mut header_va = 0;
        let mut has_found_header_va = false;

        for i in 0..ph_count {
            let ph = elf.program_header(i).map_err(|_| ())?;
            if ph.get_type().map_err(|_| ())? != xmas_elf::program::Type::Load {
                continue;
            }

            let start_va: VirtAddr = (ph.virtual_addr() as usize + offset.0).into();
            let end_va: VirtAddr = ((ph.virtual_addr() + ph.mem_size()) as usize + offset.0).into();
            let file_size = ph.file_size() as usize;
            let mem_size = ph.mem_size() as usize;
            if file_size > mem_size {
                return Err(());
            }
            if !has_found_header_va {
                header_va = start_va.0;
                has_found_header_va = true;
            }

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

            let page_offset = start_va.0 - start_va.floor().0 * PAGE_SIZE;
            let map_area = MapArea::new(
                start_va,
                end_va,
                MapType::Framed,
                map_perm,
                MapAreaType::Elf,
            );
            max_end_vpn = max_end_vpn.max(map_area.vpn_range.end());
            self.push_elf_segment_from_file(
                map_area,
                page_offset,
                file,
                ph.offset() as usize,
                file_size,
            )?;
        }
        Ok((max_end_vpn, header_va.into()))
    }

    /// Eagerly map an ELF segment and fill only its file bytes. This preserves
    /// the zero-before/after-segment behavior that a single mmap VMA cannot
    /// express for unaligned main-program segments, and is also used for all
    /// dynamic-interpreter segments.
    fn push_elf_segment_from_file(
        &mut self,
        mut map_area: MapArea,
        data_offset: usize,
        file: &Arc<OSFile>,
        file_offset: usize,
        file_size: usize,
    ) -> Result<(), ()> {
        let mapped_size = map_area
            .vpn_range
            .end()
            .0
            .checked_sub(map_area.vpn_range.start().0)
            .and_then(|pages| pages.checked_mul(PAGE_SIZE))
            .ok_or(())?;
        if file_size != 0
            && data_offset
                .checked_add(file_size)
                .filter(|end| *end <= mapped_size)
                .is_none()
        {
            return Err(());
        }

        let chunk_capacity = file_size.min(ELF_SEGMENT_READ_CHUNK);
        let mut read_buf = Vec::new();
        read_buf.try_reserve_exact(chunk_capacity).map_err(|_| ())?;
        read_buf.resize(chunk_capacity, 0);

        map_area.map(&mut self.page_table)?;

        let mut copied = 0;
        let cache_path = file.inode.page_cache_path();
        let file_size_for_cache = file.inode.size();
        while copied < file_size {
            let chunk_len = (file_size - copied).min(read_buf.len());
            let file_read_offset = file_offset.checked_add(copied).ok_or(())?;
            // The interpreter and unaligned main-program segments are read
            // again on every exec. Serve the chunk from the shared file page
            // cache when the whole range is resident; otherwise read through
            // lwext4 once and publish the fully covered pages for the next
            // exec (bytes are already compatibility-patched by read_at).
            let read = if let Some(path) = cache_path.as_deref() {
                if let Some(cached) = FILE_PAGE_CACHE.read_cached_at(
                    path,
                    file_read_offset,
                    &mut read_buf[..chunk_len],
                ) {
                    cached
                } else {
                    let r = file
                        .inode
                        .read_at(file_read_offset, &mut read_buf[..chunk_len])
                        .map_err(|_| ())?;
                    if r != 0 {
                        FILE_PAGE_CACHE.insert_read_range(
                            path,
                            file_read_offset,
                            &read_buf[..r],
                            file_size_for_cache,
                        );
                    }
                    r
                }
            } else {
                file.inode
                    .read_at(file_read_offset, &mut read_buf[..chunk_len])
                    .map_err(|_| ())?
            };
            #[cfg(feature = "perf")]
            crate::utils::perf::record_inode_read_source(
                crate::utils::perf::InodeReadSource::Other,
                read,
            );
            if read == 0 || read > chunk_len {
                return Err(());
            }

            let mut chunk_copied = 0;
            while chunk_copied < read {
                let area_offset = data_offset
                    .checked_add(copied)
                    .and_then(|offset| offset.checked_add(chunk_copied))
                    .ok_or(())?;
                let vpn = VirtPageNum(
                    map_area
                        .vpn_range
                        .start()
                        .0
                        .checked_add(area_offset / PAGE_SIZE)
                        .ok_or(())?,
                );
                let page_offset = area_offset % PAGE_SIZE;
                let copy_len = (read - chunk_copied).min(PAGE_SIZE - page_offset);
                let ppn = self.page_table.translate(vpn).ok_or(())?;
                ppn.bytes_array_mut()[page_offset..page_offset + copy_len]
                    .copy_from_slice(&read_buf[chunk_copied..chunk_copied + copy_len]);
                chunk_copied = chunk_copied.checked_add(copy_len).ok_or(())?;
            }
            copied = copied.checked_add(read).ok_or(())?;
        }

        self.push_lazily(map_area);
        Ok(())
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
        Self::from_elf_inner(elf_data, None)
    }

    /// Create an address space from ELF metadata while retaining the opened
    /// executable for demand-paged, private file-backed PT_LOAD mappings.
    ///
    /// Only page-aligned PT_LOAD segments take this path.  The loader keeps the
    /// existing eager implementation for callers that do not provide a file
    /// object (the boot-time init process) and for unaligned segments.
    pub fn from_elf_file(
        elf_data: &[u8],
        file: &Arc<OSFile>,
    ) -> Result<(Self, usize, usize, Vec<Aux>), ()> {
        Self::from_elf_inner(elf_data, Some(file))
    }

    fn from_elf_inner(
        elf_data: &[u8],
        executable_file: Option<&Arc<OSFile>>,
    ) -> Result<(Self, usize, usize, Vec<Aux>), ()> {
        let mut auxv = Vec::new();
        // 新用户地址空间仍必须带内核映射；用户态 trap 进入内核后需要这些映射
        // 才能继续执行内核代码。
        #[cfg(feature = "perf")]
        let kernel_space_begin = get_ticks();
        let mut memory_set = Self::new_from_kernel();
        #[cfg(feature = "perf")]
        crate::utils::perf::record_exec_kernel_space_duration(
            get_ticks().saturating_sub(kernel_space_begin),
        );
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
        #[cfg(feature = "perf")]
        let interp_begin = get_ticks();
        if let Some(interp_entry_point) = memory_set.load_dl_interp_if_needed(&elf)? {
            // `AT_BASE` 记录动态解释器的加载基址；动态链接器用它定位自身。
            auxv.push(Aux::new(AuxType::BASE, DL_INTERP_OFFSET));
            // 动态链接程序先进入解释器，由解释器完成重定位后跳回主程序。
            entry_point = interp_entry_point;
        } else {
            // 静态 ELF 没有动态解释器，Linux 语义下 `AT_BASE` 为 0。
            auxv.push(Aux::new(AuxType::BASE, 0));
        }
        #[cfg(feature = "perf")]
        crate::utils::perf::record_exec_interp_duration(get_ticks().saturating_sub(interp_begin));
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
        // 平台字符串、硬件能力、secure exec 等字段提供给 libc 和动态加载器。
        auxv.push(Aux::new(AuxType::PLATFORM, 0 as usize));
        auxv.push(Aux::new(AuxType::HWCAP, elf_hwcap()));
        auxv.push(Aux::new(AuxType::CLKTCK, 100 as usize));
        auxv.push(Aux::new(AuxType::SECURE, 0 as usize));
        auxv.push(Aux::new(AuxType::NOTELF, 0x112d as usize));

        // 主程序按 ELF 自己声明的虚拟地址映射，所以 offset 为 0。
        #[cfg(feature = "perf")]
        let map_elf_begin = get_ticks();
        let (max_end_vpn, head_va) = if let Some(file) = executable_file {
            memory_set.map_elf_lazy_file(&elf, VirtAddr(0), file)?
        } else {
            memory_set.map_elf(&elf, VirtAddr(0))?
        };
        #[cfg(feature = "perf")]
        crate::utils::perf::record_exec_map_elf_duration(get_ticks().saturating_sub(map_elf_begin));

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
