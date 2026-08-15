PLATFORM ?= qemu
MODE := release

ARCH := riscv64
TARGET := riscv64gc-unknown-none-elf
DISK_IMG ?= ./2026_testsuits_img/pre_tests/sdcard-rv.img
# DISK_IMG ?= ./2026_testsuits_img/final-2026/sdcard-rv.img

ifeq ($(PLATFORM),visionfive2)
MEMORY_SIZE := 2G
SMP := 4
KERNEL_BIN := kernel-vf2
KERNEL_PLATFORM_FEATURES := visionfive2
else ifeq ($(PLATFORM),qemu)
MEMORY_SIZE := 16G  # 修改时同步 os/src/arch/riscv64/qemu/memory_layout.rs
SMP := 8  # CPU核心数
KERNEL_BIN := kernel-rv
KERNEL_PLATFORM_FEATURES :=
else
$(error Unsupported RISC-V PLATFORM: $(PLATFORM), expected qemu or visionfive2)
endif


KERNEL_ELF := $(if $(CARGO_TARGET_DIR),$(CARGO_TARGET_DIR),$(PROJECT_ROOT)/os/target)/$(TARGET)/$(MODE)/os

KERNEL_BUILD_ARGS := --$(MODE) --target $(TARGET)

ifeq ($(PLATFORM),qemu)
QEMU_CMD := qemu-system-riscv64 \
    -machine virt \
    -kernel $(KERNEL_BIN) \
    -m $(MEMORY_SIZE) \
    -nographic \
    -smp $(SMP) \
    -bios default \
    -drive file=disk.img,if=none,format=raw,id=x0 \
    -device virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0 \
    -no-reboot \
    -device virtio-net-device,netdev=net \
    -netdev user,id=net \
    -rtc base=utc \
	-snapshot
endif
# Keep guest filesystem changes so final-2026's in-guest build artifacts can
# be reused by subsequent runs instead of rebuilding the project every time.

OBJDUMP := rust-objdump --arch-name=$(ARCH)
OBJCOPY := rust-objcopy --binary-architecture=$(ARCH)

GDB_TOOL := gdb-multiarch

export PLATFORM MEMORY_SIZE SMP MODE
export ARCH PLATFORM TARGET DISK_IMG KERNEL_ELF KERNEL_BIN
export KERNEL_BUILD_ARGS QEMU_CMD OBJDUMP OBJCOPY GDB_TOOL
export KERNEL_PLATFORM_FEATURES
