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
# keeps its own library folder (~/Library/Application Support/<bundle id>),
# as the Flutter app's did: com.rutatang.vocara, .dev, .test.
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

# The oldest macOS each slice runs on (LSMinimumSystemVersion in
# Info.plist.in): 10.15 for Intel, the Flutter app's deployment target;
# 11.0 for Apple silicon, the first macOS it ran (and rustc's floor there).
# The C dependencies (cc) build for the same version.
deployment_target() {
  case "$1" in
    x86_64-apple-darwin) echo 10.15 ;;
    aarch64-apple-darwin) echo 11.0 ;;
    *) echo "no deployment target for $1" >&2; return 2 ;;
  esac
}

build_one() {
  local target="$1"
  local minimum
  minimum="$(deployment_target "$target")"
  local flags=(--locked -p vocara_app --features device --target "$target")
  [[ "$PROFILE" == release ]] && flags+=(--release)
  (cd "$WORKSPACE" && MACOSX_DEPLOYMENT_TARGET="$minimum" VOCARA_FLAVOR="$FLAVOR" \
    cargo build "${flags[@]}" >&2)
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

# Ad-hoc signed for local runs, with Release.entitlements (the microphone
# only; no build is sandboxed: see the file); the release workflow re-signs
# with the Developer ID, the hardened runtime and the same file. A release
# bundle gets the hardened runtime here too, so a local or CI launch of it
# runs as the shipped app does: without the runtime a missing entitlement
# goes unnoticed (the microphone opens regardless). A --debug bundle leaves
# it off, so a debugger can attach.
SIGN=(--force --sign - --entitlements "$HERE/Release.entitlements")
[[ "$PROFILE" == release ]] && SIGN+=(--options runtime)
codesign "${SIGN[@]}" "$APP" >&2

echo "$APP"
