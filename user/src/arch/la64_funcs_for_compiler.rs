// 因为在.cargo/config中指定"-Clink-arg=-nostdlib"，链接时不会链接标准库
// 需要实现一些C标准库函数给链接器链接

#[no_mangle]
pub unsafe extern "C" fn memcpy(dest: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    let mut i = 0;
    while i < n {
        *dest.add(i) = *src.add(i);
        i += 1;
    }
    dest
}

#[no_mangle]
pub unsafe extern "C" fn memset(s: *mut u8, c: i32, n: usize) -> *mut u8 {
    for i in 0..n {
        *s.add(i) = c as u8;
    }
    s
}

// 在 Rust 中定义一个简单的 bcmp
#[no_mangle]
pub extern "C" fn bcmp(s1: *const u8, s2: *const u8, n: usize) -> i32 {
    let slice1 = unsafe { core::slice::from_raw_parts(s1, n) };
    let slice2 = unsafe { core::slice::from_raw_parts(s2, n) };
    if slice1 == slice2 {
        0
    } else {
        1
    }
}
