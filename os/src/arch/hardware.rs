//! 启动期硬件参数探测。
//!
//! RISC-V SBI 启动约定把 FDT 地址放在 `a1`。这里实现一个不依赖堆的
//! 最小 FDT reader，在清空 BSS 后尽早读取内存、CPU 和计时器参数。

use core::sync::atomic::{AtomicUsize, Ordering};

pub const DEFAULT_RAM_START: usize = 0x8000_0000;
pub const DEFAULT_RAM_SIZE: usize = 0x4_0000_0000;
pub const DEFAULT_TIMEBASE_HZ: usize = 10_000_000;

static RAM_START: AtomicUsize = AtomicUsize::new(DEFAULT_RAM_START);
static RAM_SIZE: AtomicUsize = AtomicUsize::new(DEFAULT_RAM_SIZE);
static HART_COUNT: AtomicUsize = AtomicUsize::new(1);
static TIMEBASE_HZ: AtomicUsize = AtomicUsize::new(DEFAULT_TIMEBASE_HZ);

#[inline]
pub fn ram_start() -> usize {
    RAM_START.load(Ordering::Acquire)
}
#[inline]
pub fn ram_size() -> usize {
    RAM_SIZE.load(Ordering::Acquire)
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

/// 从启动固件提供的 FDT 读取平台参数。解析失败时保留架构默认值。
pub fn init_from_fdt(fdt: usize) {
    // `clear_bss()` runs before this function, so Atomics whose initializers
    // reside in BSS must be restored before any malformed-FDT fallback.
    RAM_START.store(DEFAULT_RAM_START, Ordering::Release);
    RAM_SIZE.store(DEFAULT_RAM_SIZE, Ordering::Release);
    HART_COUNT.store(1, Ordering::Release);
    TIMEBASE_HZ.store(DEFAULT_TIMEBASE_HZ, Ordering::Release);
    if fdt == 0 || fdt & 3 != 0 {
        return;
    }
    let base = fdt as *const u8;
    if be32(base) != 0xd00d_feed {
        return;
    }
    let total = be32(unsafe { base.add(4) });
    let struct_off = be32(unsafe { base.add(8) });
    let strings_off = be32(unsafe { base.add(12) });
    if total < struct_off || total < strings_off || total > 16 * 1024 * 1024 {
        return;
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
            1 => {
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
            2 => {
                if depth == memory_depth {
                    memory_depth = 0;
                }
                if depth == cpus_depth {
                    cpus_depth = 0;
                }
                depth = depth.saturating_sub(1);
            }
            3 => {
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
                        if let (Some(start), Some(size)) = (
                            cells(value, address_cells, end),
                            cells(value + address_cells * 4, size_cells, end),
                        ) {
                            if size != 0 {
                                RAM_START.store(start, Ordering::Release);
                                RAM_SIZE.store(size, Ordering::Release);
                            }
                        }
                    }
                }
                cursor = align4(value.saturating_add(len));
            }
            4 => {}
            9 => break,
            _ => break,
        }
    }
    if cpu_count != 0 {
        HART_COUNT.store(
            cpu_count.min(crate::arch::config::HART_NUM),
            Ordering::Release,
        );
    }
}

pub fn init_without_fdt() {
    #[cfg(target_arch = "loongarch64")]
    {
        RAM_START.store(0, Ordering::Release);
        RAM_SIZE.store(0x9_0000_0000, Ordering::Release);
        HART_COUNT.store(crate::arch::config::HART_NUM, Ordering::Release);
    }
    #[cfg(not(target_arch = "loongarch64"))]
    HART_COUNT.store(1, Ordering::Release);
}
