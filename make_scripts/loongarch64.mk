PLATFORM := qemu
MEMORY_SIZE := 128M  # 0x8000000
SMP := 1  # CPU核心数
MODE := release

ARCH := loongarch64

TARGET := loongarch64-unknown-none

DISK_IMG := ./2026_testsuits_img/pre_tests/sdcard-la.img

KERNEL_ELF := $(PROJECT_ROOT)/os/target/$(TARGET)/$(MODE)/os
KERNEL_BIN := kernel-la

KERNEL_BUILD_ARGS := --$(MODE) --target $(TARGET)

# 注释掉的qemu命令是评测是使用的命令，二者区别在于bus参数
# 不知道什么原因bus参数报错：qemu-system-loongarch64: -device virtio-blk-pci,drive=x0,bus=virtio-mmio-bus.0: Bus 'virtio-mmio-bus.0' not found
# 暂时不带bus参数

# QEMU_CMD := qemu-system-loongarch64 \
#     -kernel $(KERNEL_BIN) \
#     -m $(MEMORY_SIZE) \
#     -nographic \
#     -smp $(SMP) \
#     -drive file=$(DISK_IMG),if=none,format=raw,id=x0 \
#     -device virtio-blk-pci,drive=x0,bus=virtio-mmio-bus.0 \
#     -no-reboot \
#     -device virtio-net-pci,netdev=net0 \
#     -netdev user,id=net0,hostfwd=tcp::5555-:5555,hostfwd=udp::5555-:5555 \
#     -rtc base=utc \

QEMU_CMD := qemu-system-loongarch64 \
    -kernel $(KERNEL_BIN) \
    -m $(MEMORY_SIZE) \
    -nographic \
    -smp $(SMP) \
    -drive file=$(DISK_IMG),if=none,format=raw,id=x0 \
    -device virtio-blk-pci,drive=x0 \
    -no-reboot \
    -device virtio-net-pci,netdev=net0 \
    -netdev user,id=net0,hostfwd=tcp::5555-:5555,hostfwd=udp::5555-:5555 \
    -rtc base=utc \
	-snapshot #-d in_asm,cpu -D log.txt
# -snapshot是为了避免修改被保存仅镜像
OBJDUMP := rust-objdump --arch-name=$(ARCH)
OBJCOPY := rust-objcopy --binary-architecture=$(ARCH)

GDB_TOOL := gdb-multiarch

export PLATFORM MEMORY_SIZE SMP MODE
export ARCH PLATFORM TARGET DISK_IMG KERNEL_ELF KERNEL_BIN
export KERNEL_BUILD_ARGS QEMU_CMD OBJDUMP OBJCOPY GDB_TOOL