//! 本文件主要实现了关于虚拟地址到物理地址转换的相关功能
//! 再原来的基础上，本人使用Cursor内部的ChatGPT5.5对改文件进行了修改
//! 为了兼容性和封装性，未来的地址转换不应该直接暴露出去，只应该给外部暴露 copy_from 和 copy_to 两个接口
//! Implementation of [`PageTableEntry`] and [`PageTable`].
use crate::{
    arch::{
        memory_layout::{PAGE_SIZE, PAGE_SIZE_BITS},
        time::get_ticks,
    },
    fs::MAX_PATH_LEN,
    mm::{PhysPageNum, VirtPageNum},
    utils::{SysErrNo, SyscallRet},
};

use crate::mm::MapPermission;

use super::{MemorySet, StepByOne, VirtAddr};
use alloc::{string::String, vec, vec::Vec};

use crate::trap::trap_types::*;

use crate::arch::page_table::PageTable;

fn checked_user_range(start: usize, len: usize) -> Result<usize, SysErrNo> {
    if len == 0 {
        return Ok(start);
    }
    if start == 0 {
        return Err(SysErrNo::EFAULT);
    }
    // 验证起始地址是合法的 SV39 规范地址，避免后续 VirtAddr::from  panic
    if VirtAddr::try_from(start).is_none() {
        return Err(SysErrNo::EFAULT);
    }
    start.checked_add(len).ok_or(SysErrNo::EFAULT)
}

fn translated_user_page(
    memory_set: &MemorySet,
    page_table: &PageTable,
    vpn: VirtPageNum,
    fault: Trap,
) -> Option<PhysPageNum> {
    match page_table.translate(vpn) {
        Some(ppn) => Some(ppn),
        None => {
            memory_set.handle_page_fault(vpn, fault);
            page_table.translate(vpn)
        }
    }
}

fn user_range_has_perm(
    memory_set: &MemorySet,
    start: usize,
    len: usize,
    wanted: MapPermission,
) -> bool {
    if len == 0 {
        return true;
    }
    let end = match start.checked_add(len) {
        Some(end) => end,
        None => return false,
    };
    let mut current = match VirtAddr::try_from(start) {
        Some(va) => va.floor().0,
        None => return false,
    };
    let end_vpn = match VirtAddr::try_from(end - 1) {
        Some(va) => va.floor().0,
        None => return false,
    };

    let memory_set = memory_set.get_ref();
    while current <= end_vpn {
        let Some(area) = memory_set.areas.iter().find(|area| {
            let (start, end) = area.vpn_range.range();
            start.0 <= current && current < end.0
        }) else {
            return false;
        };
        if !area.map_perm.contains(wanted) {
            return false;
        }
        let (_, area_end) = area.vpn_range.range();
        current = area_end.0;
    }
    true
}

#[cfg(target_arch = "loongarch64")]
fn translated_user_page_for_write(
    memory_set: &MemorySet,
    page_table: &PageTable,
    vpn: VirtPageNum,
) -> Option<PhysPageNum> {
    let fault = Trap::Exception(Exception::StorePageFault);
    if page_table.translate(vpn).is_none() && !memory_set.handle_page_fault(vpn, fault) {
        return None;
    }
    memory_set.handle_page_fault(vpn, fault);
    page_table.translate(vpn)
}

#[cfg(not(target_arch = "loongarch64"))]
fn translated_user_page_for_write(
    memory_set: &MemorySet,
    page_table: &PageTable,
    vpn: VirtPageNum,
) -> Option<PhysPageNum> {
    let fault = Trap::Exception(Exception::StorePageFault);
    if page_table.translate(vpn).is_none() && !memory_set.handle_page_fault(vpn, fault) {
        return None;
    }

    // copy_to_user() accesses the physical page directly, so it would bypass
    // the CPU's StorePageFault on a present COW PTE without this explicit
    // write-fault step.
    if page_table.is_cow_page(vpn) && !memory_set.handle_page_fault(vpn, fault) {
        return None;
    }

    page_table.translate(vpn)
}

/// 安全地从用户空间复制任意类型 T 的值到内核空间。
///
/// 底层调用 `copy_from_user`，自动处理跨页、延迟页分配等。
/// 成功返回 `Ok(T)`，失败返回 `Err(EFAULT)`。
pub fn copy_from_user_val<T: Sized>(memory_set: &MemorySet, src: *const T) -> Result<T, SysErrNo> {
    let mut val: core::mem::MaybeUninit<T> = core::mem::MaybeUninit::uninit();
    let dst_slice = unsafe {
        core::slice::from_raw_parts_mut(val.as_mut_ptr() as *mut u8, core::mem::size_of::<T>())
    };
    copy_from_user(memory_set, src as usize, dst_slice)?;
    Ok(unsafe { val.assume_init() })
}

/// 安全地从内核空间复制任意类型 T 的值到用户空间。
///
/// 底层调用 `copy_to_user`，自动处理跨页、延迟页分配等。
/// 成功返回 `Ok(())`，失败返回 `Err(EFAULT)`。
pub fn copy_to_user_val<T: Sized>(
    memory_set: &MemorySet,
    dst: *mut T,
    val: &T,
) -> Result<(), SysErrNo> {
    let src_slice = unsafe {
        core::slice::from_raw_parts(val as *const T as *const u8, core::mem::size_of::<T>())
    };
    copy_to_user(memory_set, dst as usize, src_slice)?;
    Ok(())
}

/// 可失败地从用户空间复制任意类型 T 的值到内核空间。
///
/// 与 `copy_from_user_val` 相同，但返回 `Option<T>` 以兼容原有 `try_get_data` 的调用风格。
/// 用于 futex 等需要在用户内存可能被部分 unmap 时优雅降级的场景。
pub fn try_copy_from_user_val<T: Sized>(memory_set: &MemorySet, src: *const T) -> Option<T> {
    copy_from_user_val(memory_set, src).ok()
}

/// 将数据从用户空间安全地复制到内核空间
///
/// - `token`: 源用户空间的页表 token
/// - `src`: 用户空间的源虚拟地址
/// - `dst`: 内核空间的目标缓冲区
///
/// 成功返回 `Ok(复制的字节数)`，失败返回 `Err(EFAULT)`
pub fn copy_from_user(memory_set: &MemorySet, src: usize, dst: &mut [u8]) -> SyscallRet {
    let len = dst.len();
    if len == 0 {
        return Ok(0);
    }
    let end = checked_user_range(src, len)?;
    if !user_range_has_perm(memory_set, src, len, MapPermission::R) {
        return Err(SysErrNo::EFAULT);
    }

    let page_table = PageTable::from_token(memory_set.token());
    let mut cur_src = src;
    let mut cur_dst = 0;

    while cur_src < end {
        let start_va = VirtAddr::try_from(cur_src).ok_or(SysErrNo::EFAULT)?;
        let vpn = start_va.floor();
        let ppn = translated_user_page(
            memory_set,
            &page_table,
            vpn,
            Trap::Exception(Exception::LoadPageFault),
        )
        .ok_or(SysErrNo::EFAULT)?;
        // 本页内可复制的字节数：从当前偏移到页末，或到 end
        let next_page_va = ((vpn.0 + 1) << PAGE_SIZE_BITS) as usize;
        let copy_len = (end - cur_src).min(next_page_va - cur_src);

        let src_slice =
            &ppn.bytes_array()[start_va.page_offset()..start_va.page_offset() + copy_len];
        dst[cur_dst..cur_dst + copy_len].copy_from_slice(src_slice);

        cur_src += copy_len;
        cur_dst += copy_len;
    }

    Ok(len)
}

/// 将数据从内核空间安全地复制到用户空间（对应 Linux 的 copy_to_user）
///
/// - `token`: 目标用户空间的页表 token
/// - `dst`: 用户空间的目标虚拟地址
/// - `src`: 内核空间的源数据切片
///
/// 成功返回 `Ok(复制的字节数)`，失败返回 `Err(EFAULT)`
pub fn copy_to_user(memory_set: &MemorySet, dst: usize, src: &[u8]) -> SyscallRet {
    let len = src.len();
    if len == 0 {
        return Ok(0);
    }
    let end = checked_user_range(dst, len)?;
    if !user_range_has_perm(memory_set, dst, len, MapPermission::W) {
        return Err(SysErrNo::EFAULT);
    }
    let page_table = PageTable::from_token(memory_set.token());
    let mut cur_dst = dst;
    let mut cur_src = 0;

    while cur_dst < end {
        let start_va = VirtAddr::try_from(cur_dst).ok_or(SysErrNo::EFAULT)?;
        let vpn = start_va.floor();
        let ppn =
            translated_user_page_for_write(memory_set, &page_table, vpn).ok_or(SysErrNo::EFAULT)?;

        // 本页内可复制的字节数：从当前偏移到页末，或到 end
        let next_page_va = ((vpn.0 + 1) << PAGE_SIZE_BITS) as usize;
        let copy_len = (end - cur_dst).min(next_page_va - cur_dst);

        let dst_slice =
            &mut ppn.bytes_array_mut()[start_va.page_offset()..start_va.page_offset() + copy_len];
        dst_slice.copy_from_slice(&src[cur_src..cur_src + copy_len]);

        cur_dst += copy_len;
        cur_src += copy_len;
    }

    Ok(len)
}

/// 检查用户写缓冲区是否可访问，但不修改缓冲区内容。
pub fn probe_user_write(memory_set: &MemorySet, dst: usize, len: usize) -> SyscallRet {
    if len == 0 {
        return Ok(0);
    }
    let end = checked_user_range(dst, len)?;
    if !user_range_has_perm(memory_set, dst, len, MapPermission::W) {
        return Err(SysErrNo::EFAULT);
    }

    let page_table = PageTable::from_token(memory_set.token());
    let mut cur_dst = dst;

    while cur_dst < end {
        let start_va = VirtAddr::try_from(cur_dst).ok_or(SysErrNo::EFAULT)?;
        let vpn = start_va.floor();
        translated_user_page_for_write(memory_set, &page_table, vpn).ok_or(SysErrNo::EFAULT)?;

        let next_page_va = ((vpn.0 + 1) << PAGE_SIZE_BITS) as usize;
        cur_dst += (end - cur_dst).min(next_page_va - cur_dst);
    }

    Ok(len)
}

/// 安全地将用户虚拟地址转换为物理地址。
///
/// 遵循内核设计原则：先通过 `copy_from_user` 触发延迟页分配，
/// 确保页面已映射后再进行 VA→PA 转换。避免在未分配页面上直接
/// 调用 `translate_va` 导致 panic 或遗漏延迟分配。
pub fn translate_user_va_safe(memory_set: &MemorySet, va: VirtAddr) -> Result<usize, SysErrNo> {
    let page_table = PageTable::from_token(memory_set.token());
    let vpn = va.floor();
    // 页面未映射时，通过 copy_from_user 触发延迟页分配
    if page_table.translate(vpn).is_none() {
        let mut dummy = [0u8; 4];
        copy_from_user(memory_set, va.into(), &mut dummy)?;
    }
    page_table
        .translate_va(va)
        .map(|pa| pa.0)
        .ok_or(SysErrNo::EFAULT)
}

pub fn read_user_cstr(memory_set: &MemorySet, ptr: *const u8) -> Result<String, SysErrNo> {
    if ptr.is_null() {
        return Ok(String::new());
    }
    let mut dst_str = [0u8; MAX_PATH_LEN];
    let mut pos = 0;
    let ptr_addr = ptr as usize;

    // 逐页读取用户空间字符串，在遇到 '\0' 时提前停止，
    // 避免因尝试读取 MAX_PATH_LEN 字节而跨越未映射的页边界
    while pos < MAX_PATH_LEN {
        let current_addr = ptr_addr + pos;
        // 计算当前页内还能读取多少字节
        let va = match VirtAddr::try_from(current_addr) {
            Some(va) => va,
            None => {
                // 非法 VA：检查已读部分，若已有完整字符串则返回
                if let Some(null_pos) = dst_str[..pos].iter().position(|&b| b == 0) {
                    return Ok(String::from(
                        core::str::from_utf8(&dst_str[..null_pos]).unwrap_or(""),
                    ));
                }
                return Err(SysErrNo::EFAULT);
            }
        };
        let vpn = va.floor();
        let page_end = ((vpn.0 + 1) << PAGE_SIZE_BITS) as usize;
        let chunk_size = core::cmp::min(page_end - current_addr, MAX_PATH_LEN - pos);

        // 尝试从用户空间复制这一块
        if copy_from_user(
            memory_set,
            current_addr,
            &mut dst_str[pos..pos + chunk_size],
        )
        .is_err()
        {
            // 该块无法读取：检查已经读取的部分是否已包含完整字符串
            if let Some(null_pos) = dst_str[..pos].iter().position(|&b| b == 0) {
                return Ok(String::from(
                    core::str::from_utf8(&dst_str[..null_pos]).unwrap_or(""),
                ));
            }
            return Err(SysErrNo::EFAULT);
        }

        // 在新读取的块中查找 '\0'
        if let Some(offset) = dst_str[pos..pos + chunk_size].iter().position(|&b| b == 0) {
            let null_pos = pos + offset;
            return Ok(String::from(
                core::str::from_utf8(&dst_str[..null_pos]).unwrap_or(""),
            ));
        }

        pos += chunk_size;
    }

    // 读取了 MAX_PATH_LEN 均未遇到 '\0'
    Ok(String::from(core::str::from_utf8(&dst_str).unwrap_or("")))
}

/// Safely read a NUL-terminated user string with an explicit byte limit.
///
/// `max_len` includes the trailing NUL byte.  Unlike [`read_user_cstr`], this
/// helper never silently truncates an unterminated string: reaching the limit
/// returns `E2BIG`.  It preserves the raw non-NUL bytes because `execve`
/// argv/envp entries are byte strings, not necessarily UTF-8 text.
pub fn read_user_cstr_with_limit(
    memory_set: &MemorySet,
    ptr: *const u8,
    max_len: usize,
) -> Result<Vec<u8>, SysErrNo> {
    if ptr.is_null() {
        return Ok(Vec::new());
    }
    if max_len == 0 {
        return Err(SysErrNo::E2BIG);
    }

    // Keep this buffer small enough for the LoongArch kernel stack.  The
    // destination grows in the heap only up to the caller-specified bound.
    const CSTR_COPY_CHUNK_SIZE: usize = 256;
    let mut chunk = [0u8; CSTR_COPY_CHUNK_SIZE];
    let mut bytes = Vec::new();
    let ptr_addr = ptr as usize;
    let mut pos = 0;

    while pos < max_len {
        let current_addr = ptr_addr.checked_add(pos).ok_or(SysErrNo::EFAULT)?;
        let va = VirtAddr::try_from(current_addr).ok_or(SysErrNo::EFAULT)?;
        let chunk_len = (PAGE_SIZE - va.page_offset())
            .min(max_len - pos)
            .min(CSTR_COPY_CHUNK_SIZE);

        copy_from_user(memory_set, current_addr, &mut chunk[..chunk_len])?;
        if let Some(nul_offset) = chunk[..chunk_len].iter().position(|&byte| byte == 0) {
            bytes
                .try_reserve(nul_offset)
                .map_err(|_| SysErrNo::ENOMEM)?;
            bytes.extend_from_slice(&chunk[..nul_offset]);
            return Ok(bytes);
        }

        bytes.try_reserve(chunk_len).map_err(|_| SysErrNo::ENOMEM)?;
        bytes.extend_from_slice(&chunk[..chunk_len]);
        pos += chunk_len;
    }

    Err(SysErrNo::E2BIG)
}

// Internal helpers for mm-crate use (pages guaranteed mapped)

/// Internal: read bytes from user memory via page table into an existing buffer.
/// Pages must already be mapped (used in writeback scenarios).
pub(crate) fn read_user_bytes_direct_into(token: usize, src: usize, dst: &mut [u8]) -> Option<()> {
    let len = dst.len();
    if len == 0 {
        return Some(());
    }
    let page_table = PageTable::from_token(token);
    let mut cur_src = src;
    let end = src.checked_add(len)?;
    let mut cur_dst = 0;
    while cur_src < end {
        let start_va = VirtAddr::try_from(cur_src)?;
        let vpn = start_va.floor();
        let ppn = page_table.translate(vpn)?;
        let next_page_va = ((vpn.0 + 1) << PAGE_SIZE_BITS) as usize;
        let copy_len = (end - cur_src).min(next_page_va - cur_src);
        let src_slice =
            &ppn.bytes_array()[start_va.page_offset()..start_va.page_offset() + copy_len];
        dst[cur_dst..cur_dst + copy_len].copy_from_slice(src_slice);
        cur_src += copy_len;
        cur_dst += copy_len;
    }
    Some(())
}

/// Internal: read bytes from user memory via page table.
/// Pages must already be mapped (used in writeback scenarios).
pub(crate) fn read_user_bytes_direct(token: usize, src: usize, len: usize) -> Option<Vec<u8>> {
    if len == 0 {
        return Some(Vec::new());
    }
    let mut buf = vec![0u8; len];
    read_user_bytes_direct_into(token, src, &mut buf)?;
    Some(buf)
}

/// Internal: write bytes to user memory via page table.
/// Pages must already be mapped (used in page-fault scenarios).
pub(crate) fn write_user_bytes_direct(token: usize, dst: usize, src: &[u8]) -> Option<()> {
    let len = src.len();
    if len == 0 {
        return Some(());
    }
    let page_table = PageTable::from_token(token);
    let mut cur_dst = dst;
    let end = dst + len;
    let mut cur_src = 0;
    while cur_dst < end {
        let start_va = VirtAddr::try_from(cur_dst)?;
        let vpn = start_va.floor();
        let ppn = page_table.translate(vpn)?;
        let next_page_va = ((vpn.0 + 1) << PAGE_SIZE_BITS) as usize;
        let copy_len = (end - cur_dst).min(next_page_va - cur_dst);
        let dst_slice =
            &mut ppn.bytes_array_mut()[start_va.page_offset()..start_va.page_offset() + copy_len];
        dst_slice.copy_from_slice(&src[cur_src..cur_src + copy_len]);
        cur_dst += copy_len;
        cur_src += copy_len;
    }
    Some(())
}

/// 从内核分配的缓冲区创建 UserBuffer。
///
/// # Safety
/// 调用者必须确保 `buf` 的生命周期长于返回的 `UserBuffer` 的使用期。
pub unsafe fn user_buffer_from_kernel(buf: &mut [u8]) -> UserBuffer {
    let len = buf.len();
    let ptr = buf.as_mut_ptr();
    UserBuffer::new(vec![core::slice::from_raw_parts_mut(ptr, len)])
}

///Array of u8 slice that user communicate with os
pub struct UserBuffer {
    ///U8 vec
    pub buffers: Vec<&'static mut [u8]>,
}

impl UserBuffer {
    ///Create a `UserBuffer` by parameter
    pub fn new(buffers: Vec<&'static mut [u8]>) -> Self {
        Self { buffers }
    }
    ///Length of `UserBuffer`
    pub fn len(&self) -> usize {
        let mut total: usize = 0;
        for b in self.buffers.iter() {
            total += b.len();
        }
        total
    }
    /// 将内容数组返回
    pub fn read(&mut self, len: usize) -> Vec<u8> {
        let len = self.len().min(len);
        let mut bytes = vec![0; len];
        if len == 0 {
            return bytes;
        }
        let mut current = 0;
        for sub_buff in self.buffers.iter_mut() {
            let mut sblen = (*sub_buff).len();
            if current + sblen > len {
                sblen = len - current;
            }
            bytes[current..current + sblen].copy_from_slice(&(*sub_buff)[..sblen]);
            current += sblen;
            if current == len {
                return bytes;
            }
        }
        bytes.truncate(current);
        bytes
    }
    /// 直接读取内容到传入的缓冲区中，返回实际读取的长度
    pub fn read_to(&self, dst: &mut [u8]) -> usize {
        let len = dst.len();
        let mut current = 0;

        for sub_buff in self.buffers.iter() {
            let mut sblen = sub_buff.len();
            if current + sblen > len {
                sblen = len - current;
            }

            // 直接拷贝到传入的 dst 中，不创建新 Vec
            dst[current..current + sblen].copy_from_slice(&sub_buff[..sblen]);
            current += sblen;

            if current == len {
                return current;
            }
        }
        current
    }
    /// 将一个Buffer的数据写入UserBuffer，返回写入长度
    pub fn write(&mut self, buff: &[u8]) -> usize {
        let len = self.len().min(buff.len());
        if len == 0 {
            return len;
        }
        let mut current = 0;
        for sub_buff in self.buffers.iter_mut() {
            let mut sblen = (*sub_buff).len();
            if buff.len() > 10 {
                if current + sblen > len {
                    sblen = len - current;
                }
                (*sub_buff)[..sblen].copy_from_slice(&buff[current..current + sblen]);
                current += sblen;
                if current == len {
                    return len;
                }
            } else {
                for j in 0..sblen {
                    (*sub_buff)[j] = buff[current];
                    current += 1;
                    if current == len {
                        return len;
                    }
                }
            }
        }
        return len;
    }

    /// 将多个连续源切片顺序写入用户缓冲区，避免调用方先聚合成临时 `Vec`。
    ///
    /// 返回最多 `len` 字节中的实际写入量。源切片和用户页片段都可能不连续，
    /// 因此这里同时推进两侧的偏移量。
    pub fn write_from_slices<'a, I>(&mut self, slices: I, len: usize) -> usize
    where
        I: IntoIterator<Item = &'a [u8]>,
    {
        let limit = self.len().min(len);
        if limit == 0 {
            return 0;
        }

        let mut written = 0;
        let mut target_index = 0;
        let mut target_offset = 0;

        for source in slices {
            let mut source_offset = 0;
            while source_offset < source.len() && written < limit {
                while target_index < self.buffers.len()
                    && target_offset == self.buffers[target_index].len()
                {
                    target_index += 1;
                    target_offset = 0;
                }
                if target_index == self.buffers.len() {
                    return written;
                }

                let copy_len = (limit - written)
                    .min(source.len() - source_offset)
                    .min(self.buffers[target_index].len() - target_offset);
                self.buffers[target_index][target_offset..target_offset + copy_len]
                    .copy_from_slice(&source[source_offset..source_offset + copy_len]);
                source_offset += copy_len;
                target_offset += copy_len;
                written += copy_len;
            }
            if written == limit {
                return written;
            }
        }
        written
    }

    //在指定位置写入数据
    pub fn write_at(&mut self, offset: usize, buff: &[u8]) -> isize {
        //未被使用，暂不做优化
        let len = buff.len();
        if offset + len > self.len() {
            return -1;
        }
        let mut head = 0; // offset of slice in UBuffer
        let mut current = 0; // current offset of buff

        for sub_buff in self.buffers.iter_mut() {
            let sblen = (*sub_buff).len();
            if head + sblen < offset {
                continue;
            } else if head < offset {
                for j in (offset - head)..sblen {
                    (*sub_buff)[j] = buff[current];
                    current += 1;
                    if current == len {
                        return len as isize;
                    }
                }
            } else {
                //head + sblen > offset and head > offset
                for j in 0..sblen {
                    (*sub_buff)[j] = buff[current];
                    current += 1;
                    if current == len {
                        return len as isize;
                    }
                }
            }
            head += sblen;
        }
        0
    }

    pub fn fill0(&mut self) -> usize {
        for sub_buff in self.buffers.iter_mut() {
            let sblen = (*sub_buff).len();
            for j in 0..sblen {
                (*sub_buff)[j] = 0;
            }
        }
        self.len()
    }

    pub fn fillrandom(&mut self) -> usize {
        //随机数生成方法： 线性计算+噪声+零特殊处理
        let mut random: u8 = (get_ticks() % 256) as u8;
        for sub_buff in self.buffers.iter_mut() {
            let sblen = (*sub_buff).len();
            for j in 0..sblen {
                if random == 0 {
                    random = (get_ticks() % 256) as u8;
                }
                random = (((random as usize) * (get_ticks() / 3 % 256) + 37) % 256) as u8; //生成一个字节大小的随机数
                (*sub_buff)[j] = random;
            }
        }
        self.len()
    }

    pub fn printbuf(&mut self, size: usize) {
        if size == 0 {
            return;
        }
        let mut count: usize = 0;
        for sub_buff in self.buffers.iter_mut() {
            let sblen = (*sub_buff).len();
            for j in 0..sblen {
                print!("{} ", (*sub_buff)[j]);
                count += 1;
                if count == size {
                    println!("");
                    return;
                }
            }
        }
    }

    pub fn clear(&mut self) -> usize {
        self.buffers.clear();
        self.len()
    }
}

impl IntoIterator for UserBuffer {
    type Item = *mut u8;
    type IntoIter = UserBufferIterator;
    fn into_iter(self) -> Self::IntoIter {
        UserBufferIterator {
            buffers: self.buffers,
            current_buffer: 0,
            current_idx: 0,
        }
    }
}
/// Iterator of `UserBuffer`
pub struct UserBufferIterator {
    buffers: Vec<&'static mut [u8]>,
    current_buffer: usize,
    current_idx: usize,
}

impl Iterator for UserBufferIterator {
    type Item = *mut u8;
    fn next(&mut self) -> Option<Self::Item> {
        while self.current_buffer < self.buffers.len() {
            // Skip empty buffers
            if self.buffers[self.current_buffer].is_empty() {
                self.current_buffer += 1;
                self.current_idx = 0;
                continue;
            }
            let r = &mut self.buffers[self.current_buffer][self.current_idx] as *mut _;
            if self.current_idx + 1 == self.buffers[self.current_buffer].len() {
                self.current_idx = 0;
                self.current_buffer += 1;
            } else {
                self.current_idx += 1;
            }
            return Some(r);
        }
        None
    }
}
