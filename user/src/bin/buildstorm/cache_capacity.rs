//! Directed reproduction for file-page-cache capacity bypass during mmap.
//!
//! Build this case with the kernel-only `file-cache-capacity-test` feature.
//! The feature reduces the cache limit to 2,048 pages, so the test reaches the
//! same capacity-bypass path as the long BuildStorm run in seconds.

use super::common;

const SCRIPT_PATH: &str = "/tmp/buildstorm-cache-capacity.sh\0";
const SCRIPT_BODY: &str = r#"#!/bin/sh
mount -t proc proc /proc 2>/dev/null
mount -t sysfs sysfs /sys 2>/dev/null
mount -t devtmpfs devtmpfs /dev 2>/dev/null

# These two regular files are each below the per-file cache-read ceiling and
# total more than the 8 MiB directed-test capacity. Ordinary reads therefore
# fill the cache, whereas a later mmap must use a capacity-bypassed page.
for file in \
    /root/.rustup/toolchains/nightly-2026-05-28-riscv64gc-unknown-linux-gnu/bin/cargo-fmt \
    /root/.rustup/toolchains/nightly-2026-05-28-riscv64gc-unknown-linux-gnu/bin/rustfmt
do
    cat "$file" >/dev/null || {
        echo "BUILDSTORM_DEBUG_CACHE_CAPACITY fail stage=fill"
        exit 1
    }
done

# `tail` was not executed while populating the cache. Before the fix, its
# dynamic loader/libc mappings faulted after the capacity bypass and SIGSEGV.
if /usr/bin/tail --version >/dev/null 2>&1; then
    echo "BUILDSTORM_DEBUG_CACHE_CAPACITY ok"
    exit 0
fi

echo "BUILDSTORM_DEBUG_CACHE_CAPACITY fail stage=exec"
exit 1
"#;

#[allow(dead_code)]
pub fn run() -> i32 {
    common::run_case("cache-capacity", SCRIPT_PATH, SCRIPT_BODY)
}
