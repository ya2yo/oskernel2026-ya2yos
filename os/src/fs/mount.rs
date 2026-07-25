use alloc::{collections::BTreeMap, format, string::String, sync::Arc, vec, vec::Vec};
use linux_raw_sys::general::*;
use spin::{Lazy, Mutex};

use crate::utils::{SysErrNo, SysResult};

const MNT_MAXLEN: usize = 256;

bitflags! {
    /// 所有 Linux mount(2) 标志位的完整定义。
    ///
    /// 按 Linux 语义分为三类：
    /// - **操作类型标志**（互斥）：`BIND`, `MOVE`, `REMOUNT`
    /// - **传播类型标志**：`SHARED`, `PRIVATE`, `SLAVE`, `UNBINDABLE`
    /// - **挂载属性标志**：`RDONLY`, `NOSUID`, `NODEV` 等
    ///
    /// 标志位值与 `linux_raw_sys::general::MS_*` 完全一致。
    pub struct MountFlags: u32 {
        // ---- 挂载属性 ----
        const RDONLY       = MS_RDONLY;
        const NOSUID       = MS_NOSUID;
        const NODEV        = MS_NODEV;
        const NOEXEC       = MS_NOEXEC;
        const SYNCHRONOUS  = MS_SYNCHRONOUS;
        const MANDLOCK     = MS_MANDLOCK;
        const DIRSYNC      = MS_DIRSYNC;
        const NOSYMFOLLOW  = MS_NOSYMFOLLOW;
        const NOATIME      = MS_NOATIME;
        const NODIRATIME   = MS_NODIRATIME;
        const RELATIME     = MS_RELATIME;
        const STRICTATIME  = MS_STRICTATIME;
        // MS_LAZYTIME (33554432) 暂不包含：ext4_lw 不支持。

        // ---- 操作类型（互斥） ----
        const REMOUNT      = MS_REMOUNT;
        const BIND         = MS_BIND;
        const MOVE         = MS_MOVE;

        // ---- 传播类型 ----
        const SHARED       = MS_SHARED;
        const PRIVATE      = MS_PRIVATE;
        const SLAVE        = MS_SLAVE;
        const UNBINDABLE   = MS_UNBINDABLE;

        // ---- 递归 ----
        const REC          = MS_REC;

        // ---- 杂项 ----
        const SILENT       = MS_SILENT;
        const POSIXACL     = MS_POSIXACL;
        const I_VERSION    = MS_I_VERSION;
        const ACTIVE       = MS_ACTIVE;
        const NOUSER       = MS_NOUSER;
    }
}

/// 传播类型掩码：`SHARED | PRIVATE | SLAVE | UNBINDABLE`
pub const PROPAGATION_MASK: MountFlags = MountFlags::SHARED
    .union(MountFlags::PRIVATE)
    .union(MountFlags::SLAVE)
    .union(MountFlags::UNBINDABLE);

/// 操作类型掩码：`BIND | MOVE | REMOUNT`
pub const OPERATION_MASK: MountFlags = MountFlags::BIND
    .union(MountFlags::MOVE)
    .union(MountFlags::REMOUNT);

/// 可 remount 修改的属性：不包含操作类型和传播类型
pub const REMOUNT_ATTR_MASK: MountFlags = MountFlags::RDONLY
    .union(MountFlags::NOSUID)
    .union(MountFlags::NODEV)
    .union(MountFlags::NOEXEC)
    .union(MountFlags::SYNCHRONOUS)
    .union(MountFlags::MANDLOCK)
    .union(MountFlags::DIRSYNC)
    .union(MountFlags::NOSYMFOLLOW)
    .union(MountFlags::NOATIME)
    .union(MountFlags::NODIRATIME)
    .union(MountFlags::RELATIME)
    .union(MountFlags::STRICTATIME)
    .union(MountFlags::SILENT)
    .union(MountFlags::POSIXACL)
    .union(MountFlags::I_VERSION);

impl MountFlags {
    /// 是否为 MS_MOVE 操作。
    pub fn is_move(self) -> bool {
        self.contains(MountFlags::MOVE)
    }

    /// 是否为 MS_BIND 操作。
    pub fn is_bind(self) -> bool {
        self.contains(MountFlags::BIND)
    }

    /// 是否为 MS_REMOUNT 操作。
    pub fn is_remount(self) -> bool {
        self.contains(MountFlags::REMOUNT)
    }

    /// 是否为纯粹的传播类型修改（设置了传播标志但未设置 BIND/REMOUNT）。
    pub fn is_propagation_only(self) -> bool {
        self.intersects(PROPAGATION_MASK) && !self.intersects(OPERATION_MASK)
    }

    /// 返回传播类型位（SHARED | PRIVATE | SLAVE | UNBINDABLE）。
    pub fn propagation_bits(self) -> MountFlags {
        self.intersection(PROPAGATION_MASK)
    }

    /// 是否为普通新挂载（非 BIND, MOVE, REMOUNT, propagation-only）。
    pub fn is_regular(self) -> bool {
        !self.intersects(OPERATION_MASK.union(PROPAGATION_MASK))
    }
}

/// 简化路径化 ext4 挂载模型的逻辑容量跟踪。
///
/// 当前 VFS 只维护一个根 superblock，因此 ext4 loop 挂载无法提供独立的块分配器。
/// 这个计数器保留了调用者最需要的约束：挂载点下文件的增长会消耗建模的可用数据容量，
/// 超出时返回 `ENOSPC`。
struct MountUsage {
    limit: usize,                   // 该挂载点的可用数据容量上限
    used: usize,                    // 已使用的容量
    files: BTreeMap<String, usize>, // 每个文件占用的容量
}

// 真实的 ext4 小镜像会为 journal、inode 表和 superblock 元数据预留空间。
// 路径化模型没有块组分配器，因此保守地将格式化镜像的约 1/3 开放为普通文件数据
// 容量；这样既为元数据留出空间，又能在不分配整个假设备的情况下保持 ENOSPC 检查的
// 确定性。

#[derive(Clone)]
struct MountEntry {
    /// "special" 是 mount(2) 的第一个参数，即挂载的"源"：
    /// - 普通挂载：ext4 镜像路径（相当于块设备）
    /// - bind mount：被 bind 的源目录路径
    /// - remount：新的源路径
    special: String,
    dir: String,
    fstype: String,
    flags: MountFlags,
    // MS_BIND 是 mount(2) 的操作标志。将 bind 状态独立保存是因为后续的
    // MS_REMOUNT 会替换 `flags`，但不会改变底层 bind mount 的本质。
    is_bind: bool,
    // 同一 shared group 中的挂载会在匹配的相对路径上收到新的子挂载事件。
    shared_group: Option<u64>,
    // slave 从该 shared peer group 接收挂载事件，但不会反向传播事件。
    // slave 也可以拥有自己的 shared group。
    master_group: Option<u64>,
    // Linux 禁止对 unbindable 挂载进行 bind-clone 操作。
    unbindable: bool,
    // 同一 mount event 产生的所有副本在 umount 时会一起被移除。
    event_group: u64,
    // 同一 ext4 挂载的传播副本之间共享的容量配额。
    quota: Option<Arc<Mutex<MountUsage>>>,
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
    /// `SHARED` 将同一 mount event 的副本放入一个新的 shared group。
    /// `SLAVE` 保留原 shared group 作为 master；`PRIVATE` 和
    /// `UNBINDABLE` 断开所有传播关系。
    fn set_propagation(&mut self, dir: &str, flags: MountFlags) {
        let recursive = flags.contains(MountFlags::REC);
        let Some(root_idx) = self.top_mount_index_at_path(dir) else {
            return;
        };
        let root_event = self.mnt_list[root_idx].event_group;
        let select_event_copies = flags.contains(MountFlags::SHARED);
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

        if flags.contains(MountFlags::SHARED) {
            let group = self.next_group();
            for idx in selected {
                let mount = &mut self.mnt_list[idx];
                mount.shared_group = Some(group);
                mount.unbindable = false;
            }
        } else if flags.contains(MountFlags::SLAVE) {
            for idx in selected {
                let mount = &mut self.mnt_list[idx];
                // 已经是 shared slave 的挂载已有上游 master。取消其 shared 状态时
                // 必须保留该 master 关系，不能用挂载自己原先的 peer group 替代。
                mount.master_group = mount.master_group.or(mount.shared_group);
                mount.shared_group = None;
                mount.unbindable = false;
            }
        } else {
            let unbindable = flags.contains(MountFlags::UNBINDABLE);
            for idx in selected {
                let mount = &mut self.mnt_list[idx];
                mount.shared_group = None;
                mount.master_group = None;
                mount.unbindable = unbindable;
            }
        }
    }

    /// 将新的子挂载扩展到所有可达的 peer 和 slave 挂载。
    ///
    /// 返回的 (目标路径, 接收者索引) 对保留了接收每个副本的挂载，使子挂载可以
    /// 继承接收者的传播状态。这对 shared slave 很重要：后续子挂载事件必须流向
    /// 它自己的 peer/slave，但绝不回流到它的 master。
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
                // 移动到 shared 父节点下的挂载，每个接收者各有一份条目，
                // 均保留其原始 event group。移动根下的事件必须先到达这些
                // 匹配的根，以保持其相对于父节点的偏移（例如 `parent/child`
                // 映射为 `peer/child`，而非 `peer`）。
                for (candidate_idx, candidate) in self.mnt_list.iter().enumerate() {
                    if candidate_idx != idx
                        && candidate.event_group == mount.event_group
                        && self.top_mount_index_at_path(&candidate.dir) == Some(candidate_idx)
                    {
                        receivers.push(candidate_idx);
                    }
                }
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
                let receiver = &self.mnt_list[receiver_idx];
                // bind mount 可能暴露 shared mount 的子目录。
                // 当该 bind mount 下方的事件到达源挂载的 peer/slave 时，
                // 需要保留源子目录偏移。例如 `dir2`（bind from `dir1/1/2`）
                // 下方的事件应映射到 `dir1/1/2/<event>` 而非 `dir1/<event>`。
                let bind_source_is_below_receiver =
                    mount.is_bind && Self::path_is_at_or_below(&mount.special, &receiver.dir);
                let target = if bind_source_is_below_receiver {
                    let source_relative = mount
                        .special
                        .strip_prefix(receiver.dir.as_str())
                        .unwrap_or("")
                        .trim_start_matches('/');
                    let mapped_relative = if source_relative.is_empty() {
                        String::from(relative)
                    } else if relative.is_empty() {
                        String::from(source_relative)
                    } else {
                        format!("{}/{}", source_relative, relative)
                    };
                    Self::append_relative(&receiver.dir, &mapped_relative)
                } else {
                    Self::append_relative(&receiver.dir, relative)
                };
                if !targets.iter().any(|(known, _)| known == &target) {
                    targets.push((target, receiver_idx));
                }
                pending.push(receiver_idx);
            }
        }
        targets
    }

    /// 将一个已有 mount subtree 移动到 `dir`，并把同一 move event 映射到目标父
    /// 挂载的 shared peer/slave 后代。返回的路径对由 syscall 层用于更新当前
    /// 路径化 VFS 中可观察的目录视图。
    fn move_mount(&mut self, source: &str, dir: &str) -> Result<Vec<(String, String)>, SysErrNo> {
        if self.top_mount_index_at_path(source).is_none() {
            return Err(SysErrNo::EINVAL);
        }
        if Self::path_is_at_or_below(dir, source) {
            return Err(SysErrNo::EINVAL);
        }

        let targets = self
            .top_mount_index_for_path(dir)
            .map(|idx| self.propagation_targets(dir, idx))
            .unwrap_or_else(|| vec![(String::from(dir), usize::MAX)]);
        let subtree: Vec<MountEntry> = self
            .mnt_list
            .iter()
            .filter(|mount| Self::path_is_at_or_below(&mount.dir, source))
            .cloned()
            .collect();
        if subtree.is_empty() {
            return Err(SysErrNo::EINVAL);
        }
        let extra_entries = subtree.len() * targets.len().saturating_sub(1);
        if self.mnt_list.len() + extra_entries > MNT_MAXLEN {
            return Err(SysErrNo::ENOSPC);
        }

        // 移动到 shared/slave 父节点下的 private mount 继承接收者的传播关系。
        // 每个 target 保留一份状态：每个传播副本可能位于不同的接收者之下。
        let receiver_states: Vec<Option<(Option<u64>, Option<u64>, bool)>> = targets
            .iter()
            .map(|(_, idx)| {
                (*idx != usize::MAX).then(|| {
                    let receiver = &self.mnt_list[*idx];
                    (
                        receiver.shared_group,
                        receiver.master_group,
                        receiver.unbindable,
                    )
                })
            })
            .collect();

        // 将原始挂载树移动到第一个 target（始终是 `dir`），然后为 peer/slave
        // 添加携带相同 event group 的副本。event group 的匹配至关重要：
        // 后续 umount 任一副本时，必须移除该原始 mount event 生成的全部实例。
        for mount in &mut self.mnt_list {
            if Self::path_is_at_or_below(&mount.dir, source) {
                let relative = String::from(mount.dir.strip_prefix(source).unwrap_or(""));
                let is_root = relative.is_empty();
                mount.dir = Self::append_relative(dir, relative.trim_start_matches('/'));
                if is_root {
                    if let Some((shared_group, master_group, unbindable)) = receiver_states[0] {
                        mount.shared_group = shared_group;
                        mount.master_group = master_group;
                        mount.unbindable = unbindable;
                    }
                }
            }
        }
        for ((target, _), receiver_state) in
            targets.iter().skip(1).zip(receiver_states.iter().skip(1))
        {
            for mount in &subtree {
                let relative = mount.dir.strip_prefix(source).unwrap_or("");
                let mut copy = mount.clone();
                copy.dir = Self::append_relative(target, relative.trim_start_matches('/'));
                if relative.is_empty() {
                    if let Some((shared_group, master_group, unbindable)) = receiver_state {
                        copy.shared_group = *shared_group;
                        copy.master_group = *master_group;
                        copy.unbindable = *unbindable;
                    }
                }
                self.mnt_list.push(copy);
            }
        }

        Ok(targets
            .into_iter()
            .map(|(target, _)| (String::from(source), target))
            .collect())
    }

    /// 记录一次挂载，并返回路径化 VFS 需要执行的 bind tree 镜像动作。
    ///
    /// 对 propagation-only 操作更新挂载状态，对 remount 更新顶层属性；普通挂载
    /// 创建一层新条目。目标父挂载的 shared peer 与 slave 后代都会收到同一相对路径
    /// 的副本。返回的 `(source, target)` 适用于 `MS_BIND` 与 `MS_MOVE`，由 syscall
    /// 层在表锁外完成目录镜像。
    ///
    /// # 参数说明
    ///
    /// - `special`: mount(2) 的第一个参数，即"源"（设备路径 / bind 源目录 / move 源路径）
    /// - `dir`: mount(2) 的第二个参数，即挂载目标目录
    /// - `fstype`: 文件系统类型字符串（如 "ext4"）
    /// - `flags`: 挂载标志位，包含操作类型（BIND/MOVE/REMOUNT）、传播类型、属性等
    /// - `data`: 挂载选项字符串，当前未使用，仅保留 ABI 兼容
    /// - `capacity`: 可选磁盘容量（字节），仅 ext4 非 bind 挂载时用于创建配额
    ///
    /// # 返回值
    ///
    /// - 普通挂载 / remount / propagation-only → 返回空 `Vec`
    /// - bind mount → 返回 `(source, target)` 列表，供 syscall 层复制目录树
    /// - move mount → 返回 `(old_path, new_path)` 列表
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
        flags: MountFlags,
        data: String,
        capacity: Option<usize>,
    ) -> SysResult<Vec<(String, String)>> {
        // data（挂载选项字符串）在当前路径化 VFS 中不产生实际效果，仅保留以兼容 ABI
        _ = data;

        // ================================================================
        // 第一步：按操作类型分流
        //
        // mount(2) 的 flags 包含一组互斥的操作类型：
        //   MS_MOVE            → 移动已有的挂载树到新位置
        //   propagation-only   → 只修改传播类型（SHARED/PRIVATE/SLAVE/UNBINDABLE）
        //   MS_REMOUNT         → 修改已有挂载点的属性
        //   以上都不是          → 创建新挂载（普通挂载或 MS_BIND）
        // ================================================================

        // ----- MS_MOVE：移动已有挂载子树 -----
        // 将 `special`（源挂载点路径）整棵子树移动到 `dir` 下。
        // 委托给 move_mount()，它会处理传播（shared peer/slave 也得到对应副本）。
        if flags.is_move() {
            return self.move_mount(&special, &dir);
        }

        // ----- 纯传播类型修改 -----
        // 例如 `mount --make-shared /mnt`：不创建新挂载条目，只修改已有挂载的
        // shared_group / master_group / unbindable 字段。
        if flags.is_propagation_only() {
            self.set_propagation(&dir, flags);
            return Ok(Vec::new());
        }

        // ----- MS_REMOUNT：修改已有挂载的属性 -----
        // 例如 `mount -o remount,ro /`：更新已有挂载点的 flags、special、fstype。
        // 注意：remount 不会改变 is_bind 字段 —— 见 MountEntry 设计说明。
        if flags.is_remount() {
            let Some(idx) = self.top_mount_index_at_path(&dir) else {
                return Err(SysErrNo::EINVAL);
            };
            let mount = &mut self.mnt_list[idx];
            mount.special = special;
            mount.fstype = fstype;
            mount.flags = flags;
            return Ok(Vec::new());
        }

        // 至此，必然是创建新挂载的操作（普通挂载或 MS_BIND）

        // ================================================================
        // 第二步：合法性校验
        // ================================================================

        // ----- 校验 1：禁止 bind-clone 一个 unbindable 挂载 -----
        // Linux 语义：如果 bind mount 的 source（special 参数路径）落在某个
        // unbindable 挂载覆盖范围内，则拒绝操作。
        // 实现：先通过 top_mount_index_for_path 找到覆盖 special 路径的挂载索引，
        //       再检查该挂载的 unbindable 字段。
        if flags.is_bind()
            && self
                .top_mount_index_for_path(&special)
                .is_some_and(|idx| self.mnt_list[idx].unbindable)
        {
            return Err(SysErrNo::EINVAL);
        }

        // ----- 校验 2：普通挂载不能遮盖已有挂载点 -----
        // bind mount 允许在同一路径上叠加多层；但普通（非 bind）挂载如果目标路径
        // 已经是一个挂载点，返回 EBUSY，防止意外遮盖已有挂载。
        if !flags.is_bind() && self.top_mount_index_at_path(&dir).is_some() {
            return Err(SysErrNo::EBUSY);
        }

        // ================================================================
        // 第三步：确定传播目标列表
        //
        // 如果目标路径 `dir` 被某个挂载覆盖（即存在"父挂载"），且父挂载是 shared
        // 或 slave 类型的，则新挂载需要"传播"到父挂载的所有 peer/slave。
        //
        // 例如：/mnt 是 shared 挂载，现在在 /mnt/a 下挂载一个新文件系统。
        // /mnt 的所有 peer（如 /peer）也会在 /peer/a 看到这份新挂载。
        // ================================================================

        // 对于 bind mount：找到 special 路径对应的挂载条目（source_entry），
        // 以便后续继承其传播属性（shared_group/master_group）和容量配额。
        let source = flags
            .is_bind()
            .then(|| self.top_mount_index_for_path(&special))
            .flatten()
            .map(|idx| self.mnt_list[idx].clone());

        // 找到覆盖目标路径 `dir` 的父挂载索引（决定新挂载挂在哪棵挂载树下）
        let parent_idx = self.top_mount_index_for_path(&dir);

        // 计算传播目标：
        // - 有父挂载 → 调用 propagation_targets() 得到所有 peer/slave 路径和接收者索引
        // - 无父挂载 → 只有 dir 一个目标，receiver_idx = usize::MAX 表示"无接收者"
        let targets = parent_idx
            .map(|idx| self.propagation_targets(&dir, idx))
            .unwrap_or_else(|| vec![(dir.clone(), usize::MAX)]);

        // ----- 条目数上限检查 -----
        if self.mnt_list.len() + targets.len() > MNT_MAXLEN {
            return Err(SysErrNo::ENOSPC);
        }

        // ================================================================
        // 第四步：创建挂载条目（为每个传播目标各创建一条 MountEntry）
        // ================================================================

        // 分配一个新的 event_group ID。
        // 同一 mount event 产生的所有传播副本共享同一个 event_group，
        // 这样 umount 任一副本时可以根据 event_group 一次性移除全部副本。
        let event_group = self.next_group();

        // 创建容量配额（仅 ext4 且非 bind 挂载时需要）：
        // - bind mount 不创建新配额，而是在下面循环中继承 source 的配额
        //   （因为 bind mount 与源挂载共享同一 ext4 镜像的物理容量）
        // - 可用容量 = capacity / 3：保守估计，为 journal/inode 表等元数据预留空间
        // - 下限 1 MiB：避免容量太小导致立即 ENOSPC
        let quota = (fstype == "ext4" && !flags.is_bind())
            .then(|| capacity.filter(|limit| *limit != 0))
            .flatten()
            .map(|limit| {
                Arc::new(Mutex::new(MountUsage {
                    limit: (limit / 3).max(1024 * 1024),
                    used: 0,
                    files: BTreeMap::new(),
                }))
            });

        // 为每个传播目标创建一条 MountEntry
        for (target, receiver_idx) in &targets {
            // receiver 是接收此传播副本的父挂载条目（如果有的话）
            let receiver = (*receiver_idx != usize::MAX).then(|| &self.mnt_list[*receiver_idx]);

            // ----- 决定传播状态 -----
            // 新挂载的 shared_group / master_group 继承优先级：
            //   1. bind mount → 优先继承 source 挂载的传播状态
            //   2. 否则 → 继承父挂载（receiver）的传播状态
            // 这样在 shared 父挂载下做普通挂载时，新挂载也自动加入同一 shared group。
            let shared_group = source
                .as_ref()
                .and_then(|mount| mount.shared_group)
                .or_else(|| receiver.and_then(|mount| mount.shared_group));
            let master_group = source
                .as_ref()
                .and_then(|mount| mount.master_group)
                .or_else(|| receiver.and_then(|mount| mount.master_group));

            // 将新条目插入挂载表
            self.mnt_list.push(MountEntry {
                special: special.clone(),   // 源设备路径 / bind 源路径
                dir: target.clone(),        // 挂载目标路径（传播后的路径）
                fstype: fstype.clone(),     // 文件系统类型
                flags,                      // 挂载标志位
                is_bind: flags.is_bind(),   // 是否为 bind mount
                shared_group,               // 所属 shared peer group
                master_group,               // 所属 master（slave 的上游）
                unbindable: false,          // 新挂载默认不是 unbindable
                event_group,                // 同一 mount event 的标识
                quota: quota
                    .clone()
                    // bind mount：继承 source 的配额（共享同一 ext4 镜像容量）
                    .or_else(|| source.as_ref().and_then(|mount| mount.quota.clone())),
            });
        }

        // ================================================================
        // 第五步：返回结果
        // ================================================================

        // 非 bind 挂载：不需要复制目录树，直接返回空列表
        if !flags.is_bind() {
            return Ok(Vec::new());
        }

        // bind mount：返回 (source, target) 对列表，供 syscall 层在表锁外
        // 执行目录树镜像（将 source 的 dentry 树复制到每个 target 路径）。
        // 过滤掉 target == source 的情况（bind 自己到自己无意义）。
        Ok(targets
            .into_iter()
            .filter(|(target, _)| target != &special)
            .map(|(target, _)| (special.clone(), target))
            .collect())
    }

    /// 为覆盖 `path` 的 ext4 挂载预留文件增长的逻辑空间。
    ///
    /// 如果 `path` 不在 ext4 挂载下，直接放行（无配额限制）。
    /// 超出容量上限时返回 `ENOSPC`。
    pub fn reserve_write(
        &mut self,
        path: &str,
        old_size: usize,
        new_size: usize,
    ) -> Result<(), SysErrNo> {
        let Some(idx) = self.top_mount_index_for_path(path) else {
            return Ok(());
        };
        let Some(quota) = self.mnt_list[idx].quota.clone() else {
            return Ok(());
        };
        let mut usage = quota.lock();
        let current = usage.files.get(path).copied().unwrap_or_else(|| {
            usage.used = usage.used.saturating_add(old_size);
            old_size
        });
        if new_size <= current {
            usage.files.entry(String::from(path)).or_insert(current);
            return Ok(());
        }
        let growth = new_size - current;
        if growth > usage.limit.saturating_sub(usage.used) {
            return Err(SysErrNo::ENOSPC);
        }
        usage.used += growth;
        usage.files.insert(String::from(path), new_size);
        Ok(())
    }

    /// 当底层 inode 写入失败时，回滚刚刚扩大的预留空间。
    ///
    /// `previous_size` 可能已经包含之前的 chunk 预留。
    pub fn rollback_reservation(&mut self, path: &str, previous_size: usize, new_size: usize) {
        let Some(idx) = self.top_mount_index_for_path(path) else {
            return;
        };
        let Some(quota) = self.mnt_list[idx].quota.clone() else {
            return;
        };
        let mut usage = quota.lock();
        if let Some(current) = usage.files.get(path).copied() {
            let rollback = new_size
                .saturating_sub(previous_size)
                .min(current.saturating_sub(previous_size));
            usage.used = usage.used.saturating_sub(rollback);
            if current == new_size {
                usage.files.insert(String::from(path), previous_size);
            }
        }
    }

    /// 当 inode 路径名被删除后，释放对应的逻辑空间。
    pub fn remove_file(&mut self, path: &str) {
        let Some(idx) = self.top_mount_index_for_path(path) else {
            return;
        };
        let Some(quota) = self.mnt_list[idx].quota.clone() else {
            return;
        };
        let mut usage = quota.lock();
        if let Some(size) = usage.files.remove(path) {
            usage.used = usage.used.saturating_sub(size);
        }
    }

    /// 查询精确挂载点的可见顶层，并复制返回 `(source, dir, fstype, flags)`。
    ///
    /// 若该路径没有挂载层，返回 `None`。更早叠加在同一路径上的挂载不会被返回。
    pub fn got_mount(&mut self, dir: String) -> Option<(String, String, String, MountFlags)> {
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
    pub fn mount_for_path(&self, path: &str) -> Option<(String, String, String, MountFlags)> {
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
            let opts = if mount.flags.contains(MountFlags::RDONLY) { "ro" } else { "rw" };
            // bind mount 不能将目标路径暴露为设备名。这个路径化 VFS 没有保留
            // 后端设备标识，因此使用一个稳定的、非路径的占位符。否则 BusyBox
            // umount 会把 self-bind 叠加层视为同一设备的别名，一次性弹出多层。
            let source = if mount.is_bind {
                "none"
            } else {
                mount.special.as_str()
            };
            content.push_str(&format!(
                "{} {} {} {} 0 0\n",
                source, mount.dir, mount.fstype, opts
            ));
        }
        content
    }

    /// 卸载 `dir` 的顶层及同一 mount event 创建的所有 peer 副本。
    ///
    /// 成功返回 0；路径没有顶层挂载时返回 -1。卸载后，保留在同一路径下的更早
    /// 挂载层重新可见。`flags` 当前只保留 ABI 入口，尚不影响卸载策略。
    pub fn umount(&mut self, dir: String, flags: MountFlags) -> isize {
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
