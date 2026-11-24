#!/bin/bash
# The evidence that the macOS app bundle given as $1 loads on macOS 10.15,
# its LSMinimumSystemVersion, which no CI runner can show by launching it
# (none runs 10.15):
#  - the Intel (x86_64) slice of its binary is built for 10.15: its
#    LC_BUILD_VERSION minos is 10.15 (bundle.sh's MACOSX_DEPLOYMENT_TARGET);
#    the Apple silicon (arm64) slice, if any, for 11.0;
#  - every framework and library each slice links is one macOS 10.15 ships
#    (the allow-list below), so dyld finds them all; in particular not
#    UniformTypeIdentifiers.framework, which is macOS 11+.
# Used by CI (a debug universal bundle) and by the release workflow (the
# universal release binary, before it is signed).
set -euo pipefail

APP="${1:?usage: macos-minimum-check.sh path/to/vocara.app}"
BIN="$APP/Contents/MacOS/vocara"
[[ -f "$BIN" ]] || { echo "::error::no binary at $BIN"; exit 1; }

# What the app may link, all shipped by macOS 10.15. Frameworks by name,
# under /System/Library/Frameworks (rust/PORTING.md lists the crate that
# links each and the first macOS that had it); libraries by path. A new
# dependency that links something else fails here until it is checked
# against 10.15 and added.
FRAMEWORKS=(
  AppKit
  ApplicationServices
  AudioToolbox
  AVFoundation
  Carbon
  CoreAudio
  CoreFoundation
  CoreGraphics
  CoreMedia
  CoreVideo
  Foundation
  Metal
  MetalPerformanceShaders
  QuartzCore
  Security
)
LIBRARIES=(
  /usr/lib/libSystem.B.dylib
  /usr/lib/libobjc.A.dylib
  /usr/lib/libiconv.2.dylib
  /usr/lib/libc++.1.dylib
)

FAILED=0
fail() {
  echo "::error::$*"
  FAILED=1
}

contains() {
  local needle="$1"
  shift
  local item
  for item in "$@"; do
    [[ "$item" == "$needle" ]] && return 0
  done
  return 1
}

# The slice's minimum macOS: LC_BUILD_VERSION's minos, or for an old-style
# binary LC_VERSION_MIN_MACOSX's version.
minimum() {
  otool -arch "$1" -l "$BIN" | awk '
    $1 == "cmd" { cmd = $2 }
    cmd == "LC_BUILD_VERSION" && $1 == "minos" { print $2; exit }
    cmd == "LC_VERSION_MIN_MACOSX" && $1 == "version" { print $2; exit }
  '
}

ARCHS="$(lipo -archs "$BIN")"
echo "slices: $ARCHS"
for arch in x86_64 arm64; do
  case " $ARCHS " in
    *" $arch "*) ;;
    *)
      # The Intel slice is the 10.15 one: without it nothing is shown.
      [[ "$arch" == x86_64 ]] && fail "no x86_64 slice in $BIN"
      continue
      ;;
  esac
  want=10.15
  [[ "$arch" == arm64 ]] && want=11.0
  got="$(minimum "$arch")"
  if [[ "$got" == "$want" ]]; then
    echo "$arch: minimum macOS $got"
  else
    fail "$arch slice is built for macOS ${got:-?}, not $want"
  fi

  # Every load command naming a dylib (otool -L: one per line after the
  # header, the install name first).
  linked=0
  while read -r path _; do
    linked=$((linked + 1))
    if [[ "$path" == /System/Library/Frameworks/*.framework/* ]]; then
      name="${path#/System/Library/Frameworks/}"
      name="${name%%.framework/*}"
      if contains "$name" "${FRAMEWORKS[@]}"; then
        echo "$arch: links $name.framework"
      else
        fail "$arch slice links $path, which is not on the macOS 10.15 allow-list"
      fi
    elif contains "$path" "${LIBRARIES[@]}"; then
      echo "$arch: links $path"
    else
      fail "$arch slice links $path, which is not on the macOS 10.15 allow-list"
    fi
  done < <(otool -arch "$arch" -L "$BIN" | tail -n +2)
  # libSystem at least: none read means otool failed.
  [[ "$linked" -gt 0 ]] || fail "otool listed nothing $arch links"
done

[[ "$FAILED" == 0 ]] || exit 1
echo "macOS 10.15 check passed: $BIN"
