//! 该文件实际上实现了socket相关系统调用

use alloc::{collections::vec_deque::VecDeque, format, string::ToString, vec::Vec};
use spin::{Lazy, Mutex};

use crate::{
    fs::{
        make_socket, make_socketpair,
        socket_defs::{SockAddrInet, SocketDomain},
        FileClass, FileDescriptor, OpenFlags,
    }, mm::{get_data, put_data, safe_translated_byte_buffer, translated_refmut}, net::{Socket, SocketOps}, task::{current_task, current_token}, utils::{SysErrNo, SyscallRet}
};
use log::{debug, warn};

pub static UDP_QUEUE: Lazy<Mutex<VecDeque<Vec<u8>>>> = Lazy::new(|| Mutex::new(VecDeque::new()));

/// 参考 https://man7.org/linux/man-pages/man2/socket.2.html
pub fn sys_socket(_domain: u32, _type: u32, _protocol: u32) -> SyscallRet {
    warn!(
        "[sys_socket] domain={}, type={}, protocol={}",
        _domain, _type, _protocol
    );
    let domain: SocketDomain = SocketDomain::from(_domain);
    warn!("domain is {:?}", domain);
    if domain != SocketDomain::Unix && domain != SocketDomain::Inet {
        warn!("Not supported socket domain: {:?}", domain);
    }

    let task = current_task().unwrap();
    let task_inner = task.inner_lock();
    let new_fd = task_inner.fd_table.alloc_fd()?;
    let close_on_exec = (_type & 0o2000000) == 0o2000000;
    let non_block = (_type & 0o4000) == 0o4000;
    let mut flags = OpenFlags::empty();
    if close_on_exec {
        flags |= OpenFlags::O_CLOEXEC;
    }
    if non_block {
        flags |= OpenFlags::O_NONBLOCK;
    }
    task_inner.fd_table.set(
        new_fd,
        FileDescriptor::new(flags, FileClass::Abs(make_socket())),
    );
    task_inner
        .fs_info
        .lock()
        .insert(format!("socket{}", new_fd).to_string(), new_fd);
    Ok(new_fd)
}

/// 参考 https://man7.org/linux/man-pages/man2/bind.2.html
pub fn sys_bind(_sockfd: usize, _addr: *const u8, _addrlen: u32) -> SyscallRet {
    debug!(
        "[sys_bind] fd={}, addr={}, len={}",
        _sockfd, _addr as usize, _addrlen
    );

    let token = current_task()
        .unwrap()
        .get_process()
        .inner_lock()
        .get_locked_memory_set()
        .token();
    // 取出addr的family字段
    let family: u16 = get_data(token, _addr as *const u16);
    let domain = SocketDomain::from(family as u32);
    match domain {
        SocketDomain::Unix => {
            warn!("[sys_bind] is not implemented for AF_UNIX, return Ok(0)");
            return Ok(0);
        }

        SocketDomain::Inet => {
            // 取出addr的字节码
            let mut codes: [u8; 110] = [0; 110];
            for i in 0..(_addrlen as usize) {
                let code = get_data::<u8>(token, unsafe { _addr.byte_add(i) });
                codes[i] = code;
            }
            debug!("code={:?}", &codes[..(_addrlen as usize)]);
            for i in 2..(_addrlen as usize) {
                if codes[i] != 0 {
                    warn!("We only support sys_bind(INET) for all-zero addr");
                }
            }
            // 反正我们已经知道addr=全0了，剩余的操作并没有必要

            Ok(0)
        }

        _ => {
            warn!(
                "[sys_bind] is not implemented for domain {:?}, return Ok(0)",
                domain
            );
            return Ok(0);
        }
    }
}

/// 参考 https://man7.org/linux/man-pages/man2/getsockname.2.html
pub fn sys_getsockname(fd: usize, addr: *const u8, addr_len: usize) -> SyscallRet {
    warn!(
        "[sys_getsockname] fd={}, addr={}, len={}",
        fd, addr as usize, addr_len
    );
    if (fd as isize) < 0 {
        return Err(SysErrNo::EBADF);
    }
    if addr as uszie == 0 {
        return Err(SysErrNo::EFAULT);
    }
    log::info!("sys_getsockname fd: {}, addr: {:#x}, addr_len: {}", fd, addr, addr_len);
    let task = current_task().unwrap();
    let token=current_token();
    let inner = task.inner_lock();
    if fd >= inner.fd_table.len() {
        return Err(SysErrNo::EBADF);
    }
    let file=match &inner.fd_table.try_get(fd) {
        Some(f)=>f.clone(),
        None=>return Err(SysErrNo::EBADF),
    };
    drop(inner);
    let socket = file.socket()?;
    let local_addr = socket.get_local_addr(); // 假设 Socket 有这个方法
    let addr_bytes = local_addr.as_bytes();   // 转换为 sockaddr 字节流

    let mut user_len = copy_from_user(addr_len_ptr); // 从用户态读入缓冲区大小
    let real_len = addr_bytes.len() as u32;
    
    // 确定拷贝长度：取用户缓冲区和实际地址长度的最小值
    let copy_len = core::cmp::min(user_len, real_len);
    
    // 拷贝地址数据到用户态 addr 指向的内存
    copy_to_user(addr, &addr_bytes[..copy_len as usize]);
    
    // 把真实的地址长度写回用户态 addr_len_ptr
    copy_to_user(addr_len_ptr, &real_len);
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/getpeername.2.html
pub fn sys_getpeername(_sockfd: usize, _addr: *const u8, _addrlen: u32) -> SyscallRet {
    warn!(
        "[sys_getpeername] fd={}, addr={}, len={}",
        _sockfd, _addr as usize, _addrlen
    );
    warn!("sys_getpeername is not implemented, return Err(SysErrNo::Default)");
    Err(SysErrNo::Default)
}

/// 参考 https://man7.org/linux/man-pages/man2/setsockopt.2.html
pub fn sys_setsockopt(
    _sockfd: usize,
    _level: u32,
    _optname: u32,
    _optcal: *const u8,
    _optlen: u32,
) -> SyscallRet {
    warn!("[sys_setsockopt] fd={}", _sockfd,);
    warn!("sys_setsockopt is not implemented, return Ok(0)");

    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/sendto.2.html
pub fn sys_sendto(
    _sockfd: usize,
    _buf: *const u8,
    _len: usize,
    _flags: u32,
    _dest_addr: *const u8,
    _addrlen: u32,
) -> SyscallRet {
    debug!("[sys_sendto] fd={}", _sockfd,);
    // 断言：dest_addr必须为AF_INET + 127.0.0.1 + 0 port
    let token = current_task()
        .unwrap()
        .get_process()
        .inner_lock()
        .get_locked_memory_set()
        .token();

    let sockaddr_in = get_data(token, _dest_addr as *const SockAddrInet);
    debug!("family = {:?}", sockaddr_in.family);
    debug!("port   = {:?}", sockaddr_in.port);
    debug!("addr   = {:#x}", sockaddr_in.addr);
    if SocketDomain::from(sockaddr_in.family as u32) != SocketDomain::Inet {
        warn!("sys_sendto only support AF_INET dest_addr !");
    }
    if sockaddr_in.port != 0 {
        warn!("sys_sendto only support port == 0 !");
    }
    if sockaddr_in.addr != 0x100007f {
        // 127.0.0.1
        warn!("sys_sendto only support addr=127.0.0.1 !");
    }
    // 现在我们知道目标是127.0.0.1的任意端口
    // 读取buf
    // TODO: 下面的代码十分粗糙
    let mut vec: Vec<u8> = Vec::new();
    // let p = _buf;
    for i in 0.._len {
        unsafe {
            let c = get_data(token, _buf.byte_add(i));
            vec.push(c);
        }
    }
    // 将内容注册到全局队列中
    UDP_QUEUE.try_lock().unwrap().push_back(vec);
    Ok(1)
}

/// 参考 https://man7.org/linux/man-pages/man2/recvfrom.2.html
pub fn sys_recvfrom(
    _sockfd: usize,
    buf: *mut u8,
    _len: usize,
    _flags: u32,
    _src_addr: *const u8,
    _addrlen: u32,
) -> SyscallRet {
    debug!("ENTER recvfrom");
    let token = current_task()
        .unwrap()
        .get_process()
        .inner_lock()
        .get_locked_memory_set()
        .token();
    let sockaddr_in = get_data(token, _src_addr as *const SockAddrInet);
    debug!("family = {:?}", sockaddr_in.family);
    debug!("port   = {:?}", sockaddr_in.port);
    debug!("addr   = {:#x}", sockaddr_in.addr);
    if SocketDomain::from(sockaddr_in.family as u32) != SocketDomain::Inet {
        warn!("sys_recvfrom only support AF_INET dest_addr !");
    }
    if sockaddr_in.port != 0 {
        warn!("sys_recvfrom only support port == 0 !");
    }
    if sockaddr_in.addr != 0x100007f {
        // 127.0.0.1
        warn!("sys_recvfrom only support addr=127.0.0.1 !");
    }
    let vec = UDP_QUEUE.try_lock().unwrap().pop_front().unwrap();
    // 把vec写回去
    // TODO: 下面的代码十分粗糙
    let real_len = vec.len().min(_len); // 取两个中较小的那个
    for i in 0..real_len {
        unsafe {
            put_data(token, buf.byte_add(i), vec[i]);
        }
    }
    Ok(real_len)
}

/// 参考 https://man7.org/linux/man-pages/man2/listen.2.html
pub fn sys_listen(_sockfd: usize, _backlog: u32) -> SyscallRet {
    warn!("[sys_listen] fd={}", _sockfd,);
    warn!("sys_listen is not implemented, return Ok(0)");
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/connect.2.html
pub fn sys_connect(_sockfd: usize, _addr: *const u8, _addrlen: u32) -> SyscallRet {
    warn!("[sys_connect] fd={}", _sockfd,);
    warn!("sys_connect is not implemented, return Ok(0)");
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/accept.2.html
pub fn sys_accept(_sockfd: usize, _addr: *const u8, _addrlen: u32) -> SyscallRet {
    warn!("[sys_accept] fd={}", _sockfd,);
    warn!("sys_accept is not implemented, return Ok(0)");
    Ok(0)
}

pub fn sys_accept4(_sockfd: usize, _addr: *const u8, _addrlen: u32, _flags: u32) -> SyscallRet {
    warn!("[sys_accept4] fd={}", _sockfd,);
    warn!("sys_accept4 is not implemented, return Ok(0)");
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/sendmsg.2.html
pub fn sys_sendmsg(_sockfd: usize, _addr: *const u8, _flags: u32) -> SyscallRet {
    warn!("[sys_sendmsg] fd={}", _sockfd,);
    warn!("sys_sendmsg is not implemented, return Ok(0)");
    Ok(0)
}

pub fn sys_socketpair(domain: u32, stype: u32, protocol: u32, sv: *mut u32) -> SyscallRet {
    debug!(
        "[sys_socketpair] domain is {}, type is {}, protocol is {}, sv is {}",
        domain, stype, protocol, sv as usize
    );

    return Ok(0);

    let task = current_task().unwrap();
    let inner = task.inner_lock();
    let token = task.process.inner_lock().get_locked_memory_set().token();

    let (socket1, socket2) = make_socketpair();
    let close_on_exec = (stype & 0o2000000) == 0o2000000;
    let non_block = (stype & 0o4000) == 0o4000;
    let mut flags = OpenFlags::empty();
    if close_on_exec {
        flags |= OpenFlags::O_CLOEXEC;
    }
    if non_block {
        flags |= OpenFlags::O_NONBLOCK;
    }

    let new_fd1 = inner.fd_table.alloc_fd()?;
    inner
        .fd_table
        .set(new_fd1, FileDescriptor::new(flags, FileClass::Abs(socket1)));
    inner
        .fs_info
        .lock()
        .insert(format!("socket{}", new_fd1).to_string(), new_fd1);

    let new_fd2 = inner.fd_table.alloc_fd()?;
    inner
        .fd_table
        .set(new_fd2, FileDescriptor::new(flags, FileClass::Abs(socket2)));
    inner
        .fs_info
        .lock()
        .insert(format!("socket{}", new_fd2).to_string(), new_fd2);

    *translated_refmut(token, sv) = new_fd1 as u32;
    *translated_refmut(token, unsafe { sv.add(1) }) = new_fd2 as u32;

    Ok(0)
}
