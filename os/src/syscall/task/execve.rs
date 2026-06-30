use alloc::{
    string::{String, ToString},
    vec::Vec,
};
use log::debug;

use crate::{
    fs::{open, OpenFlags, NONE_MODE},
    mm::{copy_from_user, read_user_cstr},
    syscall::FaccessatFileMode,
    task::current_task,
    utils::{get_abs_path, strip_color, trim_start_slash, SysErrNo, SyscallRet},
};

fn is_elf(data: &[u8]) -> bool {
    data.len() >= 4 && data[0] == 0x7F && data[1] == b'E' && data[2] == b'L' && data[3] == b'F'
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
    let proc_inner = task.process.inner_lock();

    let memory_set = proc_inner.get_locked_memory_set_read();
    let mut path = trim_start_slash(read_user_cstr(&memory_set, path)?);
    if path.starts_with("ltp/testcases/bin/\u{1b}[1;32m") {
        //去除颜色
        path = strip_color(path, "ltp/testcases/bin/\u{1b}[1;32m", "\u{1b}[m");
    }
    //log::info!("[sys_execve] path={}", path);

    //处理argv参数
    let mut argv_vec = Vec::<String>::new();
    if !argv.is_null() {
        let mut argv_ptr: usize = 0;
        copy_from_user(&memory_set, argv as usize, unsafe {
            core::slice::from_raw_parts_mut(
                &mut argv_ptr as *mut usize as *mut u8,
                core::mem::size_of::<usize>(),
            )
        })?;
        if argv_ptr != 0 {
            argv_vec.push(path.clone());
            unsafe {
                argv = argv.add(1);
            }
        }
    }
    loop {
        if argv.is_null() {
            break;
        }
        let mut argv_ptr: usize = 0;
        copy_from_user(&memory_set, argv as usize, unsafe {
            core::slice::from_raw_parts_mut(
                &mut argv_ptr as *mut usize as *mut u8,
                core::mem::size_of::<usize>(),
            )
        })?;
        if argv_ptr == 0 {
            break;
        }
        argv_vec.push(read_user_cstr(&memory_set, argv_ptr as *const u8)?);
        unsafe {
            argv = argv.add(1);
        }
    }

    // 这个还得留着，因为busybox真的会试图exec这样的文件
    // 以后也许可以改成检测Shebang
    if path.ends_with(".sh") {
        //.sh文件不是可执行文件，需要用busybox的sh来启动
        argv_vec.insert(0, String::from("sh"));
        argv_vec.insert(0, String::from("busybox"));
        path = String::from("/musl/busybox");
    }

    // if path.ends_with("ls") || path.ends_with("xargs") || path.ends_with("sleep") {
    //     //ls,xargs,sleep文件为busybox调用，需要用busybox来启动
    //     argv_vec.insert(0, String::from("busybox"));
    //     path = String::from("/musl/busybox");
    // }

    debug!("[sys_execve] path is {},arg is {:?}", path, argv_vec);
    let mut env = Vec::<String>::new();

    if envp.is_null() {
        // debug!("use default env");
        env.push("PATH=/bin:.".to_string());
        // env.push("LD_LIBRARY_PATH=/musl/lib:".to_string());
        // env.push("LD_LIBRARY_PATH=/glibc/lib:/musl/lib".to_string());
        //设置系统最大负载
        env.push("ENOUGH=100000".to_string());
        // 尝试强制设置环境变量满足clocale
        env.push("LANG=C".to_string());
        env.push("LC_CTYPE=C".to_string());
    } else {
        // debug!("use assigned env");
        loop {
            let mut envp_ptr: usize = 0;
            copy_from_user(&memory_set, envp as usize, unsafe {
                core::slice::from_raw_parts_mut(
                    &mut envp_ptr as *mut usize as *mut u8,
                    core::mem::size_of::<usize>(),
                )
            })?;
            if envp_ptr == 0 {
                break;
            }
            env.push(read_user_cstr(&memory_set, envp_ptr as *const u8)?);
            unsafe {
                envp = envp.add(1);
            }
        }
    }

    // debug!("[sys_execve] env is {:?}", env);

    let locked_fs_info = &proc_inner.fs_info;
    let cwd = locked_fs_info.get_cwd();
    let exe = locked_fs_info.get_exe();
    let mut abs_path = get_abs_path(&cwd, &path);
    // HXC:
    // 如果是/proc/self/exe，特殊处理
    // 这个实现有点将就，只会重构文件系统的时候再想想怎么处理吧
    if abs_path == "/proc/self/exe" {
        abs_path = exe.clone().into();
        if argv_vec[0] == "/proc/self/exe" {
            argv_vec[0] = exe.into();
        }
    }
    // debug!("The real abs_path is {}", abs_path);
    let script_abs_path = abs_path.clone();
    let app_inode = open(&abs_path, OpenFlags::O_RDONLY, NONE_MODE)?.file()?;
    let app_stat = app_inode.inode.fstat();
    check_exec_permission(app_inode.inode.fmode()?, app_stat.st_uid, app_stat.st_gid)?;

    let mut elf_data = app_inode.inode.read_all()?;
    if !is_elf(&elf_data) {
        // 非 ELF：尝试按 shebang 脚本处理（如 #!/bin/sh）。
        // Linux 内核不会把脚本当最终可执行体，而是转去 exec 解释器。
        if let Some((interp, shebang_arg)) = parse_shebang(&elf_data) {
            // 重建 argv，与 Linux binfmt_script 一致：
            //   execve("./file_x", ["./file_x"], env)
            // → execve("/bin/sh", ["/bin/sh", "/abs/path/file_x"], env)
            // 若有 shebang 可选参数，插在解释器与脚本路径之间：
            //   #!/usr/bin/env python3 → ["/usr/bin/env", "python3", "/abs/script", ...]
            let mut new_argv = Vec::new();
            new_argv.push(interp.clone()); // argv[0]：shebang 行里的解释器字符串
            if let Some(arg) = shebang_arg {
                new_argv.push(arg);
            }
            new_argv.push(script_abs_path); // 脚本绝对路径，供解释器读取
            for arg in argv_vec.iter().skip(1) {
                // 保留用户传入的额外参数（原 argv[1..]）
                new_argv.push(arg.clone());
            }
            argv_vec = new_argv;
            // 打开解释器 ELF（如 /bin/sh → busybox），后续走正常 ELF 加载
            abs_path = get_abs_path(&cwd, trim_start_slash(interp).as_str());
            let interp_inode = open(&abs_path, OpenFlags::O_RDONLY, NONE_MODE)?.file()?;
            let interp_stat = interp_inode.inode.fstat();
            check_exec_permission(
                interp_inode.inode.fmode()?,
                interp_stat.st_uid,
                interp_stat.st_gid,
            )?;
            elf_data = interp_inode.inode.read_all()?;
            if !is_elf(&elf_data) {
                return Err(SysErrNo::ENOEXEC);
            }
        } else {
            // 既非 ELF 也无 shebang（如纯文本），与 Linux 一样返回 ENOEXEC
            return Err(SysErrNo::ENOEXEC);
        }
    }
    locked_fs_info.set_exe(abs_path);
    drop(memory_set);
    drop(proc_inner);

    // 不用切换页表，因为return_to_user会切换
    task.exec(&elf_data, &argv_vec, &mut env)
        .map_err(|_| SysErrNo::ENOMEM)?;
    Ok(0)
}
