use alloc::string::String;

use crate::{
    fs::{
        open, suppress_fanotify_events, FanotifyFd, FileClass, FileDescriptor, OpenFlags, StMode,
        NONE_MODE,
    },
    mm::read_user_cstr,
    task::current_task,
    utils::{SysErrNo, SyscallRet},
};

/// 创建 fanotify fd 时设置 close-on-exec。
const FAN_CLOEXEC: u32 = 0x0000_0001;
/// 创建 fanotify fd 时启用非阻塞读。
const FAN_NONBLOCK: u32 = 0x0000_0002;
/// 普通通知类 fanotify group，不拦截文件访问。
const FAN_CLASS_NOTIF: u32 = 0x0000_0000;
/// 内容类 fanotify group，可用于权限事件。
const FAN_CLASS_CONTENT: u32 = 0x0000_0004;
/// 预内容类 fanotify group，优先级高于 `FAN_CLASS_CONTENT`。
const FAN_CLASS_PRE_CONTENT: u32 = 0x0000_0008;
/// fanotify class 位掩码。三个 class 互斥，`FAN_CLASS_NOTIF` 的值为 0。
const FAN_CLASS_BITS: u32 = FAN_CLASS_CONTENT | FAN_CLASS_PRE_CONTENT;
/// 请求不限制事件队列长度；当前只做参数兼容。
const FAN_UNLIMITED_QUEUE: u32 = 0x0000_0010;
/// 请求不限制 mark 数量；当前只做参数兼容。
const FAN_UNLIMITED_MARKS: u32 = 0x0000_0020;
/// 请求审计集成；当前只做参数兼容。
const FAN_ENABLE_AUDIT: u32 = 0x0000_0040;
/// 事件携带 pidfd 信息。
const FAN_REPORT_PIDFD: u32 = 0x0000_0080;
/// 事件按线程 ID 报告。
const FAN_REPORT_TID: u32 = 0x0000_0100;
/// 事件使用 file handle 形式报告目标。
const FAN_REPORT_FID: u32 = 0x0000_0200;
/// 事件报告父目录 file handle。
const FAN_REPORT_DIR_FID: u32 = 0x0000_0400;
/// 事件报告目录项名称；Linux 要求同时设置 `FAN_REPORT_DIR_FID`。
const FAN_REPORT_NAME: u32 = 0x0000_0800;
/// rename 等事件报告目标 file handle；Linux 要求依赖 FID、DIR_FID 和 NAME。
const FAN_REPORT_TARGET_FID: u32 = 0x0000_1000;
/// 当前 `fanotify_init` 接受的 init flags 集合。
///
/// 这些 flag 已经足够创建 fanotify fd 并让 LTP 能进入后续 `fanotify_mark`
/// 探测；真实事件投递仍需要后续实现 mark 表和 VFS hook。
const FANOTIFY_INIT_SUPPORTED_FLAGS: u32 = FAN_CLOEXEC
    | FAN_NONBLOCK
    | FAN_CLASS_BITS
    | FAN_UNLIMITED_QUEUE
    | FAN_UNLIMITED_MARKS
    | FAN_ENABLE_AUDIT
    | FAN_REPORT_PIDFD
    | FAN_REPORT_TID
    | FAN_REPORT_FID
    | FAN_REPORT_DIR_FID
    | FAN_REPORT_NAME
    | FAN_REPORT_TARGET_FID;
/// `event_f_flags` 当前接受的 open flags。
///
/// Linux 会用这些 flags 打开事件中返回的对象 fd；当前内核尚未生成事件 fd，
/// 但这里先按 ABI 做基本校验，避免非法参数被接受。
const FANOTIFY_EVENT_F_FLAGS_SUPPORTED: u32 =
    OpenFlags::O_ACCMODE.bits() | OpenFlags::O_LARGEFILE.bits() | OpenFlags::O_CLOEXEC.bits();

/// `fanotify_mark()` 添加 mark。
const FAN_MARK_ADD: u32 = 0x0000_0001;
/// `fanotify_mark()` 移除 mark。
const FAN_MARK_REMOVE: u32 = 0x0000_0002;
/// 解析路径时不跟随末尾符号链接。
const FAN_MARK_DONT_FOLLOW: u32 = 0x0000_0004;
/// 目标必须是目录。
const FAN_MARK_ONLYDIR: u32 = 0x0000_0008;
/// 以 mount 为粒度建立 mark。
const FAN_MARK_MOUNT: u32 = 0x0000_0010;
/// 操作 ignore mask。
const FAN_MARK_IGNORED_MASK: u32 = 0x0000_0020;
/// ignore mask 不因 modify 事件被清除。
const FAN_MARK_IGNORED_SURV_MODIFY: u32 = 0x0000_0040;
/// 清空指定类型的所有 mark。
const FAN_MARK_FLUSH: u32 = 0x0000_0080;
/// 以 filesystem 为粒度建立 mark。
const FAN_MARK_FILESYSTEM: u32 = 0x0000_0100;
/// 允许 inode mark 被回收；当前仅做参数兼容。
const FAN_MARK_EVICTABLE: u32 = 0x0000_0200;
/// 新式 ignore mask。
const FAN_MARK_IGNORE: u32 = 0x0000_0400;
/// mark 动作位集合。
const FAN_MARK_ACTIONS: u32 = FAN_MARK_ADD | FAN_MARK_REMOVE | FAN_MARK_FLUSH;
/// mark 类型位集合。`FAN_MARK_INODE` 的值为 0，不占位。
const FAN_MARK_TYPES: u32 = FAN_MARK_MOUNT | FAN_MARK_FILESYSTEM;
/// 当前接受的 fanotify mark flags。
const FANOTIFY_MARK_SUPPORTED_FLAGS: u32 = FAN_MARK_ACTIONS
    | FAN_MARK_DONT_FOLLOW
    | FAN_MARK_ONLYDIR
    | FAN_MARK_MOUNT
    | FAN_MARK_IGNORED_MASK
    | FAN_MARK_IGNORED_SURV_MODIFY
    | FAN_MARK_FILESYSTEM
    | FAN_MARK_EVICTABLE
    | FAN_MARK_IGNORE;

const FAN_ACCESS: u64 = 0x0000_0001;
const FAN_MODIFY: u64 = 0x0000_0002;
const FAN_ATTRIB: u64 = 0x0000_0004;
const FAN_CLOSE_WRITE: u64 = 0x0000_0008;
const FAN_CLOSE_NOWRITE: u64 = 0x0000_0010;
const FAN_OPEN: u64 = 0x0000_0020;
const FAN_MOVED_FROM: u64 = 0x0000_0040;
const FAN_MOVED_TO: u64 = 0x0000_0080;
const FAN_CREATE: u64 = 0x0000_0100;
const FAN_DELETE: u64 = 0x0000_0200;
const FAN_DELETE_SELF: u64 = 0x0000_0400;
const FAN_MOVE_SELF: u64 = 0x0000_0800;
const FAN_OPEN_EXEC: u64 = 0x0000_1000;
const FAN_OPEN_PERM: u64 = 0x0001_0000;
const FAN_ACCESS_PERM: u64 = 0x0002_0000;
const FAN_OPEN_EXEC_PERM: u64 = 0x0004_0000;
const FAN_EVENT_ON_CHILD: u64 = 0x0800_0000;
const FAN_RENAME: u64 = 0x1000_0000;
const FAN_ONDIR: u64 = 0x4000_0000;
const FAN_PERMISSION_EVENTS: u64 = FAN_OPEN_PERM | FAN_ACCESS_PERM | FAN_OPEN_EXEC_PERM;
const FANOTIFY_MARK_SUPPORTED_MASK: u64 = FAN_ACCESS
    | FAN_MODIFY
    | FAN_ATTRIB
    | FAN_CLOSE_WRITE
    | FAN_CLOSE_NOWRITE
    | FAN_OPEN
    | FAN_MOVED_FROM
    | FAN_MOVED_TO
    | FAN_CREATE
    | FAN_DELETE
    | FAN_DELETE_SELF
    | FAN_MOVE_SELF
    | FAN_OPEN_EXEC
    | FAN_PERMISSION_EVENTS
    | FAN_EVENT_ON_CHILD
    | FAN_RENAME
    | FAN_ONDIR;
const AT_FDCWD: i32 = -100;

fn stat_file_type(mode: u32) -> u32 {
    mode & 0o170000
}

fn fanotify_mark_type(flags: u32) -> Result<u32, SysErrNo> {
    let ty = flags & FAN_MARK_TYPES;
    match ty {
        0 | FAN_MARK_MOUNT | FAN_MARK_FILESYSTEM => Ok(ty),
        _ => Err(SysErrNo::EINVAL),
    }
}

fn validate_fanotify_mark_flags(flags: u32) -> Result<u32, SysErrNo> {
    if flags & !FANOTIFY_MARK_SUPPORTED_FLAGS != 0 {
        return Err(SysErrNo::EINVAL);
    }

    if (flags & FAN_MARK_ACTIONS).count_ones() != 1 {
        return Err(SysErrNo::EINVAL);
    }

    if flags & FAN_MARK_IGNORE != 0 && flags & FAN_MARK_IGNORED_MASK != 0 {
        return Err(SysErrNo::EINVAL);
    }

    let mark_type = fanotify_mark_type(flags)?;
    if mark_type != 0 && flags & FAN_MARK_IGNORE != 0 && flags & FAN_MARK_IGNORED_SURV_MODIFY == 0 {
        return Err(SysErrNo::EINVAL);
    }

    Ok(mark_type)
}

fn validate_fanotify_mark_mask(flags: u32, mask: u64) -> Result<(), SysErrNo> {
    if mask == 0 {
        return Err(SysErrNo::EINVAL);
    }
    if mask & !FANOTIFY_MARK_SUPPORTED_MASK != 0 {
        return Err(SysErrNo::EINVAL);
    }
    if mask & FAN_PERMISSION_EVENTS != 0 {
        return Err(SysErrNo::EINVAL);
    }
    if flags & FAN_MARK_IGNORED_MASK == 0 && flags & FAN_MARK_IGNORE == 0 {
        let event_only_flags = FAN_EVENT_ON_CHILD | FAN_ONDIR;
        if mask & !event_only_flags == 0 {
            return Err(SysErrNo::EINVAL);
        }
    }
    Ok(())
}

fn resolve_fanotify_mark_path(dirfd: i32, pathname: *const u8) -> Result<String, SysErrNo> {
    if pathname.is_null() {
        return Err(SysErrNo::EFAULT);
    }
    if dirfd < 0 && dirfd != AT_FDCWD {
        return Err(SysErrNo::EBADF);
    }

    let task = current_task().unwrap();
    let proc_inner = &task.process;
    let memory_set = proc_inner.memory_set_arc();
    let path = read_user_cstr(&memory_set, pathname)?;
    proc_inner.get_abs_path(dirfd as isize, &path)
}

fn check_fanotify_mark_target(flags: u32, abs_path: &str) -> Result<(), SysErrNo> {
    let mut open_flags = OpenFlags::O_RDONLY;
    if flags & FAN_MARK_ONLYDIR != 0 {
        open_flags |= OpenFlags::O_DIRECTORY;
    }
    if flags & FAN_MARK_DONT_FOLLOW != 0 {
        open_flags |= OpenFlags::O_NOFOLLOW;
    }

    let suppress = suppress_fanotify_events();
    let file = open(abs_path, open_flags, NONE_MODE)?.any();
    let st_mode = file.fstat().st_mode;
    drop(file);
    drop(suppress);
    if flags & FAN_MARK_ONLYDIR != 0 && stat_file_type(st_mode) != StMode::FDIR.bits() {
        return Err(SysErrNo::ENOTDIR);
    }
    Ok(())
}

/// 创建 fanotify notification group。
///
/// 当前实现覆盖 `fanotify_init(2)` 的 fd 创建和参数校验：
/// - 校验 init flags、class bits 和 `FAN_REPORT_*` 依赖关系；
/// - 校验 `event_f_flags` 的访问模式和受支持附加 flags；
/// - 将 `FAN_CLOEXEC/FAN_NONBLOCK` 转换为 fd 表中的 `OpenFlags`；
/// - 返回一个独立的 `FanotifyFd`。
///
/// 注意：完整 fanotify 还需要权限事件响应、完整 FID 信息记录和更精确的
/// mount/filesystem 传播语义；这些不在本函数内完成。
///
/// 参考 https://www.man7.org/linux/man-pages/man2/fanotify_init.2.html
pub fn sys_fanotify_init(flags: u32, event_f_flags: u32) -> SyscallRet {
    if flags & !FANOTIFY_INIT_SUPPORTED_FLAGS != 0 {
        return Err(SysErrNo::EINVAL);
    }

    match flags & FAN_CLASS_BITS {
        FAN_CLASS_NOTIF | FAN_CLASS_CONTENT | FAN_CLASS_PRE_CONTENT => {}
        _ => return Err(SysErrNo::EINVAL),
    }

    if flags & FAN_REPORT_NAME != 0 && flags & FAN_REPORT_DIR_FID == 0 {
        return Err(SysErrNo::EINVAL);
    }
    if flags & FAN_REPORT_TARGET_FID != 0
        && (flags & (FAN_REPORT_FID | FAN_REPORT_DIR_FID | FAN_REPORT_NAME))
            != (FAN_REPORT_FID | FAN_REPORT_DIR_FID | FAN_REPORT_NAME)
    {
        return Err(SysErrNo::EINVAL);
    }

    if event_f_flags & !FANOTIFY_EVENT_F_FLAGS_SUPPORTED != 0 {
        return Err(SysErrNo::EINVAL);
    }
    match event_f_flags & OpenFlags::O_ACCMODE.bits() {
        bits if bits == OpenFlags::O_RDONLY.bits()
            || bits == OpenFlags::O_WRONLY.bits()
            || bits == OpenFlags::O_RDWR.bits() => {}
        _ => return Err(SysErrNo::EINVAL),
    }

    let fanotify_file = FanotifyFd::new(flags, event_f_flags, flags & FAN_NONBLOCK != 0);
    let mut open_flags = OpenFlags::O_RDONLY;
    if flags & FAN_CLOEXEC != 0 {
        open_flags |= OpenFlags::O_CLOEXEC;
    }
    if flags & FAN_NONBLOCK != 0 {
        open_flags |= OpenFlags::O_NONBLOCK;
    }

    let task = current_task().unwrap();
    let proc_inner = &task.process;
    let fd = proc_inner.fd_table.alloc_fd()?;
    proc_inner.fd_table.set(
        fd,
        FileDescriptor::new(open_flags, FileClass::Abs(fanotify_file.clone())),
    )?;
    FanotifyFd::register_fd(fd, &fanotify_file);
    Ok(fd)
}

/// 修改 fanotify notification group 的 mark 集合。
///
/// 当前实现覆盖 `fanotify_mark(2)` 的 fd 查找、flag/mask 校验、路径解析、
/// 目标存在性检查，以及普通 mark / ignore mark 的 add、remove、flush 管理。
/// 配合 `FanotifyFd` 和 `OSFile` hook，当前已经支持 LTP `fanotify01` 所需的
/// open/access/modify/close 基础通知事件。
///
/// 参考 https://man7.org/linux/man-pages/man2/fanotify_mark.2.html
pub fn sys_fanotify_mark(
    fanotify_fd: i32,
    flags: u32,
    mask: u64,
    dirfd: i32,
    pathname: *const u8,
) -> SyscallRet {
    if fanotify_fd < 0 {
        return Err(SysErrNo::EBADF);
    }

    let fanotify = FanotifyFd::lookup(fanotify_fd as usize)?;
    let mark_type = validate_fanotify_mark_flags(flags)?;

    if flags & FAN_MARK_FLUSH != 0 {
        if mask != 0 {
            return Err(SysErrNo::EINVAL);
        }
        return fanotify.flush_marks(mark_type);
    }

    validate_fanotify_mark_mask(flags, mask)?;
    let abs_path = resolve_fanotify_mark_path(dirfd, pathname)?;
    check_fanotify_mark_target(flags, &abs_path)?;

    let ignored = flags & FAN_MARK_IGNORED_MASK != 0 || flags & FAN_MARK_IGNORE != 0;
    if flags & FAN_MARK_ADD != 0 {
        fanotify.add_mark(
            mark_type,
            abs_path,
            mask,
            ignored,
            flags & FAN_MARK_IGNORED_SURV_MODIFY != 0,
        )
    } else {
        fanotify.remove_mark(mark_type, abs_path, mask, ignored)
    }
}
