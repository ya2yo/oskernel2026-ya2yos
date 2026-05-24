USER_BUILD_ARGS := --$(MODE) --target $(TARGET)

USER_TARGET_DIR := $(if $(CARGO_TARGET_DIR),$(CARGO_TARGET_DIR),$(PROJECT_ROOT)/user/target)/$(TARGET)/$(MODE)

USER_APP_DIR := $(PROJECT_ROOT)/user/src/bin

USER_OBJCOPY := rust-objcopy --binary-architecture=$(ARCH)

USER_APPS := $(wildcard $(USER_APP_DIR)/*.rs)
USER_ELFS := $(patsubst $(USER_APP_DIR)/%.rs, $(USER_TARGET_DIR)/%, $(USER_APPS))
USER_BINS := $(patsubst $(USER_APP_DIR)/%.rs, $(USER_TARGET_DIR)/%.bin, $(USER_APPS))

export USER_BUILD_ARGS USER_TARGET_DIR USER_APP_DIR USER_APPS USER_ELFS USER_BINS USER_OBJCOPY