PLATFORM := qemu
MEMORY_SIZE := 128M  # 内存地址同时在os中的memory_layout等多处都定义了，这里修改只是修改了qemu模拟的内存大小
SMP := 1  # CPU核心数
MODE := release

ARCH := riscv64

TARGET := riscv64gc-unknown-none-elf

DISK_IMG := ./2026_testsuits_img/pre_tests/sdcard-rv.img
# DISK_IMG := ./2026_testsuits_img/individual_tests/sdcard-rv.img


KERNEL_ELF := $(PROJECT_ROOT)/os/target/$(TARGET)/$(MODE)/os
KERNEL_BIN := kernel-rv

KERNEL_BUILD_ARGS := --$(MODE) --features "$(ARCH)" --target $(TARGET)

QEMU_CMD := qemu-system-riscv64 \
    -machine virt \
    -kernel $(KERNEL_BIN) \
    -m $(MEMORY_SIZE) \
    -nographic \
    -smp $(SMP) \
    -bios default \
    -drive file=disk.img,if=none,format=raw,id=x0 \
    -device virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0 \
    -device virtio-net-device,netdev=net,bus=virtio-mmio-bus.1 \
    -netdev user,id=net \
	-snapshot
# -snapshot是为了避免修改被保存仅镜像

OBJDUMP := rust-objdump --arch-name=$(ARCH)
OBJCOPY := rust-objcopy --binary-architecture=$(ARCH)

GDB_TOOL := gdb-multiarch

export PLATFORM MEMORY_SIZE SMP MODE
export ARCH PLATFORM TARGET DISK_IMG KERNEL_ELF KERNEL_BIN
export KERNEL_BUILD_ARGS QEMU_CMD OBJDUMP OBJCOPY GDB_TOOL