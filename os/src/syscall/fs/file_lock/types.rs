/// 与 Linux struct flock 布局兼容（64 位平台）
///
/// C 布局:
/// ```c
/// struct flock {
///     short l_type;     // offset 0
///     short l_whence;   // offset 2
///     off_t l_start;    // offset 8  (padding after l_whence)
///     off_t l_len;      // offset 16
///     pid_t l_pid;      // offset 24
/// };
/// ```
/// 总大小 = 32 字节（尾部 padding 到 8 字节对齐）
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Flock {
    pub l_type: i16,
    pub l_whence: i16,
    pub l_start: i64,
    pub l_len: i64,
    pub l_pid: i32,
}

impl Flock {
    /// 从原始字节构造（从用户空间拷贝后使用）
    /// 期望 28 字节（不含尾部 padding）
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 28 {
            return None;
        }
        Some(Flock {
            l_type: i16::from_ne_bytes([bytes[0], bytes[1]]),
            l_whence: i16::from_ne_bytes([bytes[2], bytes[3]]),
            // bytes[4..8] 为 padding，跳过
            l_start: i64::from_ne_bytes([
                bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14],
                bytes[15],
            ]),
            l_len: i64::from_ne_bytes([
                bytes[16], bytes[17], bytes[18], bytes[19], bytes[20], bytes[21], bytes[22],
                bytes[23],
            ]),
            l_pid: i32::from_ne_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]),
        })
    }

    /// 将自身写入字节数组（用于 copy_to_user）
    pub fn to_bytes(&self) -> [u8; 32] {
        let mut bytes = [0u8; 32];
        bytes[0..2].copy_from_slice(&self.l_type.to_ne_bytes());
        bytes[2..4].copy_from_slice(&self.l_whence.to_ne_bytes());
        // bytes[4..8] 保持为 0 (padding)
        bytes[8..16].copy_from_slice(&self.l_start.to_ne_bytes());
        bytes[16..24].copy_from_slice(&self.l_len.to_ne_bytes());
        bytes[24..28].copy_from_slice(&self.l_pid.to_ne_bytes());
        // 部分 libc/架构组合把 l_pid 放在尾部 padding 位置，双写可兼容两种布局。
        bytes[28..32].copy_from_slice(&self.l_pid.to_ne_bytes());
        bytes
    }
}
