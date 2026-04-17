# 这个Makefile用于编译内核
# 该Makefile必须被上层Makefile调用

build: $(KERNEL_BIN)

$(KERNEL_BIN): kernel
# 这里如果是la64，则需要保留elf给qemu-system-loongarch64才能正常运行
ifeq ($(ARCH), riscv64)
	@echo "$(OBJCOPY) $(KERNEL_ELF) $(PROJECT_ROOT)/$(KERNEL_BIN)"
	@$(OBJCOPY) $(KERNEL_ELF) --strip-all -O binary $(PROJECT_ROOT)/$(KERNEL_BIN)
else ifeq ($(ARCH), loongarch64)
	@cp $(KERNEL_ELF) $(PROJECT_ROOT)/$(KERNEL_BIN)
endif

kernel:
	@echo "In os/Makefile, BUILD_ARGS: $(KERNEL_BUILD_ARGS)"
	@cargo build $(KERNEL_BUILD_ARGS) --features $(KERNEL_OUTPUT_LOG_LEVEL)

clean:
	@cargo clean

.PHONY: build env kernel clean initrust 
