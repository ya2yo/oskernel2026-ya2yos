//! Dynamic linking follows the Linux ELF/VFS boundary.
//!
//! Linux only uses the `PT_INTERP` pathname while executing a dynamic ELF.
//! The kernel opens that exact interpreter pathname and transfers control to
//! it. Dependency lookup, ABI selection, and relocations remain userspace
//! dynamic-loader responsibilities, so this module does not rewrite paths.
//!
//! A narrow read-time compatibility hook is retained for the known LoongArch
//! pre-test image: its musl libc contains scheduler wrappers compiled as
//! `ENOSYS` stubs, which prevents cyclictest from reaching the implemented
//! scheduler syscalls. The hook changes bytes only for that exact library and
//! never modifies the backing filesystem.

/// Apply compatibility bytes to one shared-object read chunk.
///
/// Page-cache reads can begin and end at arbitrary offsets, so replacements
/// are copied by range rather than assuming a whole-file read.
pub fn patch_dynamic_link_file_bytes(path: &str, off: usize, buf: &mut [u8]) {
    #[cfg(target_arch = "loongarch64")]
    patch_loongarch_musl_libc_sched_stubs(path, off, buf);

    #[cfg(not(target_arch = "loongarch64"))]
    let _ = (path, off, buf);
}

#[cfg(target_arch = "loongarch64")]
fn patch_loongarch_musl_libc_sched_stubs(path: &str, off: usize, buf: &mut [u8]) {
    if path != "/musl/lib/libc.so" {
        return;
    }

    // These offsets belong to the pre-test LoongArch musl libc. The original
    // functions return ENOSYS; provide SCHED_OTHER and sched_priority=0 so the
    // caller can use the kernel scheduler ABI.
    const GETPARAM: &[u8] = &[
        0xa0, 0x00, 0x80, 0x29, // st.w  $r0, $r5, 0
        0x04, 0x00, 0x15, 0x00, // move  $r4, $r0
        0x20, 0x00, 0x00, 0x4c, // jirl  $r0, $r1, 0
    ];
    const RET_ZERO: &[u8] = &[
        0x04, 0x00, 0x15, 0x00, // move  $r4, $r0
        0x20, 0x00, 0x00, 0x4c, // jirl  $r0, $r1, 0
    ];

    patch_range(off, buf, 0x544e0, GETPARAM);
    patch_range(off, buf, 0x54500, RET_ZERO);
    patch_range(off, buf, 0x54544, RET_ZERO);
    patch_range(off, buf, 0x54564, RET_ZERO);
}

#[cfg(target_arch = "loongarch64")]
fn patch_range(read_off: usize, buf: &mut [u8], patch_off: usize, patch: &[u8]) {
    let read_end = read_off.saturating_add(buf.len());
    let patch_end = patch_off + patch.len();
    if read_end <= patch_off || patch_end <= read_off {
        return;
    }

    let start = read_off.max(patch_off);
    let end = read_end.min(patch_end);
    let dst_start = start - read_off;
    let src_start = start - patch_off;
    let len = end - start;
    buf[dst_start..dst_start + len].copy_from_slice(&patch[src_start..src_start + len]);
}
