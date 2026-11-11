#!/usr/bin/env bash
# Builds BrowseCraft and wraps it in a macOS app bundle:
#   dist/BrowseCraft.app/Contents/{Info.plist,MacOS/browsecraft,MacOS/filters/,Resources/BrowseCraft.icns}
# Usage: scripts/package-macos.sh [--install]   (--install copies it to /Applications)
set -euo pipefail
ID=io.github.matteomicheletti.browsecraft
ROOT=$(cd "$(dirname "$0")/.." && pwd)
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)
APP=$ROOT/dist/BrowseCraft.app

cargo build --release --locked --manifest-path "$ROOT/Cargo.toml"

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
install -m 0755 "$ROOT/target/release/browsecraft" "$APP/Contents/MacOS/browsecraft"
cp -r "$ROOT/filters" "$APP/Contents/MacOS/filters"

# Icon: build an .iconset from the 512 px PNG with sips, then iconutil.
SET=$(mktemp -d)/BrowseCraft.iconset
mkdir -p "$SET"
SRC=$ROOT/assets/icon/browsecraft-512.png
for s in 16 32 128 256 512; do
  sips -z $s $s "$SRC" --out "$SET/icon_${s}x${s}.png" >/dev/null
  d=$((s * 2)); [[ $d -le 512 ]] && sips -z $d $d "$SRC" --out "$SET/icon_${s}x${s}@2x.png" >/dev/null
done
cp "$SRC" "$SET/icon_256x256@2x.png"
iconutil -c icns "$SET" -o "$APP/Contents/Resources/BrowseCraft.icns"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>BrowseCraft</string>
  <key>CFBundleDisplayName</key><string>BrowseCraft</string>
  <key>CFBundleIdentifier</key><string>$ID</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleExecutable</key><string>browsecraft</string>
  <key>CFBundleIconFile</key><string>BrowseCraft</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.productivity</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
  <key>NSCameraUsageDescription</key><string>Websites can ask to use the camera.</string>
  <key>NSMicrophoneUsageDescription</key><string>Websites can ask to use the microphone.</string>
  <key>CFBundleURLTypes</key>
  <array>
    <dict>
      <key>CFBundleURLName</key><string>Web URL</string>
      <key>CFBundleURLSchemes</key><array><string>http</string><string>https</string></array>
    </dict>
  </array>
</dict>
</plist>
PLIST

# Ad-hoc signature so Gatekeeper and WebKit accept the local build.
codesign --force --deep --sign - "$APP"
echo "Built $APP ($VERSION)"

if [[ ${1:-} == --install ]]; then
  rm -rf /Applications/BrowseCraft.app
  cp -R "$APP" /Applications/
  echo "Installed /Applications/BrowseCraft.app"
fi
