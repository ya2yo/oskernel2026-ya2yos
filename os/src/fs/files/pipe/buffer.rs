//! 管道数据片段及其底层存储的统一表示。
//!
//! 普通写入使用 `Bytes`，文件页缓存路径使用 `FilePage`。片段只保存底层
//! 存储的窗口（偏移和长度），因此 `splice` 可以移动窗口，`tee` 可以克隆
//! `Arc` 和元数据而不复制实际字节。

use crate::fs::FilePage;
use alloc::sync::Arc;
use alloc::vec::Vec;

/// pipe 中的一个连续数据片段。
/// offset/len 描述当前片段在底层存储中的窗口，split_to 可以只切出头部而不复制数据。
#[derive(Clone)]
pub(super) struct PipeBuf {
    pub(super) storage: PipeBufStorage,
    pub(super) offset: usize,
    pub(super) len: usize,
}

/// PipeBuf 的底层存储来源。
/// Bytes 来自普通 write，FilePage 来自 page cache；二者都通过 Arc 支持 splice/tee 的引用移动/复制。
#[derive(Clone)]
pub(super) enum PipeBufStorage {
    Bytes(Arc<Vec<u8>>),
    FilePage(Arc<FilePage>),
}

impl PipeBuf {
    pub(super) fn new(bytes: Vec<u8>) -> Self {
        let len = bytes.len();
        Self {
            storage: PipeBufStorage::Bytes(Arc::new(bytes)),
            offset: 0,
            len,
        }
    }

    pub(super) fn from_file_page(page: Arc<FilePage>, offset: usize, len: usize) -> Self {
        Self {
            storage: PipeBufStorage::FilePage(page),
            offset,
            len,
        }
    }

    pub(super) fn split_to(&mut self, len: usize) -> Self {
        let len = len.min(self.len);
        let buf = Self {
            storage: self.storage.clone(),
            offset: self.offset,
            len,
        };
        self.offset += len;
        self.len -= len;
        buf
    }

    /// 读 pipe 时统一把片段转换成字节切片；FilePage 分支直接访问页帧内容。
    pub(super) fn as_slice(&self) -> &[u8] {
        match &self.storage {
            PipeBufStorage::Bytes(data) => &data[self.offset..self.offset + self.len],
            PipeBufStorage::FilePage(page) => {
                let bytes = page.frame.ppn.bytes_array();
                &bytes[self.offset..self.offset + self.len]
            }
        }
    }
}
