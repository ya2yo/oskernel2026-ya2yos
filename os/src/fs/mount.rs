use alloc::{format, string::String, sync::Arc, vec, vec::Vec};
use linux_raw_sys::general::*;
use spin::{Lazy, Mutex};

use crate::utils::SysErrNo;

const MNT_MAXLEN: usize = 256;

// Linux mount(2) propagation flags. Keep these local because MountTable is
// also used by the legacy mount API, which does not otherwise need syscall
// flag definitions.
const PROPAGATION_MASK: u32 = MS_UNBINDABLE | MS_PRIVATE | MS_SLAVE | MS_SHARED;

#[derive(Clone)]
struct MountEntry {
    special: String,
    dir: String,
    fstype: String,
    flags: u32,
    // Mounts in the same group receive new child mount events at matching
    // relative paths.
    shared_group: Option<u64>,
    // A slave receives mount events from this shared peer group, but never
    // sends events back to it. A slave may also have its own shared group.
    master_group: Option<u64>,
    // Linux forbids bind-cloning an unbindable mount.
    unbindable: bool,
    // All copies made for one mount event are removed together by umount.
    event_group: u64,
}

pub struct MountTable {
    mnt_list: Vec<MountEntry>,
    next_group: u64,
}

impl MountTable {
    /// 分配新的传播事件或 shared peer group 标识。
    ///
    /// 标识 0 保留为无效值；计数回绕时跳过 0，避免与未分组状态混淆。
    fn next_group(&mut self) -> u64 {
        let group = self.next_group;
        self.next_group = self.next_group.wrapping_add(1).max(1);
        group
    }

    /// 判断 `path` 是否等于 `base`，或位于 `base` 的目录树下。
    ///
    /// 路径前缀必须落在目录边界上，因此 `/mnt` 不会覆盖 `/mnt2`；根目录覆盖
    /// 所有绝对路径。
    fn path_is_at_or_below(path: &str, base: &str) -> bool {
        base == "/"
            || path == base
            || path
                .strip_prefix(base)
                .map_or(false, |rest| rest.starts_with('/'))
    }

    /// 将相对路径追加到挂载 peer 的根路径，生成对应的传播目标路径。
    ///
    /// 空相对路径表示 peer 根本身；根目录作为 base 时避免产生 `//`。
    fn append_relative(base: &str, relative: &str) -> String {
        if relative.is_empty() {
            return String::from(base);
        }
        if base == "/" {
            format!("/{}", relative)
        } else {
            format!("{}/{}", base.trim_end_matches('/'), relative)
        }
    }

    /// 返回覆盖 `path` 的顶层挂载条目索引。
    ///
    /// 选择目标路径最长的条目；相同目标路径存在多层挂载时选择最后记录的可见层。
    fn top_mount_index_for_path(&self, path: &str) -> Option<usize> {
        self.mnt_list
            .iter()
            .enumerate()
            .filter(|(_, mount)| Self::path_is_at_or_below(path, &mount.dir))
            .max_by_key(|(idx, mount)| (mount.dir.len(), *idx))
            .map(|(idx, _)| idx)
    }

    /// 返回恰好挂载在 `path` 上的顶层挂载条目索引。
    ///
    /// 与 [`Self::top_mount_index_for_path`] 不同，此方法不匹配祖先挂载点。
    fn top_mount_index_at_path(&self, path: &str) -> Option<usize> {
        self.mnt_list
            .iter()
            .enumerate()
            .rev()
            .find_map(|(idx, mount)| (mount.dir == path).then_some(idx))
    }

    /// 修改挂载点及可选递归子树的 propagation type。
    ///
    /// `MS_SHARED` 将同一 mount event 的副本放入一个新的 shared group。
    /// `MS_SLAVE` 保留原 shared group 作为 master；`MS_PRIVATE` 和
    /// `MS_UNBINDABLE` 断开所有传播关系。
    fn set_propagation(&mut self, dir: &str, flags: u32) {
        let recursive = flags & MS_REC != 0;
        let Some(root_idx) = self.top_mount_index_at_path(dir) else {
            return;
        };
        let root_event = self.mnt_list[root_idx].event_group;
        let select_event_copies = flags & MS_SHARED != 0;
        let selected: Vec<usize> = self
            .mnt_list
            .iter()
            .enumerate()
            .filter_map(|(idx, mount)| {
                (mount.dir == dir
                    || (recursive && Self::path_is_at_or_below(&mount.dir, dir))
                    || (select_event_copies && mount.event_group == root_event))
                    .then_some(idx)
            })
            .collect();

        if flags & MS_SHARED != 0 {
            let group = self.next_group();
            for idx in selected {
                let mount = &mut self.mnt_list[idx];
                mount.shared_group = Some(group);
                mount.unbindable = false;
            }
        } else if flags & MS_SLAVE != 0 {
            for idx in selected {
                let mount = &mut self.mnt_list[idx];
                mount.master_group = mount.shared_group.or(mount.master_group);
                mount.shared_group = None;
                mount.unbindable = false;
            }
        } else {
            let unbindable = flags & MS_UNBINDABLE != 0;
            for idx in selected {
                let mount = &mut self.mnt_list[idx];
                mount.shared_group = None;
                mount.master_group = None;
                mount.unbindable = unbindable;
            }
        }
    }

    /// Expand a new child mount into every reachable peer and slave mount.
    ///
    /// The returned pairs retain the mount that receives each copy, so the
    /// child can inherit that receiver's propagation state. This matters for
    /// a shared slave: later child mounts must flow to its own peers/slaves,
    /// but never back to its master.
    fn propagation_targets(&self, dir: &str, parent_idx: usize) -> Vec<(String, usize)> {
        let parent = &self.mnt_list[parent_idx];
        let relative = dir
            .strip_prefix(parent.dir.as_str())
            .unwrap_or("")
            .trim_start_matches('/');
        let mut targets = vec![(String::from(dir), parent_idx)];
        let mut pending = vec![parent_idx];
        let mut visited = Vec::new();

        while let Some(idx) = pending.pop() {
            if visited.iter().any(|seen| *seen == idx) {
                continue;
            }
            visited.push(idx);
            let mount = &self.mnt_list[idx];
            let mut receivers = Vec::new();
            if let Some(group) = mount.shared_group {
                for (candidate_idx, candidate) in self.mnt_list.iter().enumerate() {
                    if candidate.shared_group == Some(group)
                        && self.top_mount_index_at_path(&candidate.dir) == Some(candidate_idx)
                    {
                        receivers.push(candidate_idx);
                    }
                }
                for (candidate_idx, candidate) in self.mnt_list.iter().enumerate() {
                    if candidate.master_group == Some(group)
                        && self.top_mount_index_at_path(&candidate.dir) == Some(candidate_idx)
                    {
                        receivers.push(candidate_idx);
                    }
                }
            }

            for receiver_idx in receivers {
                let target = Self::append_relative(&self.mnt_list[receiver_idx].dir, relative);
                if !targets.iter().any(|(known, _)| known == &target) {
                    targets.push((target, receiver_idx));
                }
                pending.push(receiver_idx);
            }
        }
        targets
    }

    /// 记录一次挂载，并返回路径化 VFS 需要执行的 bind tree 镜像动作。
    ///
    /// 对 propagation-only 操作更新挂载状态，对 remount 更新顶层属性；普通挂载
    /// 创建一层新条目。目标父挂载的 shared peer 与 slave 后代都会收到同一相对路径
    /// 的副本。返回的 `(source, target)` 仅适用于 `MS_BIND`，由 syscall 层在表锁外
    /// 完成目录镜像。
    ///
    /// # Errors
    ///
    /// 返回 `EINVAL` 表示尝试 bind-clone unbindable source；返回 `ENOSPC` 表示挂载
    /// 条目数量将超过 `MNT_MAXLEN`。在这两种错误下不会新增任何条目。
    pub fn mount(
        &mut self,
        special: String,
        dir: String,
        fstype: String,
        flags: u32,
        data: String,
    ) -> Result<Vec<(String, String)>, SysErrNo> {
        _ = data;

        if flags & PROPAGATION_MASK != 0 && flags & (MS_BIND | MS_REMOUNT) == 0 {
            self.set_propagation(&dir, flags);
            return Ok(Vec::new());
        }

        if flags & MS_REMOUNT != 0 {
            if let Some(idx) = self.top_mount_index_at_path(&dir) {
                let mount = &mut self.mnt_list[idx];
                mount.special = special;
                mount.fstype = fstype;
                mount.flags = flags;
            }
            return Ok(Vec::new());
        }

        if flags & MS_BIND != 0
            && self
                .top_mount_index_for_path(&special)
                .is_some_and(|idx| self.mnt_list[idx].unbindable)
        {
            return Err(SysErrNo::EINVAL);
        }

        let source = (flags & MS_BIND != 0)
            .then(|| self.top_mount_index_at_path(&special))
            .flatten()
            .map(|idx| self.mnt_list[idx].clone());
        let parent_idx = self.top_mount_index_for_path(&dir);
        let targets = parent_idx
            .map(|idx| self.propagation_targets(&dir, idx))
            .unwrap_or_else(|| vec![(dir.clone(), usize::MAX)]);

        if self.mnt_list.len() + targets.len() > MNT_MAXLEN {
            return Err(SysErrNo::ENOSPC);
        }
        let event_group = self.next_group();
        for (target, receiver_idx) in &targets {
            let receiver = (*receiver_idx != usize::MAX).then(|| &self.mnt_list[*receiver_idx]);
            let shared_group = source
                .as_ref()
                .and_then(|mount| mount.shared_group)
                .or_else(|| receiver.and_then(|mount| mount.shared_group));
            let master_group = source
                .as_ref()
                .and_then(|mount| mount.master_group)
                .or_else(|| receiver.and_then(|mount| mount.master_group));
            self.mnt_list.push(MountEntry {
                special: special.clone(),
                dir: target.clone(),
                fstype: fstype.clone(),
                flags,
                shared_group,
                master_group,
                unbindable: false,
                event_group,
            });
        }

        if flags & MS_BIND == 0 {
            return Ok(Vec::new());
        }
        Ok(targets
            .into_iter()
            .filter(|(target, _)| target != &special)
            .map(|(target, _)| (special.clone(), target))
            .collect())
    }

    /// 查询精确挂载点的可见顶层，并复制返回 `(source, dir, fstype, flags)`。
    ///
    /// 若该路径没有挂载层，返回 `None`。更早叠加在同一路径上的挂载不会被返回。
    pub fn got_mount(&mut self, dir: String) -> Option<(String, String, String, u32)> {
        self.top_mount_index_at_path(&dir).map(|idx| {
            let mount = &self.mnt_list[idx];
            (
                mount.special.clone(),
                mount.dir.clone(),
                mount.fstype.clone(),
                mount.flags,
            )
        })
    }

    /// 查询覆盖 `path` 的可见顶层挂载，并复制返回其元数据。
    ///
    /// 在多层嵌套挂载中优先选择目标路径最长者；同一目标存在叠加层时选择最新层。
    pub fn mount_for_path(&self, path: &str) -> Option<(String, String, String, u32)> {
        self.top_mount_index_for_path(path).map(|idx| {
            let mount = &self.mnt_list[idx];
            (
                mount.special.clone(),
                mount.dir.clone(),
                mount.fstype.clone(),
                mount.flags,
            )
        })
    }

    /// 序列化当前挂载表为 `/proc/mounts` 的兼容文本。
    ///
    /// 输出始终包含根 ext4 记录；每层挂载各输出一行。flags 的 bit 0 按 Linux
    /// `MS_RDONLY` 显示为 `ro`，其余显示为 `rw`。
    pub fn proc_mounts_content(&self) -> String {
        let mut content = String::from(" ext4 / ext rw 0 0\n");
        for mount in &self.mnt_list {
            let opts = if mount.flags & 1 != 0 { "ro" } else { "rw" };
            content.push_str(&format!(
                "{} {} {} {} 0 0\n",
                mount.special, mount.dir, mount.fstype, opts
            ));
        }
        content
    }

    /// 卸载 `dir` 的顶层及同一 mount event 创建的所有 peer 副本。
    ///
    /// 成功返回 0；路径没有顶层挂载时返回 -1。卸载后，保留在同一路径下的更早
    /// 挂载层重新可见。`flags` 当前只保留 ABI 入口，尚不影响卸载策略。
    pub fn umount(&mut self, dir: String, flags: u32) -> isize {
        _ = flags;
        let Some(idx) = self.top_mount_index_at_path(&dir) else {
            return -1;
        };
        let event_group = self.mnt_list[idx].event_group;
        self.mnt_list
            .retain(|mount| mount.event_group != event_group);
        0
    }
}

pub static MNT_TABLE: Lazy<Arc<Mutex<MountTable>>> = Lazy::new(|| {
    Arc::new(Mutex::new(MountTable {
        mnt_list: Vec::new(),
        next_group: 1,
    }))
});
