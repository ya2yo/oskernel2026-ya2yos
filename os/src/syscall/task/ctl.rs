use crate::utils::SysResult;

// https://man7.org/linux/man-pages/man2/get_mempolicy.2.html
pub fn sys_get_mempolicy(
    _policy: usize,
    _nodemask: usize,
    _maxnode: usize,
    _addr: usize,
    _flags: usize,
) -> SysResultsult<isize> {
    log::error!("Unimplemented sys_get_mempolicy");
    Ok(0)
}