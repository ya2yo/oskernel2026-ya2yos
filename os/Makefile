# 这个Makefile用于编译内核
# 该Makefile必须被上层Makefile调用

build: $(KERNEL_BIN)

SCHEDULER ?= cfs

ifeq ($(SCHEDULER),rr)
SCHEDULER_BUILD_ARGS := --no-default-features
SCHEDULER_FEATURE := net,scheduler-rr
else ifeq ($(SCHEDULER),cfs)
SCHEDULER_BUILD_ARGS :=
SCHEDULER_FEATURE := scheduler-cfs
else
$(error Unsupported SCHEDULER: $(SCHEDULER), expected rr or cfs)
endif

KERNEL_FEATURES := $(KERNEL_OUTPUT_LOG_LEVEL),$(SCHEDULER_FEATURE),$(KERNEL_EXTRA_FEATURES)

$(KERNEL_BIN): kernel
# 这里如果是la64，则需要保留elf给qemu-system-loongarch64才能正常运行
ifeq ($(ARCH), riscv64)
	@echo "$(OBJCOPY) $(KERNEL_ELF) $(PROJECT_ROOT)/$(KERNEL_BIN)"
	@$(OBJCOPY) $(KERNEL_ELF) --strip-all -O binary $(PROJECT_ROOT)/$(KERNEL_BIN)
else ifeq ($(ARCH), loongarch64)
	@cp $(KERNEL_ELF) $(PROJECT_ROOT)/$(KERNEL_BIN)
ifeq ($(PLATFORM),2k1000)
	@echo "Generating 2K1000 raw image: $(PROJECT_ROOT)/$(KERNEL_RAW_BIN)"
	@$(OBJCOPY) $(KERNEL_ELF) --strip-all -O binary $(PROJECT_ROOT)/$(KERNEL_RAW_BIN)
endif
endif

kernel:
	@echo "In os/Makefile, BUILD_ARGS: $(KERNEL_BUILD_ARGS)"
	@echo "Scheduler: $(SCHEDULER)"
	@echo "Kernel features: $(KERNEL_FEATURES)"
	@cargo build $(KERNEL_BUILD_ARGS) $(SCHEDULER_BUILD_ARGS) --features $(KERNEL_FEATURES)

clean:
	@cargo clean

.PHONY: build env kernel clean initrust 
