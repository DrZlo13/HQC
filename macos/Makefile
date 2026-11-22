# Makefile — single entry point for the macOS app.
# Run `make` or `make help` to list targets.

APP_NAME  := HQC
BIN_NAME  := HQC
BUNDLE_ID := io.github.drzlo13.hqc
APP       := $(APP_NAME).app

# The signing identity, ad-hoc (`-`) by default. macOS ties what it lets an ad-hoc signed app do
# (Full Disk Access) to that one build; with a certificate of your own it stays across builds:
# `make build SIGN_IDENTITY="<certificate name>"`.
SIGN_IDENTITY ?= -

# The app icon is made at build time from the web client's icon (not committed).
ICON_SRC  := ../client/public/icon-512.png
ICONSET   := .build/AppIcon.iconset
ICON      := .build/AppIcon.icns

.DEFAULT_GOAL := help

.PHONY: help
help: ## Show available targets
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) \
		| awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2}'

$(ICON): $(ICON_SRC)
	rm -rf "$(ICONSET)"
	mkdir -p "$(ICONSET)"
	sips -z 16 16   "$(ICON_SRC)" --out "$(ICONSET)/icon_16x16.png" >/dev/null
	sips -z 32 32   "$(ICON_SRC)" --out "$(ICONSET)/icon_16x16@2x.png" >/dev/null
	sips -z 32 32   "$(ICON_SRC)" --out "$(ICONSET)/icon_32x32.png" >/dev/null
	sips -z 64 64   "$(ICON_SRC)" --out "$(ICONSET)/icon_32x32@2x.png" >/dev/null
	sips -z 128 128 "$(ICON_SRC)" --out "$(ICONSET)/icon_128x128.png" >/dev/null
	sips -z 256 256 "$(ICON_SRC)" --out "$(ICONSET)/icon_128x128@2x.png" >/dev/null
	sips -z 256 256 "$(ICON_SRC)" --out "$(ICONSET)/icon_256x256.png" >/dev/null
	sips -z 512 512 "$(ICON_SRC)" --out "$(ICONSET)/icon_256x256@2x.png" >/dev/null
	sips -z 512 512 "$(ICON_SRC)" --out "$(ICONSET)/icon_512x512.png" >/dev/null
	iconutil -c icns "$(ICONSET)" -o "$(ICON)"

.PHONY: build
build: $(ICON) ## Build a release HQC.app bundle (compile + assemble + sign: ad-hoc, or SIGN_IDENTITY)
	@echo "==> Building release binary..."
	swift build -c release
	@echo "==> Assembling $(APP)..."
	rm -rf "$(APP)"
	mkdir -p "$(APP)/Contents/MacOS" "$(APP)/Contents/Resources"
	cp ".build/release/$(BIN_NAME)" "$(APP)/Contents/MacOS/$(APP_NAME)"
	cp "$(ICON)" "$(APP)/Contents/Resources/AppIcon.icns"
	@printf '%s\n' \
		'<?xml version="1.0" encoding="UTF-8"?>' \
		'<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">' \
		'<plist version="1.0">' \
		'<dict>' \
		'    <key>CFBundleName</key>' \
		'    <string>$(APP_NAME)</string>' \
		'    <key>CFBundleDisplayName</key>' \
		'    <string>$(APP_NAME)</string>' \
		'    <key>CFBundleExecutable</key>' \
		'    <string>$(APP_NAME)</string>' \
		'    <key>CFBundleIdentifier</key>' \
		'    <string>$(BUNDLE_ID)</string>' \
		'    <key>CFBundleIconFile</key>' \
		'    <string>AppIcon</string>' \
		'    <key>CFBundleIconName</key>' \
		'    <string>AppIcon</string>' \
		'    <key>CFBundlePackageType</key>' \
		'    <string>APPL</string>' \
		'    <key>CFBundleShortVersionString</key>' \
		'    <string>1.0</string>' \
		'    <key>CFBundleVersion</key>' \
		'    <string>1</string>' \
		'    <key>LSMinimumSystemVersion</key>' \
		'    <string>14.0</string>' \
		'    <key>NSPrincipalClass</key>' \
		'    <string>NSApplication</string>' \
		'    <key>NSHighResolutionCapable</key>' \
		'    <true/>' \
		'    <key>NSAppTransportSecurity</key>' \
		'    <dict>' \
		'        <key>NSAllowsArbitraryLoadsInWebContent</key>' \
		'        <true/>' \
		'    </dict>' \
		'</dict>' \
		'</plist>' \
		> "$(APP)/Contents/Info.plist"
	@echo "==> Signing ($(SIGN_IDENTITY))..."
	codesign --force --deep --options runtime --sign "$(SIGN_IDENTITY)" "$(APP)"
	@echo "==> Done. Created $(APP) (run: open $(APP))"

.PHONY: run
run: ## Quit any running instance, build, then launch HQC.app
	@osascript -e 'quit app "$(APP_NAME)"' >/dev/null 2>&1 || true
	@pkill -x "$(APP_NAME)" >/dev/null 2>&1 || true
	@$(MAKE) --no-print-directory build
	@open "$(APP)"

.PHONY: clean
clean: ## Remove build artifacts
	swift package clean
	rm -rf .build "$(APP)"
