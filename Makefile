SHELL := /bin/zsh

PROJECT_ROOT := $(CURDIR)
CORE_DIR := $(PROJECT_ROOT)/src/core
TARGET := aarch64-apple-ios
SDK := $(shell xcrun --sdk iphoneos --show-sdk-path)
CLANG := $(shell xcrun --sdk iphoneos --find clang)
CORE_LIBRARY := $(CORE_DIR)/target/$(TARGET)/release/libisaac_cloud_core.a
DYLIB := $(PROJECT_ROOT)/build/IsaacSteamCloudSynciOS.dylib
DEB_STAGE := $(PROJECT_ROOT)/package/stage
DEB := $(PROJECT_ROOT)/packages/IsaacSteamCloudSynciOS-rootless.deb
MIN_IOS ?= 15.0
EXTRA_CFLAGS ?=
DIST := $(PROJECT_ROOT)/dist
RUST_REMAP_FLAGS := --remap-path-prefix=$(HOME)=/build

OBJC_SOURCES := \
	$(PROJECT_ROOT)/src/core/Keychain.m \
	$(PROJECT_ROOT)/src/loaders/Bootstrap.m \
	$(PROJECT_ROOT)/src/isaac/IsaacLifecycle.m \
	$(PROJECT_ROOT)/src/ui/IsaacCloudUI.m

.PHONY: all core dylib package test audit release clean

all: dylib package

core:
	cd "$(CORE_DIR)" && IPHONEOS_DEPLOYMENT_TARGET="$(MIN_IOS)" \
		RUSTFLAGS="$(RUSTFLAGS) $(RUST_REMAP_FLAGS)" \
		cargo build --release --target "$(TARGET)" --locked

dylib: core
	mkdir -p "$(PROJECT_ROOT)/build"
	"$(CLANG)" -isysroot "$(SDK)" -arch arm64 -miphoneos-version-min="$(MIN_IOS)" \
		-fobjc-arc -fmodules -O2 $(EXTRA_CFLAGS) -dynamiclib \
		-I"$(PROJECT_ROOT)/src/loaders" \
		-Wl,-install_name,@rpath/IsaacSteamCloudSynciOS.dylib \
		-Wl,-dead_strip -Wl,-fatal_warnings \
		-Wl,-exported_symbols_list,"$(PROJECT_ROOT)/package/exports.txt" \
		$(OBJC_SOURCES) "$(CORE_LIBRARY)" \
		-framework Foundation -framework UIKit -framework Security -framework CoreImage \
		-lz -liconv -o "$(DYLIB)"
	xcrun strip -x "$(DYLIB)"
	@if command -v codesign >/dev/null 2>&1; then \
		codesign --force --sign - --timestamp=none --identifier com.emp0ry.isaacsteamcloudsyncios.dylib "$(DYLIB)"; \
	elif command -v ldid >/dev/null 2>&1; then \
		ldid -S "$(DYLIB)"; \
	fi

package: dylib
	rm -rf "$(DEB_STAGE)"
	mkdir -p "$(DEB_STAGE)/DEBIAN" "$(DEB_STAGE)/var/jb/Library/MobileSubstrate/DynamicLibraries"
	cp "$(PROJECT_ROOT)/package/control" "$(DEB_STAGE)/DEBIAN/control"
	cp "$(PROJECT_ROOT)/package/IsaacSteamCloudSynciOS.plist" "$(DEB_STAGE)/var/jb/Library/MobileSubstrate/DynamicLibraries/IsaacSteamCloudSynciOS.plist"
	cp "$(DYLIB)" "$(DEB_STAGE)/var/jb/Library/MobileSubstrate/DynamicLibraries/IsaacSteamCloudSynciOS.dylib"
	mkdir -p "$(PROJECT_ROOT)/packages"
	dpkg-deb --root-owner-group --build "$(DEB_STAGE)" "$(DEB)"

test:
	cd "$(CORE_DIR)" && cargo test --locked

audit: dylib
	file "$(DYLIB)"
	otool -L "$(DYLIB)"
	@if command -v codesign >/dev/null 2>&1; then codesign --verify --strict "$(DYLIB)"; fi
	@if nm -u "$(DYLIB)" | rg -i 'substrate|ellekit|libhooker|/var/jb'; then \
		echo "ERROR: jailbreak-only dependency detected"; exit 1; \
	else \
		echo "Portable dependency audit passed"; \
	fi

release:
	rm -rf "$(DIST)"
	$(MAKE) test
	$(MAKE) package EXTRA_CFLAGS='-Wall -Wextra -Werror'
	$(MAKE) audit EXTRA_CFLAGS='-Wall -Wextra -Werror'
	mkdir -p "$(DIST)"
	cp "$(DYLIB)" "$(DIST)/IsaacSteamCloudSynciOS.dylib"
	cp "$(DEB)" "$(DIST)/IsaacSteamCloudSynciOS-rootless.deb"
	cd "$(DIST)" && shasum -a 256 \
		IsaacSteamCloudSynciOS.dylib \
		IsaacSteamCloudSynciOS-rootless.deb > SHA256SUMS

clean:
	cd "$(CORE_DIR)" && cargo clean
	rm -rf "$(PROJECT_ROOT)/build" "$(PROJECT_ROOT)/package/stage" "$(DIST)"
	find "$(PROJECT_ROOT)/packages" -maxdepth 1 \
		\( -name 'IsaacCloudSync-rootless.deb' -o -name 'IsaacSteamCloudSynciOS-rootless.deb' \) -delete
