TARGET = alacritty

DEBUG = false ## Build binary in debug mode
FEATURES = default ## Features to build into binary
ifeq ($(DEBUG), true)
	CARGO_BUILD = cargo build
	RELEAS_TYPE = debug
else
	CARGO_BUILD = cargo build --release
	RELEAS_TYPE = release
endif
RELEAS_DIR = target/$(RELEAS_TYPE)

APP_NAME = Alacritty.app
ASSETS_DIR = assets
APP_TEMPLATE = $(ASSETS_DIR)/osx/$(APP_NAME)
APP_DIR = $(RELEASE_DIR)/osx
APP_BINARY = $(RELEASE_DIR)/$(TARGET)
APP_BINARY_DIR  = $(APP_DIR)/$(APP_NAME)/Contents/MacOS

DMG_NAME = Alacritty.dmg
DMG_DIR = $(RELEASE_DIR)/osx

vpath $(TARGET) $(RELEASE_DIR)
vpath $(APP_NAME) $(APP_DIR)
vpath $(DMG_NAME) $(APP_DIR)

all: help

help: ## Prints help for targets with comments
	@grep -E '^[a-zA-Z._-]+:.*?## .*$$' $(MAKEFILE_LIST) | sort | awk 'BEGIN {FS = ":.*?## "}; {printf "\033[36m%-30s\033[0m %s\n", $$1, $$2}'
	@grep -E '^[a-zA-Z._-]+ *=.*?## .*$$' $(MAKEFILE_LIST) | sort | awk 'BEGIN {FS = "## "}; {printf "\033[33m%-30s\033[0m %s\n", $$1, $$2}'

binary: | $(TARGET) ## Build binary with cargo
$(TARGET):
	$(CARGO_BUILD) --no-default-features --features="$(FEATURES)"

app: | $(APP_NAME) ## Clone Alacritty.app template and mount binary
$(APP_NAME): $(TARGET) $(APP_TEMPLATE)
	@mkdir -p $(APP_BINARY_DIR)
	@cp -fRp $(APP_TEMPLATE) $(APP_DIR)
	@cp -fp $(APP_BINARY) $(APP_BINARY_DIR)
	@echo "Created '$@' in '$(APP_DIR)'"

dmg: | $(DMG_NAME) ## Pack Alacritty.app into .dmg
$(DMG_NAME): $(APP_NAME)
	@echo "Packing disk image..."
	@hdiutil create $(DMG_DIR)/$(DMG_NAME) \
		-volname "Alacritty" \
		-fs HFS+ \
		-srcfolder $(APP_DIR) \
		-ov -format UDZO
	@echo "Packed '$@' in '$(APP_DIR)'"

install: $(DMG_NAME) ## Mount disk image
	@open $(DMG_DIR)/$(DMG_NAME)

.PHONY: app binary clean dmg install

clean: ## Remove all artifacts
	-rm -rf $(APP_DIR)
