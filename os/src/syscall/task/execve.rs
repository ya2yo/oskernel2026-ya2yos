use alloc::{
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use log::debug;

use crate::{
    arch::memory_layout::{PAGE_SIZE, USER_STACK_SIZE},
    fs::{open, Inode, OSFile, OpenFlags, MAX_PATH_LEN, NONE_MODE},
    mm::{
        copy_from_user_val, read_elf_load_image, read_elf_load_image_with_prefix, read_user_cstr,
        read_user_cstr_with_limit, MemorySet,
    },
    syscall::FaccessatFileMode,
    task::current_task,
    utils::{get_abs_path, strip_color, trim_start_slash, SysErrNo, SyscallRet},
};

const EXEC_PROBE_SIZE: usize = 256;
const MAX_EXEC_ARG_STRLEN: usize = 32 * PAGE_SIZE;
const MAX_EXEC_ARG_BYTES: usize = USER_STACK_SIZE / 4;
const EXEC_ARG_FIXED_STACK_BYTES: usize = 3 * core::mem::size_of::<usize>();
const MAX_EXEC_ARG_COUNT: usize = MAX_EXEC_ARG_BYTES / (core::mem::size_of::<usize>() + 1);

fn read_exec_argument(
    memory_set: &MemorySet,
    ptr: usize,
    total_bytes: &mut usize,
    total_count: &mut usize,
) -> Result<Vec<u8>, SysErrNo> {
    if *total_count >= MAX_EXEC_ARG_COUNT {
        return Err(SysErrNo::E2BIG);
    }

    let remaining = MAX_EXEC_ARG_BYTES
        .checked_sub(*total_bytes)
        .ok_or(SysErrNo::E2BIG)?;
    let max_string_len = remaining
        .checked_sub(core::mem::size_of::<usize>())
        .ok_or(SysErrNo::E2BIG)?;
    if max_string_len == 0 {
        return Err(SysErrNo::E2BIG);
    }

    let value = read_user_cstr_with_limit(
        memory_set,
        ptr as *const u8,
        max_string_len.min(MAX_EXEC_ARG_STRLEN),
    )?;
    let value_bytes = value.len().checked_add(1).ok_or(SysErrNo::E2BIG)?;
    *total_bytes = total_bytes
        .checked_add(value_bytes)
        .and_then(|bytes| bytes.checked_add(core::mem::size_of::<usize>()))
        .filter(|bytes| *bytes <= MAX_EXEC_ARG_BYTES)
        .ok_or(SysErrNo::E2BIG)?;
    *total_count = total_count.checked_add(1).ok_or(SysErrNo::E2BIG)?;
    Ok(value)
}

fn next_user_pointer(ptr: *const usize) -> Result<*const usize, SysErrNo> {
    (ptr as usize)
        .checked_add(core::mem::size_of::<usize>())
        .map(|next| next as *const usize)
        .ok_or(SysErrNo::EFAULT)
}

fn validate_exec_argument_budget(argv: &[Vec<u8>], env: &[Vec<u8>]) -> Result<(), SysErrNo> {
    let count = argv.len().checked_add(env.len()).ok_or(SysErrNo::E2BIG)?;
    if count > MAX_EXEC_ARG_COUNT {
        return Err(SysErrNo::E2BIG);
    }

    let mut total_bytes: usize = 0;
    for value in argv.iter().chain(env.iter()) {
        total_bytes = total_bytes
            .checked_add(value.len().checked_add(1).ok_or(SysErrNo::E2BIG)?)
            .ok_or(SysErrNo::E2BIG)?;
    }

    // The initial stack also holds argc and both NULL-terminated pointer arrays.
    let pointer_bytes = count
        .checked_add(3)
        .and_then(|count| count.checked_mul(core::mem::size_of::<usize>()))
        .ok_or(SysErrNo::E2BIG)?;
    total_bytes = total_bytes
        .checked_add(pointer_bytes)
        .ok_or(SysErrNo::E2BIG)?;
    if total_bytes > MAX_EXEC_ARG_BYTES {
        return Err(SysErrNo::E2BIG);
    }
    Ok(())
}

fn is_elf(data: &[u8]) -> bool {
    data.len() >= 4 && data[0] == 0x7F && data[1] == b'E' && data[2] == b'L' && data[3] == b'F'
}

fn read_exec_probe_with_size(
    inode: &Arc<dyn Inode>,
    file_size: usize,
) -> Result<Vec<u8>, SysErrNo> {
    let read_len = EXEC_PROBE_SIZE.min(file_size);
    let mut data = alloc::vec![0u8; read_len];
    let mut done = 0;
    while done < read_len {
        let read = inode.read_at(done, &mut data[done..])?;
        if read == 0 {
            break;
        }
        done += read;
    }
    data.truncate(done);
    Ok(data)
}

fn current_task_can_exec(file_mode: u32, owner_uid: u32, owner_gid: u32) -> bool {
    let task = current_task().unwrap();
    let task_inner = task.inner_lock();
    let file_mode = FaccessatFileMode::from_bits_truncate(file_mode & 0xfff);

    if task_inner.effective_uid == 0 {
        return file_mode.intersects(
            FaccessatFileMode::S_IXUSR | FaccessatFileMode::S_IXGRP | FaccessatFileMode::S_IXOTH,
        );
    }

    if task_inner.effective_uid == owner_uid {
        file_mode.contains(FaccessatFileMode::S_IXUSR)
    } else if task_inner.effective_gid == owner_gid {
        file_mode.contains(FaccessatFileMode::S_IXGRP)
    } else {
        file_mode.contains(FaccessatFileMode::S_IXOTH)
    }
}

fn check_exec_permission(file_mode: u32, owner_uid: u32, owner_gid: u32) -> SyscallRet {
    if current_task_can_exec(file_mode, owner_uid, owner_gid) {
        Ok(0)
    } else {
        Err(SysErrNo::EACCES)
    }
}

fn check_not_write_open(path: &str) -> SyscallRet {
    if OSFile::is_write_open_path(path) {
        Err(SysErrNo::ETXTBSY)
    } else {
        Ok(0)
    }
}

/// 从文件头解析 shebang（`#!` 行），语义对齐 Linux `fs/binfmt_script.c`。
///
/// 脚本第一行格式：`#!<解释器路径>[ <可选参数>]\n`
/// 例如 LTP access02 的 `file_x` 内容为 `#!/bin/sh\n`。
///
/// 返回值 `(解释器路径, 可选参数)`，例如：
/// - 文件内容为 `#!/bin/sh\n` → `("/bin/sh", None)`
/// - 文件内容为 `#!/usr/bin/env python3 -u\n` → `("/usr/bin/env", Some("python3 -u"))`
///
/// 只读第一行（到 `\n` 或 `\r` 为止），整行最多受内核读文件限制约束。
fn parse_shebang(data: &[u8]) -> Option<(String, Option<String>)> {
    // 必须以 #! 开头，否则不是脚本
    if data.len() < 2 || data[0] != b'#' || data[1] != b'!' {
        return None;
    }
    let rest = &data[2..];
    // shebang 行在第一个换行处结束；若无换行则读到 buffer 末尾
    let line_len = rest
        .iter()
        .position(|&b| b == b'\n' || b == b'\r')
        .unwrap_or(rest.len());
    let line = core::str::from_utf8(&rest[..line_len]).ok()?.trim();
    if line.is_empty() {
        return None;
    }
    // 解释器路径与可选参数以空白分隔，仅支持一个可选参数（与 Linux 一致）
    let (interp, arg) = match line.find(char::is_whitespace) {
        Some(idx) => {
            let interp = line[..idx].trim();
            let arg = line[idx..].trim();
            if interp.is_empty() {
                return None;
            }
            (
                interp.to_string(),
                if arg.is_empty() {
                    None
                } else {
                    Some(arg.to_string())
                },
            )
        }
        None => (line.to_string(), None),
    };
    Some((interp, arg))
}

/// 参考 https://man7.org/linux/man-pages/man2/execve.2.html
pub fn sys_execve(path: *const u8, mut argv: *const usize, mut envp: *const usize) -> SyscallRet {
    let task = current_task().unwrap();
    let proc = &task.process;

    let memory_set = proc.memory_set_arc();
    let mut path = trim_start_slash(read_user_cstr(&memory_set, path)?);
    if path.starts_with("ltp/testcases/bin/\u{1b}[1;32m") {
        //去除颜色
        path = strip_color(path, "ltp/testcases/bin/\u{1b}[1;32m", "\u{1b}[m");
    }
    //log::info!("[sys_execve] path={}", path);

    // path.len() >= MAX_PATH_LEN(256): read_user_cstr 的缓冲区是 256 字节，
    // 若用户传来的路径不含 '\0' 且超过 256 字节，会被截断并返回 256 字节串，
    // 此时截断后的路径落到底层 open() 会错误返回 ENOENT，应提前返回 ENAMETOOLONG。
    // path.split('/').any(|c| c.len() > 255): 单个路径分量超过 NAME_MAX(255)，
    // 同样应返回 ENAMETOOLONG 而非让底层文件系统返回 ENOENT。
    if path.len() >= MAX_PATH_LEN || path.split('/').any(|c| c.len() > 255) {
        return Err(SysErrNo::ENAMETOOLONG);
    }

    //处理argv参数
    let mut argv_vec = Vec::<Vec<u8>>::new();
    let mut exec_arg_bytes = EXEC_ARG_FIXED_STACK_BYTES;
    let mut exec_arg_count = 0;
    loop {
        if argv.is_null() {
            break;
        }
        let argv_ptr = copy_from_user_val(&memory_set, argv)?;
        if argv_ptr == 0 {
            break;
        }
        // `argv[0]` 由调用者定义，不能用待执行文件路径覆盖。
        argv_vec.try_reserve(1).map_err(|_| SysErrNo::ENOMEM)?;
        argv_vec.push(read_exec_argument(
            &memory_set,
            argv_ptr,
            &mut exec_arg_bytes,
            &mut exec_arg_count,
        )?);
        argv = next_user_pointer(argv)?;
    }
    if argv_vec.is_empty() {
        argv_vec.push(Vec::new());
    }

    // 这个还得留着，因为busybox真的会试图exec这样的文件
    // 以后也许可以改成检测Shebang
    if path.ends_with(".sh") {
        //.sh文件不是可执行文件，需要用busybox的sh来启动
        argv_vec.try_reserve(2).map_err(|_| SysErrNo::ENOMEM)?;
        argv_vec.insert(0, b"sh".to_vec());
        argv_vec.insert(0, b"busybox".to_vec());
        path = String::from("/musl/busybox");
    }

    // if path.ends_with("ls") || path.ends_with("xargs") || path.ends_with("sleep") {
    //     //ls,xargs,sleep文件为busybox调用，需要用busybox来启动
    //     argv_vec.insert(0, String::from("busybox"));
    //     path = String::from("/musl/busybox");
    // }

    debug!("[sys_execve] path is {},arg is {:?}", path, argv_vec);
    let mut env = Vec::<Vec<u8>>::new();
    // 处理运行环境，如果为空，加载默认的运行环境
    if envp.is_null() {
        // debug!("use default env");
        env.push(
            b"PATH=/musl/ltp/testcases/bin:/glibc/ltp/testcases/bin:/bin:/sbin:/usr/bin:/usr/sbin:."
                .to_vec(),
        );
        env.push(b"TMPDIR=/tmp".to_vec());
        env.push(b"RHOST=127.0.0.1".to_vec());
        env.push(b"LHOST_HWADDRS=00:00:00:00:00:00".to_vec());
        env.push(b"RHOST_HWADDRS=00:00:00:00:00:00".to_vec());
        env.push(b"NS_DURATION=1".to_vec());
        env.push(b"IP_TOTAL_FOR_TCPIP=0".to_vec());
        // env.push("LD_LIBRARY_PATH=/musl/lib:".to_string());
        // env.push("LD_LIBRARY_PATH=/glibc/lib:/musl/lib".to_string());
        //设置系统最大负载
        env.push(b"ENOUGH=100000".to_vec());
        // 尝试强制设置环境变量满足clocale
        env.push(b"LANG=C".to_vec());
        env.push(b"LC_CTYPE=C".to_vec());
        env.push(b"TERM=xterm".to_vec());
        env.push(b"HOME=/root".to_vec());
        env.push(b"SHELL=/bin/sh".to_vec());
        env.push(b"USER=root".to_vec());
    } else {
        // debug!("use assigned env");
        loop {
            let envp_ptr = copy_from_user_val(&memory_set, envp)?;
            if envp_ptr == 0 {
                break;
            }
            env.try_reserve(1).map_err(|_| SysErrNo::ENOMEM)?;
            env.push(read_exec_argument(
                &memory_set,
                envp_ptr,
                &mut exec_arg_bytes,
                &mut exec_arg_count,
            )?);
            envp = next_user_pointer(envp)?;
        }
    }

    // debug!("[sys_execve] env is {:?}", env);

    let fs_info = &proc.fs_info;
    let cwd = fs_info.get_cwd();
    let exe = fs_info.get_exe();
    let mut abs_path = get_abs_path(&cwd, &path);
    // 如果是/proc/self/exe，特殊处理
    if abs_path == "/proc/self/exe" {
        abs_path = exe.clone().into();
        if argv_vec[0].as_slice() == b"/proc/self/exe" {
            argv_vec[0] = exe.clone().into_bytes();
        }
    }
    // debug!("The real abs_path is {}", abs_path);
    let script_abs_path = abs_path.clone();
    let app_inode = open(&abs_path, OpenFlags::O_RDONLY, NONE_MODE)?.file()?;
    let app_stat = app_inode.inode.fstat();
    // `fstat()` already provides the permission bits.  Calling `fmode()` here
    // would reacquire the serialized EXT4 operation lock for identical data.
    check_exec_permission(app_stat.st_mode, app_stat.st_uid, app_stat.st_gid)?;
    check_not_write_open(&app_inode.inode.path())?;

    let app_size = app_stat.st_size.max(0) as usize;
    let mut elf_data = read_exec_probe_with_size(&app_inode.inode, app_size)?;
    if is_elf(&elf_data) {
        elf_data = read_elf_load_image_with_prefix(&app_inode.inode, &elf_data, app_size)?;
    } else {
        // 非 ELF：尝试按 shebang 脚本处理（如 #!/bin/sh）。
        // Linux 内核不会把脚本当最终可执行体，而是转去 exec 解释器。
        if let Some((interp, shebang_arg)) = parse_shebang(&elf_data) {
            // 重建 argv，与 Linux binfmt_script 一致：
            //   execve("./file_x", ["./file_x"], env)
            // → execve("/bin/sh", ["/bin/sh", "/abs/path/file_x"], env)
            // 若有 shebang 可选参数，插在解释器与脚本路径之间：
            //   #!/usr/bin/env python3 → ["/usr/bin/env", "python3", "/abs/script", ...]
            let mut new_argv = Vec::new();
            new_argv
                .try_reserve(argv_vec.len().checked_add(2).ok_or(SysErrNo::E2BIG)?)
                .map_err(|_| SysErrNo::ENOMEM)?;
            new_argv.push(interp.clone().into_bytes()); // argv[0]：shebang 行里的解释器字符串
            if let Some(arg) = shebang_arg {
                new_argv.push(arg.into_bytes());
            }
            new_argv.push(script_abs_path.into_bytes()); // 脚本绝对路径，供解释器读取
            for arg in argv_vec.iter().skip(1) {
                // 保留用户传入的额外参数（原 argv[1..]）
                new_argv.push(arg.clone());
            }
            argv_vec = new_argv;
            // 打开解释器 ELF（如 /bin/sh → busybox），后续走正常 ELF 加载。
            // 竞赛镜像中的 /bin/sh 由启动时兼容文件生成，直接归一到 busybox
            // 可避免旧脚本解释器路径落到非 ELF wrapper。
            abs_path = match interp.as_str() {
                "/bin/sh" | "bin/sh" | "/bin/busybox" | "bin/busybox" => {
                    String::from("/musl/busybox")
                }
                _ => get_abs_path(&cwd, trim_start_slash(interp.clone()).as_str()),
            };
            let interp_inode = open(&abs_path, OpenFlags::O_RDONLY, NONE_MODE)?.file()?;
            let interp_stat = interp_inode.inode.fstat();
            check_exec_permission(interp_stat.st_mode, interp_stat.st_uid, interp_stat.st_gid)?;
            check_not_write_open(&interp_inode.inode.path())?;
            let interp_size = interp_stat.st_size.max(0) as usize;
            elf_data = read_exec_probe_with_size(&interp_inode.inode, interp_size)?;
            if is_elf(&elf_data) {
                elf_data =
                    read_elf_load_image_with_prefix(&interp_inode.inode, &elf_data, interp_size)?;
            } else {
                return Err(SysErrNo::ENOEXEC);
            }
        } else {
            // 既非 ELF 也无 shebang（如纯文本），与 Linux 一样返回 ENOEXEC
            return Err(SysErrNo::ENOEXEC);
        }
    }
    validate_exec_argument_budget(&argv_vec, &env)?;
    fs_info.set_exe(abs_path);
    drop(memory_set);

    // 不用切换页表，因为return_to_user会切换
    task.exec(&elf_data, &argv_vec, &env)?;
    Ok(0)
}
