//! 启动期从根文件系统读取的运行时内核配置。
//!
//! 配置文件是简单的 `key=value` 文本。它只调整已经支持运行时变更的
//! 资源水位，不参与早期硬件探测或块设备初始化。

use alloc::{
    string::{String, ToString},
    vec,
};
use log::warn;

use super::{open, File, OpenFlags, DEFAULT_FILE_MODE};
use crate::{
    fs::pipe,
    mm::{UserBuffer, PAGE_CACHE},
    utils::{SysErrNo, SysResult},
};

const CONFIG_PATH: &str = "/etc/ya2yos.conf";
const MAX_CONFIG_SIZE: usize = 16 * 1024;

fn parse_usize(value: &str) -> Option<usize> {
    if value.is_empty() {
        return None;
    }
    let mut result = 0usize;
    for byte in value.bytes() {
        if !byte.is_ascii_digit() {
            return None;
        }
        result = result
            .checked_mul(10)?
            .checked_add((byte - b'0') as usize)?;
    }
    Some(result)
}

fn refresh_pipe_proc_file() -> SysResult {
    let file = open(
        "/proc/sys/fs/pipe-max-size",
        OpenFlags::O_RDWR,
        DEFAULT_FILE_MODE,
    )?
    .file()?;
    let mut content = String::from("");
    content.push_str(&pipe::pipe_max_size().to_string());
    content.push('\n');
    let content_len = content.len();
    let bytes = unsafe { content.as_bytes_mut() };
    let buffer = unsafe { core::slice::from_raw_parts_mut(bytes.as_mut_ptr(), bytes.len()) };
    file.set_offset(0);
    file.write(UserBuffer::new(vec![buffer]))?;
    file.inode.truncate(content_len)?;
    file.inode.sync();
    Ok(())
}

pub fn load_sdcard_config() -> SysResult {
    let file = match open(CONFIG_PATH, OpenFlags::O_RDONLY, 0) {
        Ok(file) => file.file()?,
        Err(SysErrNo::ENOENT) => return Ok(()),
        Err(err) => return Err(err),
    };
    let bytes = file.inode.read_all()?;
    if bytes.len() > MAX_CONFIG_SIZE {
        warn!(
            "config: {} is too large ({} bytes), ignored",
            CONFIG_PATH,
            bytes.len()
        );
        return Ok(());
    }

    let (mut high, mut low, mut refill, mut flush) = PAGE_CACHE.config();
    let mut page_cache_changed = false;
    let mut pipe_changed = false;
    for raw_line in bytes.split(|byte| *byte == b'\n' || *byte == b'\r') {
        let Ok(line) = core::str::from_utf8(raw_line) else {
            warn!("config: invalid UTF-8 line, ignored");
            continue;
        };
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            warn!("config: malformed line {:?}", line);
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        let Some(value) = parse_usize(value) else {
            warn!("config: invalid numeric value for {}", key);
            continue;
        };
        match key {
            "pipe_max_size" => {
                if pipe::set_pipe_max_size(value).is_err() {
                    warn!("config: invalid pipe_max_size={}", value);
                } else {
                    pipe_changed = true;
                }
            }
            "page_cache_high_watermark" => {
                high = value;
                page_cache_changed = true;
            }
            "page_cache_low_watermark" => {
                low = value;
                page_cache_changed = true;
            }
            "page_cache_refill_batch" => {
                refill = value;
                page_cache_changed = true;
            }
            "page_cache_flush_batch" => {
                flush = value;
                page_cache_changed = true;
            }
            _ => warn!("config: unknown key {}", key),
        }
    }

    if page_cache_changed {
        if PAGE_CACHE.configure(high, low, refill, flush).is_err() {
            warn!(
                "config: invalid page cache settings high={}, low={}, refill={}, flush={}",
                high, low, refill, flush
            );
        }
    }
    if pipe_changed {
        refresh_pipe_proc_file()?;
    }
    let (effective_high, effective_low, effective_refill, effective_flush) = PAGE_CACHE.config();
    println!(
        "[kernel] config: loaded {} (pipe_max_size={}, page_cache={}/{}/{}/{})",
        CONFIG_PATH,
        pipe::pipe_max_size(),
        effective_high,
        effective_low,
        effective_refill,
        effective_flush
    );
    Ok(())
}
