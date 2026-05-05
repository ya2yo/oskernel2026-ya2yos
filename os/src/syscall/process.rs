use crate::{
    fs::{open, OpenFlags, NONE_MODE},
    mm::{
        get_data, if_bad_address, put_data, safe_put_data, translated_ref, translated_str, VirtAddr,
    },
    signal::{check_if_any_sig_for_current_task, handle_signal},
    syscall::{process, CloneFlags, Utsname},
    task::{
        current_task, current_token, exit_current_and_run_next, exit_current_group_and_run_next,
        futex_wake_up, ready_queue, suspend_current_and_run_next, tid_to_task, Process, Processor,
        Sysinfo,
    },
    timer::{calculate_left_timespec, get_time_ms, get_time_spec, Timespec},
    utils::{get_abs_path, strip_color, trim_start_slash, SysErrNo, SyscallRet},
};
use alloc::{
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};

use log::{debug, error, warn};



/// 参考 https://man7.org/linux/man-pages/man2/sched_yield.2.html
pub fn sys_sched_yield() -> SyscallRet {
    suspend_current_and_run_next();
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/getpid.2.html
pub fn sys_getpid() -> SyscallRet {
    Ok(current_task().unwrap().pid())
}

/// 参考 https://man7.org/linux/man-pages/man2/getppid.2.html
pub fn sys_getppid() -> SyscallRet {
    Ok(current_task().unwrap().ppid())
}

/// 参考 https://man7.org/linux/man-pages/man2/getuid.2.html
pub fn sys_getuid() -> SyscallRet {
    let task = current_task().unwrap();
    let task_inner = task.inner_lock();
    Ok(task_inner.user_id)
}

pub fn sys_setuid(uid: usize) -> SyscallRet {
    let task = current_task().unwrap();
    let mut task_inner = task.inner_lock();
    task_inner.user_id = uid;
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/geteuid.2.html
pub fn sys_geteuid() -> SyscallRet {
    Ok(0) // root user
}

/// 参考 https://man7.org/linux/man-pages/man2/getgid.2.html
pub fn sys_getgid() -> SyscallRet {
    Ok(0) // root group
}

/// 参考 https://man7.org/linux/man-pages/man2/getegid.2.html
pub fn sys_getegid() -> SyscallRet {
    Ok(0) // root group
}

/// 参考 https://man7.org/linux/man-pages/man2/gettid.2.html
pub fn sys_gettid() -> SyscallRet {
    Ok(current_task().unwrap().tid())
}

/// 参考 https://man7.org/linux/man-pages/man2/setsid.2.html
pub fn sys_setsid() -> SyscallRet {
    warn!("[sys_setsid] We do not really support process group!");
    Ok(0)
}

pub fn sys_setpgid() -> SyscallRet {
    warn!("[sys_setpgid] We do not really support process group!");
    Ok(0)
}

pub fn sys_getpgid() -> SyscallRet {
    warn!("[sys_getpgid] We do not really support process group!");
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/set_tid_address.2.html
pub fn sys_settidaddress(tidptr: usize) -> SyscallRet {
    current_task().unwrap().inner_lock().clear_child_tid = tidptr;
    sys_gettid()
}

/// 参考 https://man7.org/linux/man-pages/man2/clone.2.html
/// void (*fn)(void* arg) 参数通过栈传递,如果stack_ptr!=0, fn=0(stack),arg=8(stack)
pub fn sys_clone(
    flags: usize,
    stack_ptr: usize,
    parent_tid_ptr: usize,
    #[cfg(target_arch = "loongarch64")] child_tid_ptr: usize,
    tls_ptr: usize,
    #[cfg(not(target_arch = "loongarch64"))] child_tid_ptr: usize,
) -> SyscallRet {
    let flags_val = (flags & !0xff) as u64;
    let clone_flags = match CloneFlags::from_bits(flags_val) {
        Some(f) => f,
        None => return Err(SysErrNo::EINVAL),
    };

    let task = current_task().unwrap();
    
    // 执行克隆
    let new_task = task.do_task_clone(
        clone_flags,
        stack_ptr,
        parent_tid_ptr,
        tls_ptr,
        child_tid_ptr,
    )?;

    let new_tid = new_task.tid();
    
    // 将子任务放入就绪队列，等待调度器执行
    ready_queue::add_task(&new_task);

    // 父进程返回子进程的 TID
    Ok(new_tid)
}

/// 参考 https://man7.org/linux/man-pages/man2/execve.2.html
pub fn sys_execve(path: *const u8, mut argv: *const usize, mut envp: *const usize) -> SyscallRet {
    let task = current_task().unwrap();
    let proc_inner = task.process.inner_lock();

    let token = task.process.inner_lock().get_locked_memory_set_read().token();
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
        debug!("use default env");
        env.push("PATH=/bin".to_string());
        // env.push("LD_LIBRARY_PATH=/musl/lib:".to_string());
        // env.push("LD_LIBRARY_PATH=/glibc/lib:/musl/lib".to_string());
        //设置系统最大负载
        env.push("ENOUGH=100000".to_string());
        // 尝试强制设置环境变量满足clocale
        env.push("LANG=C".to_string());
        env.push("LC_CTYPE=C".to_string());
    } else {
        debug!("use assigned env");
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

    debug!("[sys_execve] env is {:?}", env);

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
    debug!("The real abs_path is {}", abs_path);
    let app_inode = open(&abs_path, OpenFlags::O_RDONLY, NONE_MODE)?.file()?;

    let elf_data = app_inode.inode.read_all()?;
    // 检查一下是不是ELF
    // 读取前4字节
    if elf_data[0] != 0x7F
        || elf_data[1] != ('E' as u8)
        || elf_data[2] != ('L' as u8)
        || elf_data[3] != ('F' as u8)
    {
        // 这就不是ELF!
        return Err(SysErrNo::ENOEXEC); // 这个报错会告诉调用者：这不是ELF
    }
    locked_fs_info.set_exe(abs_path);
    drop(proc_inner);

    task.exec(&elf_data, &argv_vec, &mut env);
    // 不用切换页表，因为return_to_user会切换
    Ok(0)
}

/// input.pid<-1: 等待一个子进程，其pgid==abs(input.pid)。这里的pgid指的是进程组id
/// input.pid=-1: 等待任一一个子进程的结束。
///     这里子进程指的是调用者task所处的进程（线程组）的子进程
///     （由本线程组的task调用sys_clone但选择不把新线程置于本线程组时创建的新线程组）。
/// input.pid=0 : 等待与调用者同进程组的任一子进程的结束
/// input.pid>0 : 等待pid==input.pid的子进程的结束。从内核的视角看，这里的pid指的是线程组id(tgid)，而不是线程id(tid)。
///
/// 参考 https://man7.org/linux/man-pages/man2/wait4.2.html
pub fn sys_wait4(mut pid: isize, wstatus: *mut i32, _options: i32) -> SyscallRet {
    if pid < -1 {
        // 需要进程组功能
        panic!(
            "[sys_wait4] We cannot handle input.pid<-1 (input.pid={})",
            pid
        );
    }
    // 由于我们假设所有进程均属于同一个进程组，我们视pid=0为pid=-1
    if pid == 0 {
        pid = -1;
    }
    // 现在只有两种情况：pid=-1表示等待任意子进程结束，pid>0表示等待特定子进程结束

    // 新实现
    loop {
        debug!("Wait4 loop begin");
        let task = current_task().unwrap();
        let mut process_meta = task.process.meta_lock();
        // 取子进程集合

        let children: Vec<Arc<Process>> = process_meta
            .children
            .clone()
            .iter()
            .filter_map(|x| x.upgrade())
            .collect();
        debug!("Wait4 len={}", children.len());
        if children.len() == 0 {
            return Err(SysErrNo::ECHILD);
        }
        // 如果是等待特定进程，但是自己根本没有这个子进程，则退出
        if pid != -1 && children.iter().all(|proc| proc.pid.0 != pid as usize) {
            return Err(SysErrNo::ECHILD);
        }

        let pair = children
            .iter()
            .enumerate()
            .find(|(_, p)| {
                // ++++ temporarily access child PCB exclusively
                p.all_tasks_exited() && (pid == -1 || pid as usize == p.pid.0)
                // ++++ release child PCB
            })
            .map(|(idx, p)| (idx, Arc::clone(p)));
        drop(children);
        if let Some((idx, child)) = pair {
            let found_pid = child.pid.clone();
            let exit_code = child.inner_lock().get_locked_sigtable().exit_code();

            if wstatus as usize != 0x0 {
                debug!(
                    "[sys_wait4] wait pid {}: child {} exit with code {}, wstatus= {:#x}",
                    pid, found_pid.0, exit_code, wstatus as usize
                );
                let token = task.process.inner_lock().get_locked_memory_set_read().token();
                if exit_code >= 128 && exit_code <= 255 {
                    //表示由于信号而退出的
                    put_data(token, wstatus, exit_code);
                } else {
                    put_data(token, wstatus, exit_code << 8);
                }
            }
            // drop(child_inner);
            process_meta.children.remove(idx);
            // 从全局进程映射中移除
            // 在移除前，我们得先把手上的这个Arc给丢掉
            drop(child);
            Process::remove_from_global_map(found_pid.0);
            return Ok(found_pid.0);
        } else {
            drop(process_meta);
            drop(task);

            debug!("Wait4 suspend");
            suspend_current_and_run_next();
            debug!("Wait4 wakeup");
        }
    }
}

/// 参考 https://man7.org/linux/man-pages/man2/nanosleep.2.html
pub fn sys_nanosleep(req: *const Timespec, rem: *mut Timespec) -> SyscallRet {
    let token = current_token();

    // debug!(
    //     "[sys_nanosleep] req is {:x}, rem is {:x}",
    //     req as usize, rem as usize
    // );

    let req = get_data(token, req);
    let waittime = req.tv_sec * 1_000_000_000usize + req.tv_nsec;
    let begin = get_time_ms() * 1_000_000usize;
    let endtime = get_time_spec() + req;

    // debug!(
    //     "[sys_nanosleep] ready to sleep for {} sec, {} nsec",
    //     req.tv_sec, req.tv_nsec
    // );

    while get_time_ms() * 1_000_000usize - begin < waittime {
        if let Some(_) = check_if_any_sig_for_current_task() {
            //被信号唤醒
            if rem as usize != 0 {
                put_data(token, rem, calculate_left_timespec(endtime));
            }
            return Err(SysErrNo::EINTR);
        }
        suspend_current_and_run_next();
    }
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/uname.2.html
pub fn sys_uname(buf: *mut u8) -> SyscallRet {
    fn str2u8(s: &str) -> [u8; 65] {
        let mut b = [0; 65];
        b[0..s.len()].copy_from_slice(s.as_bytes());
        b
    }
    let uname = Utsname {
        sysname: str2u8("TrustOS"),
        nodename: str2u8("TrustOS"),
        release: str2u8("5.0.0"),
        version: str2u8("5.0.0"),
        machine: str2u8("RISC-V64"),
        domainname: str2u8("TrustOS"),
    };
    let token = current_token();
    put_data(token, buf as *mut Utsname, uname);
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/brk.2.html
pub fn sys_brk(brk_addr: usize) -> SyscallRet {
    let former_addr = current_task().unwrap().growproc(0);
    if brk_addr == 0 {
        return Ok(former_addr);
    }
    let grow_size: isize = (brk_addr - former_addr) as isize;
    Ok(current_task().unwrap().growproc(grow_size))
}

/// 参考 https://man7.org/linux/man-pages/man2/sysinfo.2.html
pub fn sys_sysinfo(info: *const u8) -> SyscallRet {
    let task = current_task().unwrap();
    let token = task.process.inner_lock().get_locked_memory_set_read().token();

    put_data(
        token,
        info as *mut Sysinfo,
        Sysinfo::new(get_time_ms() / 1000, 1 << 56, tid_to_task::task_num()),
    );
    // debug!("[sys_sysinfo] ourinfo is {:?}", ourinfo);
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/umask.2.html
pub fn sys_umask(_mask: u32) -> SyscallRet {
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/syslog.2.html
pub fn sys_syslog(_logtype: isize, _bufp: *const u8, _len: usize) -> SyscallRet {
    // 伪实现
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/sched_setaffinity.2.html
pub fn sys_sched_setaffinity(_pid: usize, _cpusetsize: usize, _mask: usize) -> SyscallRet {
    // debug!(
    //     "[sys_sched_setaffinity] pid is {}, cpusetsize is {}, mask is {}",
    //     pid, cpusetsize, mask
    // );
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/sched_getaffinity.2.html
pub fn sys_sched_getaffinity(_pid: usize, _cpusetsize: usize, _mask: usize) -> SyscallRet {
    // debug!(
    //     "[sys_sched_getaffinity] pid is {}, cpusetsize is {}, mask is {}",
    //     pid, cpusetsize, mask
    // );
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/sched_setscheduler.2.html
pub fn sys_sched_setscheduler(_pid: usize, _policy: usize, _param: *const u8) -> SyscallRet {
    // debug!(
    //     "[sys_sched_setscheduler] pid is {}, policy is {}, param is {:x}",
    //     pid, policy, param as usize
    // );
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/sched_getscheduler.2.html
pub fn sys_sched_getscheduler(_pid: usize) -> SyscallRet {
    // debug!("[sys_sched_getscheduler] pid is {}", pid);
    //由于使用的是标准的时间片调度算法，直接返回SCHED_OHTER = 0
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/sched_getparam.2.html
pub fn sys_sched_getparam(_pid: usize, _param: *const u8) -> SyscallRet {
    // debug!(
    //     "[sys_sched_getparam] pid is {}, param is {:x}",
    //     pid, param as usize
    // );
    //由于使用的是标准的时间片调度算法，param参数需要被忽略
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/clock_nanosleep.2.html
pub fn sys_clock_nanosleep(
    clockid: usize,
    flags: u32,
    t: *const Timespec,
    remain: *mut Timespec,
) -> SyscallRet {
    const TIME_ABSTIME: u32 = 1;
    let task = current_task().unwrap();
    let process = task.process.inner_lock();
    let memory_set = process.get_locked_memory_set_read();

    if clockid != 0 {
        return Err(SysErrNo::EOPNOTSUPP);
    }

    if (t as isize) <= 0 || if_bad_address(t as usize) {
        return Err(SysErrNo::EFAULT);
    }

    if (remain as isize) < 0 || if_bad_address(remain as usize) {
        return Err(SysErrNo::EFAULT);
    }

    debug!(
        "[sys_clock_nanosleep] clockid is {}, flags is {}, t is {:x}, remain is {:x}",
        clockid, flags, t as usize, remain as usize
    );

    let t = get_data(memory_set.token(), t);
    drop(memory_set);
    drop(process);
    if t.tv_nsec >= 1_000_000_000usize {
        return Err(SysErrNo::EINVAL);
    }

    let waittime = t.tv_sec * 1_000_000_000usize + t.tv_nsec;

    let begin = get_time_ms() * 1_000_000usize;
    let endtime = if flags == TIME_ABSTIME {
        //绝对时间
        t
    } else {
        //相对时间
        get_time_spec() + t
    };

    debug!(
        "[sys_clock_nanosleep] ready to sleep for {} sec, {} nsec",
        t.tv_sec, t.tv_nsec
    );

    while get_time_ms() * 1_000_000usize - begin < waittime {
        if let Some(_) = check_if_any_sig_for_current_task() {
            //被信号唤醒
            debug!("interupt by signal");
            if remain as usize != 0 {
                let process = task.process.inner_lock();
                let memory_set = process.get_locked_memory_set_read();
                safe_put_data(&*memory_set, remain, calculate_left_timespec(endtime));
            }
            //handle_signal(signo);
            return Err(SysErrNo::EINTR);
        }
        suspend_current_and_run_next();
    }
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/set_robust_list.2.html
pub fn sys_set_robust_list(head: usize, len: usize) -> SyscallRet {
    if len != crate::task::RobustList::HEAD_SIZE {
        debug!("sys_set_robust_list len != HEAD_SIZE. early return");
        return Err(SysErrNo::EINVAL);
    }
    let task = current_task().unwrap();
    let mut task_inner = task.inner_lock();
    task_inner.robust_list.head = head;
    task_inner.robust_list.len = len; // 要不把它取消注释了？
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/get_robust_list.2.html
pub fn sys_get_robust_list(pid: usize, head_ptr: *mut usize, len_ptr: *mut usize) -> SyscallRet {
    let mut task = tid_to_task::tid2task(pid);
    if task.is_none() && pid == 0 {
        task = current_task();
    }
    if let Some(task) = task {
        let task_inner = task.inner_lock();
        let token = task.process.inner_lock().get_locked_memory_set_read().token();
        put_data(token, head_ptr, task_inner.robust_list.head);
        put_data(token, len_ptr, task_inner.robust_list.len);
        Ok(0)
    } else {
        Err(SysErrNo::ESRCH)
    }
}
