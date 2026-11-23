#!/bin/bash
# Builds Vocara.app from the Rust workspace: the iced app (with the live audio
# engine), its Info.plist, icon and entitlements-ready layout. The release
# workflow signs, notarizes and packages the result; locally it runs as is.
#
#   rust/packaging/macos/bundle.sh [--flavor prod|dev|test] [--universal]
#                                  [--debug] [--out DIR]
#
# Prints the path of the .app on stdout. The flavor is baked into the binary
# (VOCARA_FLAVOR at compile time) and picks the bundle id, so each flavor
# keeps its own sandbox container and library, as the Flutter app's did:
# com.rutatang.vocara, .dev, .test.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
WORKSPACE="$HERE/../.."
FLAVOR=prod
UNIVERSAL=0
PROFILE=release
OUT="$WORKSPACE/target/bundle"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --flavor) FLAVOR="$2"; shift 2 ;;
    --universal) UNIVERSAL=1; shift ;;
    --debug) PROFILE=debug; shift ;;
    --out) OUT="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

case "$FLAVOR" in
  prod) BUNDLE_ID=com.rutatang.vocara; DISPLAY_NAME=Vocara ;;
  dev) BUNDLE_ID=com.rutatang.vocara.dev; DISPLAY_NAME="Vocara Dev" ;;
  test) BUNDLE_ID=com.rutatang.vocara.test; DISPLAY_NAME="Vocara Test" ;;
  *) echo "unknown flavor: $FLAVOR" >&2; exit 2 ;;
esac

# The version and build number: the app crate's package version, and the
# build number the update check pins next to it.
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$WORKSPACE/Cargo.toml" | head -1)"
BUILD="$(sed -n 's/.*APP_BUILD_NUMBER: &.static str = "\(.*\)";/\1/p' \
  "$WORKSPACE/crates/vocara_services/src/update_check_service.rs")"
[[ -n "$VERSION" && -n "$BUILD" ]] || { echo "could not read version/build" >&2; exit 1; }

build_one() {
  local target="$1"
  local flags=(--locked -p vocara_app --features device --target "$target")
  [[ "$PROFILE" == release ]] && flags+=(--release)
  (cd "$WORKSPACE" && VOCARA_FLAVOR="$FLAVOR" cargo build "${flags[@]}" >&2)
  echo "$WORKSPACE/target/$target/$PROFILE/vocara"
}

ARM="$(build_one aarch64-apple-darwin)"
if [[ "$UNIVERSAL" == 1 ]]; then
  X86="$(build_one x86_64-apple-darwin)"
  BIN="$WORKSPACE/target/vocara.universal"
  lipo -create "$ARM" "$X86" -output "$BIN"
  lipo -info "$BIN" >&2
else
  BIN="$ARM"
fi

APP="$OUT/vocara.app"
rm -rf "${APP:?}"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/vocara"
sed -e "s/@BUNDLE_ID@/$BUNDLE_ID/" -e "s/@DISPLAY_NAME@/$DISPLAY_NAME/" \
  -e "s/@VERSION@/$VERSION/" -e "s/@BUILD@/$BUILD/" \
  "$HERE/Info.plist.in" > "$APP/Contents/Info.plist"
printf 'APPL????' > "$APP/Contents/PkgInfo"

# The icon, from the PNGs the Flutter app's asset catalog shipped.
ICONS="$HERE/AppIcon.appiconset"
SET="$(mktemp -d)/AppIcon.iconset"
mkdir -p "$SET"
for size in 16 32 128 256 512; do
  cp "$ICONS/app_icon_${size}.png" "$SET/icon_${size}x${size}.png"
  double=$((size * 2))
  cp "$ICONS/app_icon_${double}.png" "$SET/icon_${size}x${size}@2x.png" 2>/dev/null \
    || cp "$ICONS/app_icon_1024.png" "$SET/icon_${size}x${size}@2x.png"
done
iconutil -c icns "$SET" -o "$APP/Contents/Resources/AppIcon.icns"
rm -rf "${SET:?}"

# Ad-hoc signed for local runs; the release workflow re-signs with the
# Developer ID, the hardened runtime and Release.entitlements.
codesign --force --sign - --entitlements "$HERE/Release.entitlements" "$APP" >&2

echo "$APP"
