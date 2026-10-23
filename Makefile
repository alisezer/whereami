BUNDLE_ID  := io.sezer.whereami
APP_DIR    ?= $(HOME)/Applications
BIN_DIR    ?= $(HOME)/.local/bin
APP        := $(APP_DIR)/whereami.app
BUILD_APP  := target/whereami.app
BINARY     := target/release/whereami
LSREGISTER := /System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister

.PHONY: all build bundle install uninstall clean lint

all: bundle

build:
	cargo build --release

lint:
	cargo clippy --release -- -D warnings

bundle: build
	rm -rf $(BUILD_APP)
	mkdir -p $(BUILD_APP)/Contents/MacOS
	cp bundle/Info.plist $(BUILD_APP)/Contents/Info.plist
	cp $(BINARY) $(BUILD_APP)/Contents/MacOS/whereami
	codesign --force --sign - --identifier $(BUNDLE_ID) $(BUILD_APP)
	codesign --verify --strict $(BUILD_APP)

install: bundle
	mkdir -p $(APP_DIR) $(BIN_DIR)
	rm -rf $(APP)
	ditto $(BUILD_APP) $(APP)
	$(LSREGISTER) -f $(APP)
	sed 's|@APP@|$(APP)|g' bundle/wrapper.sh > $(BIN_DIR)/whereami
	chmod 755 $(BIN_DIR)/whereami
	@echo "Installed $(APP) and $(BIN_DIR)/whereami"

uninstall:
	-$(LSREGISTER) -u $(APP)
	rm -rf $(APP)
	rm -f $(BIN_DIR)/whereami
	@echo "Removed. The Location Services entry stays until you revoke it; see README."

clean:
	cargo clean
