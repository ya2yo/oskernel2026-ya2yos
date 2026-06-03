//! linux-7.0/include/uapi/asm-generic/fcntl.h
//! linux-7.0/include/uapi/linux/fcntl.h
pub const F_DUPFD: u32 = 0; /* dup */
pub const F_GETFD: u32 = 1; /* get close_on_exec */
pub const F_SETFD: u32 = 2; /* set/clear close_on_exec */
pub const F_GETFL: u32 = 3; /* get file->f_flags */
pub const F_SETFL: u32 = 4; /* set file->f_flags */
pub const F_GETLK: u32 = 5;
pub const F_SETLK: u32 = 6;
pub const F_SETLKW: u32 = 7;
pub const F_SETOWN: u32 = 8; /* for sockets. */
pub const F_GETOWN: u32 = 9; /* for sockets. */
pub const F_SETSIG: u32 = 10; /* for sockets. */
pub const F_GETSIG: u32 = 11; /* for sockets. */
pub const F_GETLK64: u32 = 12; /* using 'struct flock64' */
pub const F_SETLK64: u32 = 13;
pub const F_SETLKW64: u32 = 14;
pub const F_SETOWN_EX: u32 = 15;
pub const F_GETOWN_EX: u32 = 16;
pub const F_OFD_GETLK: u32 = 36;
pub const F_OFD_SETLK: u32 = 37;
pub const F_OFD_SETLKW: u32 = 38;
pub const F_SETLEASE: u32 = 1024;
pub const F_GETLEASE: u32 = 1025;
pub const F_NOTIFY: u32 = 1026;
pub const F_DUPFD_QUERY: u32 = 1027;
pub const F_DUPFD_CLOEXEC: u32 = 1030;
pub const F_SETPIPE_SZ: u32 = 1031;
pub const F_GETPIPE_SZ: u32 = 1032;

/* for F_[GET|SET]FL */
pub const FD_CLOEXEC: u32 = 1; /* actually anything with low bit set goes */

/* for F_[GET|SET]LK */
pub const F_RDLCK: i16 = 0;
pub const F_WRLCK: i16 = 1;
pub const F_UNLCK: i16 = 2;

/* for F_SETOWN_EX */
pub const F_OWNER_TID: i32 = 0;
pub const F_OWNER_PID: i32 = 1;
pub const F_OWNER_PGRP: i32 = 2;

/* for F_NOTIFY */
pub const DN_ACCESS: u32 = 0x00000001;
pub const DN_MODIFY: u32 = 0x00000002;
pub const DN_CREATE: u32 = 0x00000004;
pub const DN_DELETE: u32 = 0x00000008;
pub const DN_RENAME: u32 = 0x00000010;
pub const DN_ATTRIB: u32 = 0x00000020;
pub const DN_MULTISHOT: u32 = 0x80000000;

pub const SEEK_SET: i16 = 0;
pub const SEEK_CUR: i16 = 1;
pub const SEEK_END: i16 = 2;
