//! Implementation of [`PageTableEntry`] and [`PageTable`].
use crate::{
    arch::{memory_layout::PAGE_SIZE, time::get_ticks},
    mm::{KernelAddr, MapPermission, PhysPageNum, VirtPageNum, address, memory_set},
    utils::{SysErrNo, SyscallRet},
};

use super::{MemorySet, StepByOne, VirtAddr};
use alloc::{string::String, sync::Arc, vec, vec::Vec};

use crate::trap::trap_types::*;
use log::debug;

use crate::arch::page_table::PageTable;

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
                debug!("vpn {:#x} not found", vpn.0);
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
    let end = start + len;
    let mut v = Vec::new();
    while start < end {
        let start_va = VirtAddr::from(start);
        let mut vpn = start_va.floor();
        let ppn = match page_table.translate(vpn) {
            Some(ppn) => ppn,
            None => {
                memory_set.lazy_page_fault(vpn, Trap::Exception(Exception::LoadPageFault));
                page_table.translate(vpn).unwrap()
            }
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

/// Translate a pointer to a mutable u8 Vec end with `\0` through page table to a `String`
pub fn translated_str(token: usize, ptr: *const u8) -> String {
    let page_table = PageTable::from_token(token);
    let mut string = String::new();
    let mut va = ptr as usize;
    loop {
        let ch: u8 =
            *(KernelAddr::from(page_table.translate_va(VirtAddr::from(va)).unwrap()).as_mut());
        if ch == 0 {
            break;
        }
        string.push(ch as char);
        va += 1;
    }
    string
}

#[allow(unused)]
///Translate a generic through page table and return a reference
pub fn translated_ref<T>(token: usize, ptr: *const T) -> &'static T {
    let page_table = PageTable::from_token(token);
    let va = ptr as usize;
    KernelAddr::from(page_table.translate_va(VirtAddr::from(va)).unwrap()).as_ref()
}

pub fn safe_translated_ref<T>(memory_set: &MemorySet, ptr: *const T) -> &'static T {
    let page_table = PageTable::from_token(memory_set.token());
    let va = ptr as usize;
    let start_va = VirtAddr::from(va);
    let vpn = start_va.floor();
    if let None = page_table.translate(vpn) {
        memory_set.lazy_page_fault(vpn, Trap::Exception(Exception::LoadPageFault));
    }
    KernelAddr::from(page_table.translate_va(VirtAddr::from(va)).unwrap()).as_ref()
}
///Translate a generic through page table and return a mutable reference
pub fn translated_refmut<T>(token: usize, ptr: *mut T) -> &'static mut T {
    let page_table = PageTable::from_token(token);
    let va = ptr as usize;
    KernelAddr::from(page_table.translate_va(VirtAddr::from(va)).unwrap()).as_mut()
}

/// 安全地将用户空间指针翻译为内核态的可变引用
/// token: 进程页表的 token
/// ptr: 用户空间的原始指针
pub fn strong_translated_refmut<T>(token: usize, ptr: *mut T) -> Option<&'static mut T> {
    let page_table = PageTable::from_token(token);
    let va = ptr as usize;
    // 检查对齐
    if va % core::mem::align_of::<T>() != 0 {
        return None;
    }
    // 检查是否跨页边界
    // 如果对象跨越了页面，简单的物理地址转换是不够的，通常需要分段读写或临时映射
    let size = core::mem::size_of::<T>();
    if (va % PAGE_SIZE) + size > PAGE_SIZE {
        // 对于简单的 PID写入，通常不会跨页，但作为通用函数必须考虑
        return None;
    }
    page_table.translate_va(VirtAddr::from(va)).map(|pa| {
        // 转换为内核虚拟地址并转为引用
        // 注意：这里返回的生命周期应该绑定在调用者身上，而不是 'static
        KernelAddr::from(pa).as_mut()
    })
}

pub fn safe_translated_refmut<T>(memory_set: &MemorySet, ptr: *mut T) -> &'static mut T {
    let page_table = PageTable::from_token(memory_set.token());
    let va = ptr as usize;
    let start_va = VirtAddr::from(va);
    let vpn = start_va.floor();
    if let None = page_table.translate(vpn) {
        memory_set.lazy_page_fault(vpn, Trap::Exception(Exception::LoadPageFault));
    }
    KernelAddr::from(page_table.translate_va(VirtAddr::from(va)).unwrap()).as_mut()
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

pub fn safe_get_data<T: 'static + Copy>(memory_set: &MemorySet, ptr: *const T) -> T {
    let page_table = PageTable::from_token(memory_set.token());
    let mut va = VirtAddr::from(ptr as usize);
    let pa = page_table.translate_va(va).unwrap();
    let size = core::mem::size_of::<T>();
    // 若数据跨页，则转换成字节数据写入
    if (pa + size - 1).floor() != pa.floor() {
        debug!("work in overpage");
        let mut bytes = vec![0u8; size];
        for i in 0..size {
            bytes[i] = *(page_table.translate_va(va).unwrap().as_ref());
            va = va + 1;
        }
        unsafe { *(bytes.as_slice().as_ptr() as usize as *const T) }
    } else {
        *safe_translated_ref(memory_set, ptr)
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

pub fn safe_put_data<T: 'static>(memory_set: &MemorySet, ptr: *mut T, data: T) {
    let page_table = PageTable::from_token(memory_set.token());
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
        *safe_translated_refmut(memory_set, ptr) = data;
    }
}

/// 将数据从用户空间安全地复制到内核空间
///
/// - `token`: 源用户空间的页表 token
/// - `src`: 用户空间的源虚拟地址
/// - `dst`: 内核空间的目标缓冲区
///
/// 成功返回 `Ok(复制的字节数)`，失败返回 `Err(EFAULT)`
pub fn copy_from_user(token: usize, src: usize, dst: &mut [u8]) -> SyscallRet {
    let len = dst.len();
    if len == 0 {
        return Ok(0);
    }
    if src == 0 || src.checked_add(len).is_none() {
        return Err(SysErrNo::EFAULT);
    }

    let page_table = PageTable::from_token(token);
    let end = src + len;
    let mut cur_src = src;
    let mut cur_dst = 0;

    while cur_src < end {
        let start_va = VirtAddr::from(cur_src);
        let vpn = start_va.floor();
        let ppn = page_table.translate(vpn).ok_or(SysErrNo::EFAULT)?;
        // 本页内可复制的字节数：从当前偏移到页末，或到 end
        let next_page_va: usize = start_va.ceil().into();
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
pub fn copy_to_user(token: usize, dst: usize, src: &[u8]) -> SyscallRet {
    let len = src.len();
    if len == 0 {
        return Ok(0);
    }
    if dst == 0 || dst.checked_add(len).is_none() {
        return Err(SysErrNo::EFAULT);
    }
    let page_table = PageTable::from_token(token);
    let end = dst + len;
    let mut cur_dst = dst;
    let mut cur_src = 0;

    while cur_dst < end {
        let start_va = VirtAddr::from(cur_dst);
        let vpn = start_va.floor();
        let ppn = page_table.translate(vpn).ok_or(SysErrNo::EFAULT)?;

        // 本页内可复制的字节数：从当前偏移到页末，或到 end
        let next_page_va: usize = start_va.ceil().into();
        let copy_len = (end - cur_dst).min(next_page_va - cur_dst);

        let dst_slice =
            &mut ppn.bytes_array_mut()[start_va.page_offset()..start_va.page_offset() + copy_len];
        dst_slice.copy_from_slice(&src[cur_src..cur_src + copy_len]);

        cur_dst += copy_len;
        cur_src += copy_len;
    }

    Ok(len)
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
