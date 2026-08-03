use alloc::alloc::{alloc, dealloc, Layout};
use alloc::slice::from_raw_parts_mut;
use alloc::string::String;
use core::cmp::min;
use core::ffi::{c_char, c_int, c_size_t, c_void};

#[cfg(feature = "print")]
#[linkage = "weak"]
#[no_mangle]
unsafe extern "C" fn printf(str: *const c_char, args: ...) -> c_int {
    // extern "C" { pub fn printf(arg1: *const c_char, ...) -> c_int; }
    use printf_compat::{format, output};

    let mut s = String::new();
    let bytes_written = format(str as _, args, output::fmt_write(&mut s));
    //println!("{}", s);
    info!("{}", s);

    bytes_written
}

#[cfg(not(feature = "print"))]
#[linkage = "weak"]
#[no_mangle]
unsafe extern "C" fn printf(str: *const c_char, mut args: ...) -> c_int {
    use core::ffi::CStr;
    let c_str = unsafe { CStr::from_ptr(str) };
    //let arg1 = args.arg::<usize>();

    info!("[lwext4] {:?}", c_str);
    0
}

#[no_mangle]
pub extern "C" fn ext4_user_malloc(size: c_size_t) -> *mut c_void {
    malloc(size)
}

#[no_mangle]
pub extern "C" fn ext4_user_calloc(m: c_size_t, n: c_size_t) -> *mut c_void {
    calloc(m, n)
}

#[no_mangle]
pub extern "C" fn ext4_user_realloc(memblock: *mut c_void, size: c_size_t) -> *mut c_void {
    realloc(memblock, size)
}

#[linkage = "weak"]
#[no_mangle]
pub extern "C" fn calloc(m: c_size_t, n: c_size_t) -> *mut c_void {
    let Some(size) = m.checked_mul(n) else {
        return core::ptr::null_mut();
    };
    let mem = malloc(size);
    if mem.is_null() {
        return mem;
    }

    extern "C" {
        pub fn memset(dest: *mut c_void, c: c_int, n: c_size_t) -> *mut c_void;
    }
    unsafe { memset(mem, 0, m * n) }
}

#[linkage = "weak"]
#[no_mangle]
pub extern "C" fn realloc(memblock: *mut c_void, size: c_size_t) -> *mut c_void {
    if memblock.is_null() {
        warn!("realloc a a null mem pointer");
        return malloc(size);
    }

    let ptr = memblock.cast::<MemoryControlBlock>();
    let header = unsafe { ptr.sub(1).read() };
    if header.magic != ALLOC_MAGIC || header.size_tag != (header.size ^ ALLOC_MAGIC) {
        return core::ptr::null_mut();
    }
    let old_size = header.size;
    info!("realloc from {} to {}", old_size, size);

    let mem = malloc(size);
    if mem.is_null() {
        return mem;
    }

    unsafe {
        let old_size = min(size, old_size);
        let mbuf = from_raw_parts_mut(mem as *mut u8, old_size);
        mbuf.copy_from_slice(from_raw_parts_mut(memblock as *mut u8, old_size));
    }
    free(memblock);

    mem
}

#[no_mangle]
pub extern "C" fn ext4_user_free(p: *mut c_void) {
    free(p)
}

struct MemoryControlBlock {
    magic: usize,
    size: usize,
    size_tag: usize,
}
const CTRL_BLK_SIZE: usize = core::mem::size_of::<MemoryControlBlock>();
const ALLOC_MAGIC: usize = 0x5941_324f_5341_4c4c;

#[inline]
fn allocation_layout(size: usize) -> Option<Layout> {
    let total = size.checked_add(CTRL_BLK_SIZE)?;
    Layout::from_size_align(total, core::mem::align_of::<MemoryControlBlock>()).ok()
}

/// Allocate size bytes memory and return the memory address.
#[linkage = "weak"]
#[no_mangle]
pub extern "C" fn malloc(size: c_size_t) -> *mut c_void {
    let Some(layout) = allocation_layout(size) else {
        return core::ptr::null_mut();
    };
    unsafe {
        let ptr = alloc(layout);
        if ptr.is_null() {
            return core::ptr::null_mut();
        }
        //debug!("malloc {}@{:p}", size + CTRL_BLK_SIZE, ptr);

        let ptr = ptr.cast::<MemoryControlBlock>();
        ptr.write(MemoryControlBlock {
            magic: ALLOC_MAGIC,
            size,
            size_tag: size ^ ALLOC_MAGIC,
        });
        ptr.add(1).cast()
    }
}

/// Deallocate memory at ptr address
#[linkage = "weak"]
#[no_mangle]
pub extern "C" fn free(ptr: *mut c_void) {
    if ptr.is_null() {
        warn!("free a null pointer !");
        return;
    }
    //debug!("free pointer {:p}", ptr);

    let ptr = ptr.cast::<MemoryControlBlock>();
    unsafe {
        let ptr = ptr.sub(1);
        let header = ptr.read();
        if header.magic != ALLOC_MAGIC || header.size_tag != (header.size ^ ALLOC_MAGIC) {
            return;
        }
        let Some(layout) = allocation_layout(header.size) else {
            return;
        };
        dealloc(ptr.cast(), layout)
    }
}
