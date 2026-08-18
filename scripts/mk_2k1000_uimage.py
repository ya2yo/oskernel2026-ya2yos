#!/usr/bin/env python3
"""Build the vendor U-Boot Legacy image used by the 2K1000 board."""

from __future__ import annotations

import pathlib
import struct
import sys
import time
import zlib


MAGIC = 0x27051956
LOAD_ADDRESS = 0x90000000
ENTRY_ADDRESS = 0x90000000
OS_LINUX = 5
ARCH_LOONGARCH = 0x1B
TYPE_KERNEL = 2
COMP_NONE = 0
HEADER_FORMAT = ">7I4B32s"


def make_header(data: bytes) -> bytes:
    data_crc = zlib.crc32(data) & 0xFFFFFFFF
    name = b"Ya2yOS 2K1000".ljust(32, b"\0")
    timestamp = int(time.time())
    header = struct.pack(
        HEADER_FORMAT,
        MAGIC,
        0,
        timestamp,
        len(data),
        LOAD_ADDRESS,
        ENTRY_ADDRESS,
        data_crc,
        OS_LINUX,
        ARCH_LOONGARCH,
        TYPE_KERNEL,
        COMP_NONE,
        name,
    )
    header_crc = zlib.crc32(header) & 0xFFFFFFFF
    return struct.pack(
        HEADER_FORMAT,
        MAGIC,
        header_crc,
        timestamp,
        len(data),
        LOAD_ADDRESS,
        ENTRY_ADDRESS,
        data_crc,
        OS_LINUX,
        ARCH_LOONGARCH,
        TYPE_KERNEL,
        COMP_NONE,
        name,
    )


def main() -> int:
    root = pathlib.Path(__file__).resolve().parent.parent
    raw_path = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else root / "kernel-la.bin"
    out_path = pathlib.Path(sys.argv[2]) if len(sys.argv) > 2 else root / "kernel-la.uImage"
    data = raw_path.read_bytes()
    out_path.write_bytes(make_header(data) + data)
    print(f"Created {out_path} ({out_path.stat().st_size} bytes)")
    print(f"  data size: {len(data)} bytes")
    print(f"  load/entry: 0x{LOAD_ADDRESS:08x}")
    print(f"  data CRC32: {zlib.crc32(data) & 0xFFFFFFFF:08x}")
    print(f"  arch: LoongArch (0x{ARCH_LOONGARCH:02x})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
