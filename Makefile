#设置内核和用户程序的编译环境

# riscv64
# loongarch64
TARGET_ARCH := riscv64
# TARGET_ARCH := loongarch64

all: riscv64-build loongarch64-build
# all: $(TARGET_ARCH)-build

export PROJECT_ROOT := $(CURDIR)

ifeq ($(TARGET_ARCH), riscv64)
	include scripts/riscv64.mk
else ifeq ($(TARGET_ARCH), loongarch64)
	include scripts/loongarch64.mk
else ifeq ($(TARGET_ARCH), visionfive2)
	include scripts/visionfive2.mk
else
	$(error Unsupported TARGET_ARCH: $(TARGET_ARCH))
endif

include scripts/user.mk


riscv64-build: 
	@echo "=========================================="
	@echo "Building for RISCV64 architecture..."
	@echo "=========================================="
	@$(MAKE) build-arch TARGET_ARCH=riscv64

loongarch64-build:
	@echo "=========================================="
	@echo "Building for LoongArch64 architecture..."
	@echo "=========================================="
	@$(MAKE) build-arch TARGET_ARCH=loongarch64

# 内部目标：为指定架构编译
build-arch: set_env_arch
	@echo "Kernel build arguments: $(KERNEL_BUILD_ARGS)"
	@$(MAKE) setup_cargo
	@echo "Building user programs for $(TARGET_ARCH)..."
	@cd ./user && $(MAKE) build
	@echo "------------------------------------------- user programs built successfully"
	@echo "Building kernel for $(TARGET_ARCH)..."
	@cd ./os && $(MAKE) build KERNEL_OUTPUT_LOG_LEVEL=warn
	@$(MAKE) cleanup_cargo
	@echo "$(TARGET_ARCH) build completed successfully!"

# 为特定架构设置环境
set_env_arch:
ifeq ($(TARGET_ARCH), riscv64)
	$(eval include scripts/riscv64.mk)
	$(eval include scripts/user.mk)
else ifeq ($(TARGET_ARCH), loongarch64)
	$(eval include scripts/loongarch64.mk)
	$(eval include scripts/user.mk)
else ifeq ($(TARGET_ARCH), visionfive2)
	$(eval include scripts/visionfive2.mk)
	$(eval include scripts/user.mk)
else
	$(error Unsupported TARGET_ARCH: $(TARGET_ARCH))
endif
	@(rustup target list | grep "${TARGET} (installed)") || rustup target add $(TARGET)
	@rustup component add rust-src
	@rustup component add llvm-tools-preview

special_make:set_env
	@echo "Kernel build arguments: $(KERNEL_BUILD_ARGS)"
	@$(MAKE) setup_cargo
	@echo "Building user programs..."
	@cd ./user && $(MAKE) build
	@echo "------------------------------------------- user programs built successfully"
	@echo "Building kernel..."
	@cd ./os && $(MAKE) build KERNEL_OUTPUT_LOG_LEVEL=warn
	@$(MAKE) cleanup_cargo

log: set_env
	@$(MAKE) setup_cargo
	@cd ./user && $(MAKE) build
	@cd ./os && $(MAKE) build KERNEL_OUTPUT_LOG_LEVEL=debug
	@$(MAKE) cleanup_cargo

# 构建带内核性能统计埋点的版本
perf: set_env
	@$(MAKE) setup_cargo
	@cd ./user && $(MAKE) build
	@cd ./os && $(MAKE) build KERNEL_EXTRA_FEATURES=perf
	@$(MAKE) cleanup_cargo

# 仅生成内核 crate 的 rustdoc；依赖仍会参与类型检查，但不生成其文档页面。
doc: set_env_arch
	@$(MAKE) setup_cargo
	@echo "Generating os documentation for $(TARGET_ARCH) ($(TARGET))..."
	@cd ./os && cargo doc --no-deps --target $(TARGET)
	@$(MAKE) cleanup_cargo
	@echo "os documentation: os/target/$(TARGET)/doc/os/index.html"

# 注意，make run会创建一个临时软链接
run:
	@rm -f disk.img
	@ln -s $(DISK_IMG) ./disk.img
	@-$(QEMU_CMD)
	@rm -f disk.img

clean:
	@cd ./os && $(MAKE) clean
	@cd ./user && $(MAKE) clean

objdump:
	@${OBJDUMP} -d -S $(KERNEL_ELF) > $(KERNEL_BIN).dump 2>/dev/null || true

gdbserver: build-arch
	@rm -f disk.img
	@echo "Starting GDB server..."
	@ln -s $(DISK_IMG) ./disk.img
	@-$(QEMU_CMD) -s -S
	@rm disk.img

gdbclient:
	@$(GDB_TOOL) $(KERNEL_ELF) \
		-ex 'set logging file client.ans' \
		-ex 'set logging overwrite on' \
		-ex 'set logging enabled on' \
        -ex 'target remote localhost:1234' \
		-ex 'b os::lang_items::panic' 

gdb:
	@tmux kill-session -t os-debug 2>/dev/null || true
	@tmux new-session -d -s os-debug '$(MAKE) TARGET_ARCH=$(TARGET_ARCH) gdbserver'
	@tmux split-window -h 'sleep 1 && $(MAKE) TARGET_ARCH=$(TARGET_ARCH) gdbclient'
	@tmux attach-session -t os-debug

setup_cargo:
	-@cd ./os && mkdir -p .cargo && if [ "$(PLATFORM)" = "visionfive2" ]; then cp -f dotcargo/config-visionfive2 .cargo/config; else cp -f dotcargo/config .cargo/; fi
	-@cd ./user && mkdir -p .cargo && cp -f dotcargo/config .cargo/

cleanup_cargo:
	-@cd ./os && rm -rf .cargo
	-@cd ./user && rm -rf .cargo

set_env:
	@(rustup target list | grep "${TARGET} (installed)") || rustup target add $(TARGET)
	@rustup component add rust-src
	@rustup component add llvm-tools-preview
# 下面这个命令要求你的docker image里面有一个名字叫 zhouzhouyi/os-contest:20260510
docker:
	docker run --rm -it -v $(PROJECT_ROOT):/workplace -w /workplace zhouzhouyi/os-contest:20260510 bash

.PHONY: all all-arch riscv64-build loongarch64-build build-arch set_env_arch \
        run log perf doc clean objdump gdbserver gdbclient gdb setup_cargo cleanup_cargo set_env

.DEFAULT_GOAL := all
