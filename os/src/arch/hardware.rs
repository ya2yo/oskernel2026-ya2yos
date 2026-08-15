//! 启动期硬件参数探测。
//!
//! 启动器通过 FDT 描述的硬件参数。
//!
//! RISC-V SBI 直接在 `a1` 传递 FDT；LoongArch QEMU 则通过启动器传入的
//! EFI system table 提供 FDT。这里实现一个不依赖堆的最小 reader，在
//! 清空 BSS 后尽早读取所有 RAM 段、CPU 和计时器参数。

use core::sync::atomic::{AtomicUsize, Ordering};

const MAX_RAM_RANGES: usize = 8;
/// 编译期每 Hart 状态容量；实际在线核数由启动器 FDT 提供。
pub const MAX_SUPPORTED_HARTS: usize = 16;
const DEFAULT_TIMEBASE_HZ: usize = 10_000_000;
const FDT_MAGIC: usize = 0xd00d_feed;
// FDT structure block token values defined by the flattened device tree
// format and shared with Linux/libfdt headers.
const FDT_BEGIN_NODE: usize = 1;
const FDT_END_NODE: usize = 2;
const FDT_PROP: usize = 3;
const FDT_NOP: usize = 4;
const FDT_END: usize = 9;
const EFI_SYSTEM_TABLE_SIGNATURE: usize = 0x5453_5953_2049_4249;
const EFI_SYSTEM_TABLE_HEADER_SIZE: usize = 24;
const EFI_SYSTEM_TABLE_CONFIGURATION_TABLE_COUNT_OFFSET: usize = 104;
const EFI_SYSTEM_TABLE_CONFIGURATION_TABLE_OFFSET: usize = 112;
const EFI_CONFIGURATION_TABLE_SIZE: usize = 24;
const EFI_DEVICE_TREE_GUID: [u8; 16] = [
    0xd5, 0x21, 0xb6, 0xb1, 0x9c, 0xf1, 0xa5, 0x41, 0x83, 0x0b, 0xd9, 0x15, 0x2c, 0x69, 0xaa, 0xe0,
];

static RAM_RANGE_COUNT: AtomicUsize = AtomicUsize::new(0);
static RAM_RANGE_STARTS: [AtomicUsize; MAX_RAM_RANGES] =
    [const { AtomicUsize::new(0) }; MAX_RAM_RANGES];
static RAM_RANGE_SIZES: [AtomicUsize; MAX_RAM_RANGES] =
    [const { AtomicUsize::new(0) }; MAX_RAM_RANGES];
static HART_COUNT: AtomicUsize = AtomicUsize::new(1);
static TIMEBASE_HZ: AtomicUsize = AtomicUsize::new(DEFAULT_TIMEBASE_HZ);

#[inline]
pub fn ram_start() -> usize {
    ram_range(0).map(|(start, _)| start).unwrap_or(0)
}
#[inline]
pub fn ram_size() -> usize {
    let mut total = 0usize;
    for index in 0..ram_range_count() {
        let Some((_, size)) = ram_range(index) else {
            break;
        };
        total = total.saturating_add(size);
    }
    total
}
#[inline]
pub fn ram_range_count() -> usize {
    RAM_RANGE_COUNT.load(Ordering::Acquire)
}
#[inline]
pub fn ram_range(index: usize) -> Option<(usize, usize)> {
    if index >= ram_range_count() {
        return None;
    }
    Some((
        RAM_RANGE_STARTS[index].load(Ordering::Acquire),
        RAM_RANGE_SIZES[index].load(Ordering::Acquire),
    ))
}
#[inline]
pub fn hart_count() -> usize {
    HART_COUNT.load(Ordering::Acquire)
}
#[inline]
pub fn timebase_hz() -> usize {
    TIMEBASE_HZ.load(Ordering::Acquire)
}

#[inline]
fn be32(p: *const u8) -> usize {
    unsafe { u32::from_be_bytes([*p, *p.add(1), *p.add(2), *p.add(3)]) as usize }
}

#[inline]
fn be64(p: *const u8) -> usize {
    unsafe {
        u64::from_be_bytes([
            *p,
            *p.add(1),
            *p.add(2),
            *p.add(3),
            *p.add(4),
            *p.add(5),
            *p.add(6),
            *p.add(7),
        ]) as usize
    }
}

fn cells(p: usize, count: usize, end: usize) -> Option<usize> {
    let bytes = count.checked_mul(4)?;
    if p > end || bytes > end - p {
        return None;
    }
    let mut value = 0usize;
    for index in 0..count {
        value = value
            .checked_shl(32)?
            .checked_add(be32((p + index * 4) as *const u8))?;
    }
    Some(value)
}

#[inline]
fn align4(value: usize) -> usize {
    (value + 3) & !3
}

fn reset_from_bootloader() {
    RAM_RANGE_COUNT.store(0, Ordering::Release);
    for index in 0..MAX_RAM_RANGES {
        RAM_RANGE_STARTS[index].store(0, Ordering::Release);
        RAM_RANGE_SIZES[index].store(0, Ordering::Release);
    }
    HART_COUNT.store(1, Ordering::Release);
    TIMEBASE_HZ.store(DEFAULT_TIMEBASE_HZ, Ordering::Release);
}

fn add_ram_range(start: usize, size: usize) -> bool {
    if size == 0 || start % 4096 != 0 || size % 4096 != 0 {
        return false;
    }
    let index = RAM_RANGE_COUNT.load(Ordering::Relaxed);
    if index == MAX_RAM_RANGES || start.checked_add(size).is_none() {
        return false;
    }
    RAM_RANGE_STARTS[index].store(start, Ordering::Relaxed);
    RAM_RANGE_SIZES[index].store(size, Ordering::Relaxed);
    RAM_RANGE_COUNT.store(index + 1, Ordering::Release);
    true
}

/// 从启动器提供的 FDT 读取平台参数。
///
/// 返回 `false` 表示启动器未传递有效 FDT 或 FDT 没有可用的 RAM `reg` 段。
pub fn init_from_fdt(fdt: usize) -> bool {
    reset_from_bootloader();
    if fdt == 0 || fdt & 3 != 0 {
        return false;
    }
    let base = fdt as *const u8;
    if be32(base) != FDT_MAGIC {
        return false;
    }
    let total = be32(unsafe { base.add(4) });
    let struct_off = be32(unsafe { base.add(8) });
    let strings_off = be32(unsafe { base.add(12) });
    if total < struct_off || total < strings_off || total > 16 * 1024 * 1024 {
        return false;
    }
    let end = fdt.saturating_add(total);
    let strings = fdt.saturating_add(strings_off);
    let mut cursor = fdt.saturating_add(struct_off);
    let mut depth = 0usize;
    let mut node = [0u8; 32];
    let mut memory_depth = 0usize;
    let mut cpus_depth = 0usize;
    let mut cpu_count = 0usize;
    let mut address_cells = 2usize;
    let mut size_cells = 2usize;
    while cursor + 4 <= end {
        let token = be32(cursor as *const u8);
        cursor += 4;
        match token {
            FDT_BEGIN_NODE => {
                let start = cursor;
                while cursor < end && unsafe { *(cursor as *const u8) } != 0 {
                    cursor += 1;
                }
                let len = (cursor - start).min(node.len());
                node[..len].copy_from_slice(unsafe {
                    core::slice::from_raw_parts(start as *const u8, len)
                });
                depth += 1;
                let node_name = &node[..len];
                if node_name.starts_with(b"memory") {
                    memory_depth = depth;
                }
                if node_name == b"cpus" {
                    cpus_depth = depth;
                }
                if cpus_depth != 0 && depth == cpus_depth + 1 && node_name.starts_with(b"cpu@") {
                    cpu_count += 1;
                }
                cursor = align4(cursor + 1);
            }
            FDT_END_NODE => {
                if depth == memory_depth {
                    memory_depth = 0;
                }
                if depth == cpus_depth {
                    cpus_depth = 0;
                }
                depth = depth.saturating_sub(1);
            }
            FDT_PROP => {
                if cursor + 8 > end {
                    break;
                }
                let len = be32(cursor as *const u8);
                let nameoff = be32((cursor + 4) as *const u8);
                cursor += 8;
                let value = cursor;
                if value > end || len > end - value {
                    break;
                }
                if strings + nameoff < end {
                    let name = (strings + nameoff) as *const u8;
                    let mut name_len = 0usize;
                    while strings + nameoff + name_len < end && unsafe { *name.add(name_len) } != 0
                    {
                        name_len += 1;
                    }
                    let prop = unsafe { core::slice::from_raw_parts(name, name_len) };
                    if prop == b"timebase-frequency" && len >= 4 {
                        TIMEBASE_HZ.store(be32(value as *const u8), Ordering::Release);
                    } else if depth == 1 && prop == b"#address-cells" && len >= 4 {
                        address_cells = be32(value as *const u8).min(2);
                    } else if depth == 1 && prop == b"#size-cells" && len >= 4 {
                        size_cells = be32(value as *const u8).min(2);
                    } else if prop == b"reg"
                        && depth == memory_depth
                        && len >= (address_cells + size_cells) * 4
                    {
                        let entry_size = (address_cells + size_cells) * 4;
                        let mut entry = value;
                        let reg_end = value + len;
                        while entry + entry_size <= reg_end {
                            let Some(start) = cells(entry, address_cells, end) else {
                                break;
                            };
                            let Some(size) = cells(entry + address_cells * 4, size_cells, end)
                            else {
                                break;
                            };
                            if !add_ram_range(start, size) {
                                break;
                            }
                            entry += entry_size;
                        }
                    }
                }
                cursor = align4(value.saturating_add(len));
            }
            FDT_NOP => {}
            FDT_END => break,
            _ => break,
        }
    }
    if cpu_count != 0 {
        HART_COUNT.store(cpu_count.min(MAX_SUPPORTED_HARTS), Ordering::Release);
    }
    ram_range_count() != 0
}

/// Locate the FDT published by the LoongArch direct-kernel bootloader's EFI
/// system table. `system_table_offset` is the physical offset passed in `a2`.
#[cfg(target_arch = "loongarch64")]
pub fn loongarch_fdt_from_efi(system_table_offset: usize) -> Option<usize> {
    if system_table_offset == 0 || system_table_offset & 7 != 0 {
        return None;
    }
    let direct = crate::arch::memory_layout::KERNEL_ADDR_OFFSET;
    let system_table = direct.checked_add(system_table_offset)? as *const u8;
    if unsafe { core::ptr::read_unaligned(system_table as *const usize) }
        != EFI_SYSTEM_TABLE_SIGNATURE
    {
        return None;
    }
    let table_count = unsafe {
        core::ptr::read_unaligned(
            system_table.add(EFI_SYSTEM_TABLE_CONFIGURATION_TABLE_COUNT_OFFSET) as *const usize,
        )
    };
    let tables_offset = unsafe {
        core::ptr::read_unaligned(
            system_table.add(EFI_SYSTEM_TABLE_CONFIGURATION_TABLE_OFFSET) as *const usize,
        )
    };
    if table_count > 32 || tables_offset < EFI_SYSTEM_TABLE_HEADER_SIZE {
        return None;
    }
    let tables = direct.checked_add(tables_offset)? as *const u8;
    for index in 0..table_count {
        let table = unsafe { tables.add(index.checked_mul(EFI_CONFIGURATION_TABLE_SIZE)?) };
        let guid = unsafe { core::slice::from_raw_parts(table, EFI_DEVICE_TREE_GUID.len()) };
        if guid == EFI_DEVICE_TREE_GUID {
            let fdt = unsafe { core::ptr::read_unaligned(table.add(16) as *const usize) };
            if fdt != 0 {
                return direct.checked_add(fdt);
            }
        }
    }
    None
}
