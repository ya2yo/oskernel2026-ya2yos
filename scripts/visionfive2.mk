PLATFORM := visionfive2
MEMORY_SIZE := 2G
SMP := 4
MODE := release
ARCH := riscv64
TARGET := riscv64gc-unknown-none-elf
DISK_IMG ?= ./2026_testsuits_img/final-2026/sdcard-rv.img
KERNEL_ELF := $(PROJECT_ROOT)/os/target/$(TARGET)/$(MODE)/os
KERNEL_BIN := kernel-vf2
KERNEL_BUILD_ARGS := --$(MODE) --target $(TARGET)
KERNEL_EXTRA_FEATURES := visionfive2
OBJDUMP := rust-objdump --arch-name=$(ARCH)
OBJCOPY := rust-objcopy --binary-architecture=$(ARCH)
GDB_TOOL := gdb-multiarch
export PLATFORM MEMORY_SIZE SMP MODE ARCH TARGET DISK_IMG KERNEL_ELF KERNEL_BIN
export KERNEL_BUILD_ARGS KERNEL_EXTRA_FEATURES OBJDUMP OBJCOPY GDB_TOOL
export VISIONFIVE2_LINKER := src/arch/riscv64/qemu/visionfive2.ld
