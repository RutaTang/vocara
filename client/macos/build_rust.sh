#!/usr/bin/env bash
# Build the vocara_engine Rust cdylib and package it as a macOS framework the Runner embeds.
# Invoked by vocara_engine.podspec (prepare_command + a before_compile script phase).
#
# Apple-Silicon-only by default; set VOCARA_UNIVERSAL=1 (release CI) for a fat binary.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
CRATE="$HERE/../rust/vocara_engine"
NAME="vocara_engine"
LIB="lib${NAME}.dylib"
FW_DIR="$HERE/Frameworks"
FW="$FW_DIR/${NAME}.framework"

cd "$CRATE"

build_one() {
  local target="$1"
  # An explicit --target-dir: a CARGO_TARGET_DIR or build.target-dir set in the
  # environment would otherwise build elsewhere while the path below copied a
  # stale dylib from an earlier build (or failed when there was none).
  cargo build --release --locked --features device --target "$target" \
    --target-dir "$CRATE/target" >&2
  echo "$CRATE/target/$target/release/$LIB"
}

ARM="$(build_one aarch64-apple-darwin)"
if [[ "${VOCARA_UNIVERSAL:-0}" == "1" ]]; then
  X86="$(build_one x86_64-apple-darwin)"
  FINAL="$CRATE/target/${LIB}.universal"
  lipo -create "$ARM" "$X86" -output "$FINAL"
  lipo -info "$FINAL" >&2
else
  FINAL="$ARM"
fi

# (Re)build a deep framework bundle.
rm -rf "$FW"
mkdir -p "$FW/Versions/A/Resources"
cp "$FINAL" "$FW/Versions/A/$NAME"
ln -sf A "$FW/Versions/Current"
ln -sf "Versions/Current/$NAME" "$FW/$NAME"
ln -sf "Versions/Current/Resources" "$FW/Resources"

cat > "$FW/Versions/A/Resources/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key><string>com.vocara.${NAME}</string>
  <key>CFBundleName</key><string>${NAME}</string>
  <key>CFBundleExecutable</key><string>${NAME}</string>
  <key>CFBundlePackageType</key><string>FMWK</string>
  <key>CFBundleVersion</key><string>0.1.0</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>MinimumOSVersion</key><string>10.15</string>
</dict>
</plist>
PLIST

# CRITICAL: the @rpath install_name must match the embedded location, or DynamicLibrary.open
# fails at runtime even though the file is present. The Runner's @rpath includes
# @executable_path/../Frameworks.
install_name_tool -id "@rpath/${NAME}.framework/Versions/A/${NAME}" "$FW/Versions/A/$NAME"

# Ad-hoc sign for dev; the Runner's embed phase re-signs with the real identity in Release.
codesign --force --sign - "$FW/Versions/A/$NAME" >&2 2>/dev/null || true

echo "== otool -L (install_name must be @rpath/...) ==" >&2
otool -L "$FW/Versions/A/$NAME" | head -2 >&2
echo "built $FW" >&2
