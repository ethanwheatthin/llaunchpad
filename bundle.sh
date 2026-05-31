#!/bin/bash
# Build Llaunchpad.app from the release binary + icon.
# Run after: cargo build --release
set -e
cd "$(dirname "$0")"

APP="Llaunchpad.app"
BIN="target/release/llaunchpad"
ICON="assets/AppIcon.icns"

[ -f "$BIN" ] || { echo "missing $BIN — run 'cargo build --release' first"; exit 1; }

VERSION=$(grep '^version' Cargo.toml | head -1 | sed 's/version = "\(.*\)"/\1/')

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/llaunchpad"
cp "$ICON" "$APP/Contents/Resources/AppIcon.icns"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Llaunchpad</string>
  <key>CFBundleDisplayName</key><string>Llaunchpad</string>
  <key>CFBundleIdentifier</key><string>com.draugvar.llaunchpad</string>
  <key>CFBundleVersion</key><string>${VERSION}</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleExecutable</key><string>llaunchpad</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

# refresh icon cache so Finder/Dock pick it up
touch "$APP"
echo "Built $APP"
