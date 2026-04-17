# 用于编译用户程序，只能由上层调用

elf: $(USER_APPS)
	@echo "In user/Makefile, BUILD_ARGS: $(USER_BUILD_ARGS)"
	@cargo build $(USER_BUILD_ARGS)

binary: elf
	@echo "Converting ELF files to binary for user programs..."
	@$(foreach elf, $(USER_ELFS), $(USER_OBJCOPY) $(elf) --strip-all -O binary $(patsubst $(USER_TARGET_DIR)/%, $(USER_TARGET_DIR)/%.bin, $(elf));)
	@echo "Finished converting ELF files to binary for user programs."

build: binary

clean:
	@cargo clean

.PHONY: elf binary build clean
