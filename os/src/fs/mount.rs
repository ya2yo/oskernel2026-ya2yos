use alloc::{format, string::String, sync::Arc, vec::Vec};
use log::debug;
use spin::{Lazy, Mutex};

const MNT_MAXLEN: usize = 16;

pub struct MountTable {
    mnt_list: Vec<(String, String, String, u32)>, // special, dir, fstype
}

impl MountTable {
    /// 记录一个挂载项，或在 `MS_REMOUNT` 场景更新已有挂载项。
    ///
    /// 当前挂载表只保存源路径、目标目录、文件系统类型和挂载标志，不建立真正的
    /// VFS mount root 视图。若目标目录已经存在且 flags 带 `MS_REMOUNT`，则更新
    /// 该项的 source/fstype/flags；若目标目录已经存在但不是 remount，保持原项并
    /// 返回成功。
    ///
    /// 返回 `0` 表示记录成功或已有挂载项可复用，返回 `-1` 表示挂载表已满。
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
    /// 查询指定目录是否正好是一个挂载点。
    ///
    /// 命中时返回挂载项 `(special, dir, fstype, flags)` 的克隆；未命中返回 `None`。
    pub fn got_mount(&mut self, dir: String) -> Option<(String, String, String, u32)> {
        if let Some(mount) = self.mnt_list.iter().find(|&(_, d, _, _)| *d == dir) {
            return Some((*mount).clone());
        }
        None
    }

    /// 查找覆盖指定路径的最深挂载项。
    ///
    /// 该函数按 Linux 路径前缀语义匹配挂载点：`/mnt` 覆盖 `/mnt` 及其子路径，
    /// 但不会匹配 `/mnt2`。若多个挂载点都覆盖该路径，返回目标目录最长的一项。
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

    /// 生成 `/proc/mounts` 的文本内容。
    ///
    /// 输出包含根文件系统的固定 ext4 记录，以及当前挂载表中的每个挂载项；挂载标志
    /// bit0 被解释为只读 `ro`，否则输出 `rw`。
    pub fn proc_mounts_content(&self) -> String {
        let mut content = String::from(" ext4 / ext rw 0 0\n");
        for (special, dir, fstype, flags) in &self.mnt_list {
            let opts = if flags & 1 != 0 { "ro" } else { "rw" };
            content.push_str(&format!("{} {} {} {} 0 0\n", special, dir, fstype, opts));
        }
        content
    }

    /// 从挂载表中移除一个挂载项。
    ///
    /// 当前实现仅删除表项，不执行 busy 检查或 VFS 视图恢复。为了兼容现有测试，
    /// `special` 可以匹配源设备字段，也可以匹配目标挂载点字段；`flags` 目前保留但
    /// 不参与判断。成功移除返回 `0`，未找到返回 `-1`。
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
