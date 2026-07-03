use alloc::{format, string::String, sync::Arc, vec::Vec};
use log::debug;
use spin::{Lazy, Mutex};

const MNT_MAXLEN: usize = 16;

pub struct MountTable {
    mnt_list: Vec<(String, String, String, u32)>, // special, dir, fstype
}

impl MountTable {
    /// 执行挂载操作
    ///
    /// # 参数
    /// - `special`: 源设备路径（如 "/dev/sdc1" 或 "none"）
    /// - `dir`: 目标挂载点（如 "/mnt"）
    /// - `fstype`: 文件系统类型（如 "vfat", "ext2"）
    /// - `flags`: 挂载标志位（如只读、重新挂载等）
    /// - `data`: 挂载所需的额外参数字符串（通常由具体文件系统解析）
    ///
    /// # 返回值
    /// - `0`: 成功
    /// - `-1`: 挂载表已满或失败
    pub fn mount(
        &mut self,
        special: String,
        dir: String,
        fstype: String,
        flags: u32,
        data: String,
    ) -> isize {
        if self.mnt_list.len() == MNT_MAXLEN {
            return -1;
        }
        // 已存在
        if let Some((mountspecial, _, mountfstype, mountflags)) =
            self.mnt_list.iter_mut().find(|(_, d, _, _)| *d == dir)
        {
            if flags & 32 != 0 {
                //包含MS_REMOUNT标志
                *mountspecial = special;
                *mountfstype = fstype;
                *mountflags = flags;
            }
            return 0;
        }

        // todo
        _ = data;

        //log::info!("push mount dir {} with flags={}", dir, flags);

        self.mnt_list.push((special, dir, fstype, flags));
        0
    }
    /// 查询某个目录是否是挂载点
    ///
    /// # 返回
    /// 如果该目录已挂载，返回该项信息的克隆，否则返回 None
    pub fn got_mount(&mut self, dir: String) -> Option<(String, String, String, u32)> {
        if let Some(mount) = self.mnt_list.iter().find(|&(_, d, _, _)| *d == dir) {
            return Some((*mount).clone());
        }
        None
    }

    pub fn mount_for_path(&self, path: &str) -> Option<(String, String, String, u32)> {
        self.mnt_list
            .iter()
            .filter(|(_, dir, _, _)| {
                if dir == "/" {
                    path.starts_with('/')
                } else {
                    path == dir.as_str()
                        || path
                            .strip_prefix(dir.as_str())
                            .map_or(false, |rest| rest.starts_with('/'))
                }
            })
            .max_by_key(|(_, dir, _, _)| dir.len())
            .cloned()
    }

    pub fn proc_mounts_content(&self) -> String {
        let mut content = String::from(" ext4 / ext rw 0 0\n");
        for (special, dir, fstype, flags) in &self.mnt_list {
            let opts = if flags & 1 != 0 { "ro" } else { "rw" };
            content.push_str(&format!("{} {} {} {} 0 0\n", special, dir, fstype, opts));
        }
        content
    }

    /// 执行卸载操作
    ///
    /// # 参数
    /// - `special`: 在标准 Linux 中通常是路径，但此实现中可能是设备名或挂载点路径
    /// - `flags`: 卸载标志（如 MNT_FORCE 等）
    pub fn umount(&mut self, special: String, flags: u32) -> isize {
        let len = self.mnt_list.len();

        // todo
        _ = flags;

        for i in 0..len {
            // 根据系统调用规范应该是 self.mnt_list[i].0 == special
            // 然而测试程序传的是 dir，因此这里加了一个或运算
            if self.mnt_list[i].0 == special || self.mnt_list[i].1 == special {
                self.mnt_list.remove(i);
                return 0;
            }
        }
        -1
    }
}
/// 全局单例挂载表
///
/// - `Lazy`: 确保在第一次使用时才初始化内存。
/// - `Arc`: 原子引用计数，允许跨线程共享该实例。
/// - `Mutex`: 互斥锁，确保在多核并发操作挂载表时不会出现竞态条件。
pub static MNT_TABLE: Lazy<Arc<Mutex<MountTable>>> = Lazy::new(|| {
    let mnt_table = MountTable {
        mnt_list: Vec::new(),
    };
    Arc::new(Mutex::new(mnt_table))
});
