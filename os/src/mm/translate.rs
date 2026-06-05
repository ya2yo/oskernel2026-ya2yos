//! 本文件主要实现了关于虚拟地址到物理地址转换的相关功能
//! 再原来的基础上，本人使用Cursor内部的ChatGPT5.5对改文件进行了修改
//! 为了兼容性和封装性，未来的地址转换不应该直接暴露出去，只应该给外部暴露 copy_from 和 copy_to 两个接口
//! Implementation of [`PageTableEntry`] and [`PageTable`].
use crate::{
    arch::{
        memory_layout::{PAGE_SIZE, PAGE_SIZE_BITS},
        time::get_ticks,
    }, fs::MAX_PATH_LEN, mm::{KernelAddr, MapPermission, PhysPageNum, VirtPageNum, address, memory_set}, utils::{SysErrNo, SyscallRet}
};

use super::{MemorySet, StepByOne, VirtAddr};
use alloc::{string::String, sync::Arc, vec, vec::Vec};

use crate::trap::trap_types::*;
use log::debug;

use crate::arch::page_table::PageTable;

fn checked_user_range(start: usize, len: usize) -> Result<usize, SysErrNo> {
    if len == 0 {
        return Ok(start);
    }
    if start == 0 {
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
            memory_set.lazy_page_fault(vpn, fault);
            page_table.translate(vpn)
        }
    }
}

/// Translate a pointer to a mutable u8 Vec through page table
pub fn translated_byte_buffer(
    token: usize,
    ptr: *const u8,
    len: usize,
) -> Option<Vec<&'static mut [u8]>> {
    let page_table = PageTable::from_token(token);
    let mut start = ptr as usize;
    let end = start + len;
    let mut v = Vec::new();
    while start < end {
        let start_va = VirtAddr::from(start);
        let mut vpn = start_va.floor();
        let ppn = match page_table.translate(vpn) {
            None => {
                // debug!("vpn {:#x} not found", vpn.0);
                return None;
            }
            Some(ppn) => ppn,
        };
        vpn.step();
        let mut end_va: VirtAddr = vpn.into();
        end_va = end_va.min(VirtAddr::from(end));
        if end_va.page_offset() == 0 {
            v.push(&mut ppn.bytes_array_mut()[start_va.page_offset()..]);
        } else {
            v.push(&mut ppn.bytes_array_mut()[start_va.page_offset()..end_va.page_offset()]);
        }
        start = end_va.into();
    }
    Some(v)
}
/// Safely Translate a pointer to a mutable u8 Vec through page table
pub fn safe_translated_byte_buffer(
    memory_set: &MemorySet,
    ptr: *const u8,
    len: usize,
) -> Option<Vec<&'static mut [u8]>> {
    let page_table = PageTable::from_token(memory_set.token());
    let mut start = ptr as usize;
    let end = checked_user_range(start, len).ok()?;
    let mut v = Vec::new();
    while start < end {
        let start_va = VirtAddr::from(start);
        let mut vpn = start_va.floor();
        let ppn = translated_user_page(
            memory_set,
            &page_table,
            vpn,
            Trap::Exception(Exception::StorePageFault),
        )?;
        vpn.step();
        let mut end_va: VirtAddr = vpn.into();
        end_va = end_va.min(VirtAddr::from(end));
        if end_va.page_offset() == 0 {
            v.push(&mut ppn.bytes_array_mut()[start_va.page_offset()..]);
        } else {
            v.push(&mut ppn.bytes_array_mut()[start_va.page_offset()..end_va.page_offset()]);
        }
        start = end_va.into();
    }
    Some(v)
}

#[allow(unused)]
///Translate a generic through page table and return a reference
pub fn translated_ref<T>(token: usize, ptr: *const T) -> &'static T {
    let page_table = PageTable::from_token(token);
    let va = ptr as usize;
    KernelAddr::from(page_table.translate_va(VirtAddr::from(va)).unwrap()).as_ref()
}

///Translate a generic through page table and return a mutable reference
pub fn translated_refmut<T>(token: usize, ptr: *mut T) -> &'static mut T {
    let page_table = PageTable::from_token(token);
    let va = ptr as usize;
    KernelAddr::from(page_table.translate_va(VirtAddr::from(va)).unwrap()).as_mut()
}

/// 从 `token` 地址空间 `ptr` 处读取数据（fallible 版本）。
/// 如果地址无法翻译或跨页，返回 `None` 而不是 panic。
pub fn try_get_data<T: 'static + Copy>(token: usize, ptr: *const T) -> Option<T> {
    let page_table = PageTable::from_token(token);
    let va = match VirtAddr::try_from(ptr as usize) {
        Some(v) => v,
        None => return None, // non-canonical VA (e.g. corrupted robust list pointer)
    };
    // 对齐检查
    if ptr as usize % core::mem::align_of::<T>() != 0 {
        return None;
    }
    let pa = page_table.translate_va(va)?;
    let size = core::mem::size_of::<T>();
    // 若数据跨页，逐个字节翻译
    if (pa + size - 1).floor() != pa.floor() {
        let mut bytes = vec![0u8; size];
        let mut cur_va = va;
        for i in 0..size {
            let byte_pa = page_table.translate_va(cur_va)?;
            bytes[i] = *KernelAddr::from(byte_pa).as_ref();
            cur_va = cur_va + 1;
        }
        Some(unsafe { *(bytes.as_slice().as_ptr() as usize as *const T) })
    } else {
        Some(*KernelAddr::from(pa).as_ref::<T>())
    }
}

/// 从 `token` 地址空间 `ptr` 处读取数据，
/// 其中虚拟地址 `ptr` 解析得到的物理地址可以跨页。
///
/// 类型 `T` 需实现 Copy trait
pub fn get_data<T: 'static + Copy>(token: usize, ptr: *const T) -> T {
    let page_table = PageTable::from_token(token);
    let mut va = VirtAddr::from(ptr as usize);
    let pa = page_table.translate_va(va).unwrap();
    let size = core::mem::size_of::<T>();
    // 若数据跨页，则转换成字节数据写入
    if (pa + size - 1).floor() != pa.floor() {
        let mut bytes = vec![0u8; size];
        for i in 0..size {
            bytes[i] = *(page_table.translate_va(va).unwrap().as_ref());
            va = va + 1;
        }
        unsafe { *(bytes.as_slice().as_ptr() as usize as *const T) }
    } else {
        *translated_ref(token, ptr)
    }
}

/// 将数据 `data` 写入 `token` 地址空间 `ptr` 处，
/// 其中虚拟地址 `ptr` 解析得到的物理地址可以跨页
pub fn put_data<T: 'static>(token: usize, ptr: *mut T, data: T) {
    let page_table = PageTable::from_token(token);
    let mut va = VirtAddr::from(ptr as usize);
    let pa = page_table.translate_va(va).unwrap();
    let size = core::mem::size_of::<T>();
    // 若数据跨页，则转换成字节数据写入
    if (pa + size - 1).floor() != pa.floor() {
        let bytes =
            unsafe { core::slice::from_raw_parts(&data as *const _ as usize as *const u8, size) };
        for i in 0..size {
            *(page_table.translate_va(va).unwrap().as_mut()) = bytes[i];
            va = va + 1;
        }
    } else {
        *translated_refmut(token, ptr) = data;
    }
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

    let page_table = PageTable::from_token(memory_set.token());
    let mut cur_src = src;
    let mut cur_dst = 0;

    while cur_src < end {
        let start_va = VirtAddr::from(cur_src);
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
    let page_table = PageTable::from_token(memory_set.token());
    let mut cur_dst = dst;
    let mut cur_src = 0;

    while cur_dst < end {
        let start_va = VirtAddr::from(cur_dst);
        let vpn = start_va.floor();
        let ppn = translated_user_page(
            memory_set,
            &page_table,
            vpn,
            Trap::Exception(Exception::StorePageFault),
        )
        .ok_or(SysErrNo::EFAULT)?;

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

/// 安全地将用户虚拟地址转换为物理地址。
///
/// 遵循内核设计原则：先通过 `copy_from_user` 触发延迟页分配，
/// 确保页面已映射后再进行 VA→PA 转换。避免在未分配页面上直接
/// 调用 `translate_va` 导致 panic 或遗漏延迟分配。
pub fn translate_user_va_safe(
    memory_set: &MemorySet,
    va: VirtAddr,
) -> Result<usize, SysErrNo> {
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
        let vpn = VirtAddr::from(current_addr).floor();
        let page_end = ((vpn.0 + 1) << PAGE_SIZE_BITS) as usize;
        let chunk_size = core::cmp::min(page_end - current_addr, MAX_PATH_LEN - pos);

        // 尝试从用户空间复制这一块
        if copy_from_user(memory_set, current_addr, &mut dst_str[pos..pos + chunk_size]).is_err() {
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
    Ok(String::from(
        core::str::from_utf8(&dst_str).unwrap_or(""),
    ))
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
        let mut bytes = vec![0; len];
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
        if self.current_buffer >= self.buffers.len() {
            None
        } else {
            let r = &mut self.buffers[self.current_buffer][self.current_idx] as *mut _;
            if self.current_idx + 1 == self.buffers[self.current_buffer].len() {
                self.current_idx = 0;
                self.current_buffer += 1;
            } else {
                self.current_idx += 1;
            }
            Some(r)
        }
    }
}
