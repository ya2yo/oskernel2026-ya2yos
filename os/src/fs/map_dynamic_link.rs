//! Dynamic linking follows the Linux ELF/VFS boundary.
//!
//! Linux only uses the `PT_INTERP` pathname while executing a dynamic ELF.
//! The kernel opens that exact interpreter path and transfers control to it.
//! Dependency lookup (`DT_NEEDED`, `RPATH`/`RUNPATH`, `ld.so.cache`), symlink
//! resolution, ABI selection, and relocations remain userspace dynamic-loader
//! responsibilities.  Consequently this module intentionally provides no
//! pathname rewrite table and no read-time shared-object byte patches.
//!
//! Keeping the boundary explicit prevents a kernel built for one rootfs image
//! from silently substituting a different libc or loader for another image.

// This module is intentionally kept as a named boundary for filesystem code
// and documentation.  Dynamic-link policy is implemented by the ELF loader
// and userspace `ld.so`, not by a kernel pathname compatibility table.
