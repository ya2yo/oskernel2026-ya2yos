use alloc::{format, string::String, sync::Arc, vec::Vec};
use linux_raw_sys::general::*;
use spin::{Lazy, Mutex};

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
    // All copies made for one mount event are removed together by umount.
    event_group: u64,
}

pub struct MountTable {
    mnt_list: Vec<MountEntry>,
    next_group: u64,
}

impl MountTable {
    fn next_group(&mut self) -> u64 {
        let group = self.next_group;
        self.next_group = self.next_group.wrapping_add(1).max(1);
        group
    }

    fn path_is_at_or_below(path: &str, base: &str) -> bool {
        base == "/"
            || path == base
            || path
                .strip_prefix(base)
                .map_or(false, |rest| rest.starts_with('/'))
    }

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

    fn top_mount_index_for_path(&self, path: &str) -> Option<usize> {
        self.mnt_list
            .iter()
            .enumerate()
            .filter(|(_, mount)| Self::path_is_at_or_below(path, &mount.dir))
            .max_by_key(|(idx, mount)| (mount.dir.len(), *idx))
            .map(|(idx, _)| idx)
    }

    fn top_mount_index_at_path(&self, path: &str) -> Option<usize> {
        self.mnt_list
            .iter()
            .enumerate()
            .rev()
            .find_map(|(idx, mount)| (mount.dir == path).then_some(idx))
    }

    fn set_propagation(&mut self, dir: &str, flags: u32) {
        let recursive = flags & MS_REC != 0;
        let group = (flags & MS_SHARED != 0).then(|| self.next_group());
        for mount in &mut self.mnt_list {
            if mount.dir == dir || (recursive && Self::path_is_at_or_below(&mount.dir, dir)) {
                mount.shared_group = group;
            }
        }
    }

    /// Records a mount and returns the physical tree copies required by the
    /// current path-based VFS model. A full mount-root VFS is outside this
    /// table; mirroring bind sources keeps mount propagation observable to
    /// pathname lookup and LTP's directory comparisons.
    pub fn mount(
        &mut self,
        special: String,
        dir: String,
        fstype: String,
        flags: u32,
        data: String,
    ) -> Result<Vec<(String, String)>, ()> {
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

        let source_group = self
            .top_mount_index_for_path(&special)
            .and_then(|idx| self.mnt_list[idx].shared_group);
        let exact_target_group = self
            .top_mount_index_at_path(&dir)
            .and_then(|idx| self.mnt_list[idx].shared_group);
        let parent_idx = self.top_mount_index_for_path(&dir);
        let parent = parent_idx.map(|idx| self.mnt_list[idx].clone());
        let inherited_group = source_group
            .or(exact_target_group)
            .or_else(|| parent.as_ref().and_then(|mount| mount.shared_group));

        let mut targets = Vec::new();
        targets.push(dir.clone());
        if let Some(parent) = parent {
            if let Some(group) = parent.shared_group {
                let relative = dir
                    .strip_prefix(parent.dir.as_str())
                    .unwrap_or("")
                    .trim_start_matches('/');
                for (idx, peer) in self.mnt_list.iter().enumerate() {
                    if peer.shared_group != Some(group)
                        || self.top_mount_index_at_path(&peer.dir) != Some(idx)
                    {
                        continue;
                    }
                    let peer_target = Self::append_relative(&peer.dir, relative);
                    if !targets.iter().any(|target| target == &peer_target) {
                        targets.push(peer_target);
                    }
                }
            }
        }

        if self.mnt_list.len() + targets.len() > MNT_MAXLEN {
            return Err(());
        }
        let event_group = self.next_group();
        for target in &targets {
            self.mnt_list.push(MountEntry {
                special: special.clone(),
                dir: target.clone(),
                fstype: fstype.clone(),
                flags,
                shared_group: inherited_group,
                event_group,
            });
        }

        if flags & MS_BIND == 0 {
            return Ok(Vec::new());
        }
        Ok(targets
            .into_iter()
            .filter(|target| target != &special)
            .map(|target| (special.clone(), target))
            .collect())
    }

    /// Queries an exact mount point, returning the visible (topmost) layer.
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

    /// Finds the topmost mount with the longest matching path prefix.
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

    /// Removes only the top layer and its peer copies created by the same
    /// mount event. Earlier layers at a path become visible again.
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
