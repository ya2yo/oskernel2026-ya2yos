use alloc::{
    string::{String, ToString},
    vec::Vec,
};
use log::debug;

use crate::{
    fs::{open, OpenFlags, NONE_MODE},
    mm::{translated_ref, translated_str},
    task::current_task,
    utils::{get_abs_path, strip_color, trim_start_slash, SysErrNo, SyscallRet},
};

fn is_elf(data: &[u8]) -> bool {
    data.len() >= 4
        && data[0] == 0x7F
        && data[1] == b'E'
        && data[2] == b'L'
        && data[3] == b'F'
}

/// Parse `#!` interpreter line per Linux binfmt_script (one optional argument).
fn parse_shebang(data: &[u8]) -> Option<(String, Option<String>)> {
    if data.len() < 2 || data[0] != b'#' || data[1] != b'!' {
        return None;
    }
    let rest = &data[2..];
    let line_len = rest
        .iter()
        .position(|&b| b == b'\n' || b == b'\r')
        .unwrap_or(rest.len());
    let line = core::str::from_utf8(&rest[..line_len]).ok()?.trim();
    if line.is_empty() {
        return None;
    }
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

    let token = proc_inner.get_locked_memory_set_read().token();
    let mut path = trim_start_slash(translated_str(token, path));
    if path.starts_with("ltp/testcases/bin/\u{1b}[1;32m") {
        //去除颜色
        path = strip_color(path, "ltp/testcases/bin/\u{1b}[1;32m", "\u{1b}[m");
    }
    //log::info!("[sys_execve] path={}", path);

    //处理argv参数
    let mut argv_vec = Vec::<String>::new();
    if !argv.is_null() {
        let argv_ptr = *translated_ref(token, argv);
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
        let argv_ptr = *translated_ref(token, argv);
        if argv_ptr == 0 {
            break;
        }
        argv_vec.push(translated_str(token, argv_ptr as *const u8));
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
        env.push("PATH=/bin".to_string());
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
            let envp_ptr = *translated_ref(token, envp);
            if envp_ptr == 0 {
                break;
            }
            env.push(translated_str(token, envp_ptr as *const u8));
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

    let mut elf_data = app_inode.inode.read_all()?;
    if !is_elf(&elf_data) {
        if let Some((interp, shebang_arg)) = parse_shebang(&elf_data) {
            let mut new_argv = Vec::new();
            new_argv.push(interp.clone());
            if let Some(arg) = shebang_arg {
                new_argv.push(arg);
            }
            new_argv.push(script_abs_path);
            for arg in argv_vec.iter().skip(1) {
                new_argv.push(arg.clone());
            }
            argv_vec = new_argv;
            abs_path = get_abs_path(&cwd, trim_start_slash(interp).as_str());
            let interp_inode = open(&abs_path, OpenFlags::O_RDONLY, NONE_MODE)?.file()?;
            elf_data = interp_inode.inode.read_all()?;
            if !is_elf(&elf_data) {
                return Err(SysErrNo::ENOEXEC);
            }
        } else {
            return Err(SysErrNo::ENOEXEC);
        }
    }
    locked_fs_info.set_exe(abs_path);
    drop(proc_inner);

    task.exec(&elf_data, &argv_vec, &mut env);
    // 不用切换页表，因为return_to_user会切换
    Ok(0)
}
