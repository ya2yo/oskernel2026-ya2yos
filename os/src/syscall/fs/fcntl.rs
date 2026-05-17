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
pub const F_DUPFD_CLOEXEC: u32 = 1030;

/* for F_[GET|SET]FL */
pub const FD_CLOEXEC: u32 = 1; /* actually anything with low bit set goes */
