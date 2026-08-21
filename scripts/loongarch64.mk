PLATFORM ?= qemu
MODE := release

ARCH := loongarch64
TARGET := loongarch64-unknown-none
# DISK_IMG := ./2026_testsuits_img/pre_tests/sdcard-la.img
# DISK_IMG := ./2026_testsuits_img/final-2026/sdcard-la.img
DISK_IMG := 2026_testsuits_img/onsite-2026/sdcard-la.img
DISK_IMG := 2026_testsuits_img/onsite-2026/la.img

ifeq ($(PLATFORM),2k1000)
MEMORY_SIZE := 1G
SMP := 2
KERNEL_PLATFORM_FEATURES := 2k1000
KERNEL_RAW_BIN := kernel-la.bin
KERNEL_LOAD_ADDR := 0x9000000090000000
KERNEL_ENTRY_ADDR := 0x9000000090000000
else ifeq ($(PLATFORM),qemu)
MEMORY_SIZE := 36G  # 与 os/src/arch/loongarch64/qemu/memory_layout.rs 同步
SMP := 12  # 与 os/src/arch/loongarch64/qemu/config.rs 同步
KERNEL_PLATFORM_FEATURES :=
KERNEL_RAW_BIN :=
KERNEL_LOAD_ADDR :=
KERNEL_ENTRY_ADDR :=
else
$(error Unsupported LoongArch64 PLATFORM: $(PLATFORM), expected qemu or 2k1000)
endif

KERNEL_ELF := $(if $(CARGO_TARGET_DIR),$(CARGO_TARGET_DIR),$(PROJECT_ROOT)/os/target)/$(TARGET)/$(MODE)/os
KERNEL_BIN := kernel-la

KERNEL_BUILD_ARGS := --$(MODE) --target $(TARGET)

ifneq ($(PLATFORM),2k1000)
QEMU_CMD := qemu-system-loongarch64 \
    -kernel $(KERNEL_BIN) \
    -m $(MEMORY_SIZE) \
    -nographic \
    -smp $(SMP) \
    -drive file=disk.img,if=none,format=raw,id=x0 \
    -device virtio-blk-pci,drive=x0 \
    -no-reboot \
    -device virtio-net-pci,netdev=net0 \
    -netdev user,id=net0 \
    -rtc base=utc \
	-snapshot #-d in_asm,cpu -D log.txt
endif
# 暂时移除 -snapshot,保留前期构建的结果
# -snapshot是为了避免修改被保存仅镜像
OBJDUMP := rust-objdump --arch-name=$(ARCH)
OBJCOPY := rust-objcopy --binary-architecture=$(ARCH)

GDB_TOOL := /opt/gdb-15.2/bin/loongarch64-unknown-elf-gdb

export PLATFORM MEMORY_SIZE SMP MODE
export ARCH PLATFORM TARGET DISK_IMG KERNEL_ELF KERNEL_BIN
export KERNEL_BUILD_ARGS QEMU_CMD OBJDUMP OBJCOPY GDB_TOOL
export KERNEL_PLATFORM_FEATURES KERNEL_RAW_BIN KERNEL_LOAD_ADDR KERNEL_ENTRY_ADDR
