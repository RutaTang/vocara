#!/bin/bash
# Launches the macOS app bundle given as APP and checks that it boots as the
# shipped app must: unsandboxed, under the hardened runtime. The signature
# carries the runtime flag and the microphone entitlement (without which the
# runtime keeps the mic shut) and no App Sandbox entitlement; the library
# opens in ~/Library/Application Support/<bundle id> and no sandbox
# container is made; the process is still up 10 s later; and the
# diagnostics log holds no boot failure. Quits the app at the end. Used by
# CI (an ad-hoc signed release build, run by hand) and by the release
# workflow (the notarized app, before it is published).
#
#   launch-check.sh [--update] APP [BUNDLE_ID]
#
# With --update it checks an update instead of a fresh install: it
# downloads the last sandboxed release's DMG (LAST_SANDBOXED_RELEASE below,
# or UPDATE_FROM) from RELEASES_REPO, checks it against the release's
# SHA256SUMS and that it is sandboxed, launches it so that it makes its App
# Sandbox container and its library there, and quits it. Then it launches
# APP and checks that the library moved out of the container whole, to
# ~/Library/Application Support/<bundle id>, and that APP booted on it:
# the device node id the old release minted is the one found there, the
# container's folder is gone (moved, not copied), and APP logged the move
# (crates/vocara_services/src/sandbox_migration.rs).
#
# Either way it needs a Mac with no Vocara data for BUNDLE_ID (a CI runner;
# a workflow running both checks removes the fresh install's library in
# between), and refuses to start otherwise rather than launch a build on a
# real library.
set -euo pipefail

# The last release that ran in the App Sandbox: the one whose container an
# update carries the library out of. Every later release is unsandboxed
# (rust/packaging/macos/Release.entitlements), so this does not move on.
LAST_SANDBOXED_RELEASE=v0.3.0

UPDATE=""
if [ "${1:-}" = "--update" ]; then
  UPDATE=1
  shift
fi
APP="${1:?usage: launch-check.sh [--update] APP [BUNDLE_ID]}"
BUNDLE_ID="${2:-com.rutatang.vocara}"
UPDATE_FROM="${UPDATE_FROM:-$LAST_SANDBOXED_RELEASE}"
RELEASES_REPO="${RELEASES_REPO:-RutaTang/vocara}"
CONTAINER="$HOME/Library/Containers/$BUNDLE_ID"
CONTAINER_LIBRARY="$CONTAINER/Data/Library/Application Support/$BUNDLE_ID"
LIBRARY="$HOME/Library/Application Support/$BUNDLE_ID"
WORK="$(mktemp -d "${RUNNER_TEMP:-/tmp}/launch-check.XXXXXX")"
OLD_MOUNT=""

cleanup() {
  if [ -n "$OLD_MOUNT" ]; then hdiutil detach "$OLD_MOUNT" -force > /dev/null 2>&1 || true; fi
  rm -rf "$WORK"
}
trap cleanup EXIT

fail() {
  echo "::error::$*"
  exit 1
}

# Whether the app bundle at $1 is running. Matched by its folder's name and
# its own, which tell two copies of the app apart (the DMG mounts are
# temporary folders) whatever symlinks their paths resolve through.
running() {
  pgrep -f "$(basename "$(dirname "$1")")/$(basename "$1")/Contents/MacOS/" > /dev/null
}

# Quits the app bundle at $1 (the only copy running, so by its bundle id)
# and waits for it to go, killing it if it will not.
quit() {
  local pattern
  pattern="$(basename "$(dirname "$1")")/$(basename "$1")/Contents/MacOS/"
  osascript -e "quit app id \"$BUNDLE_ID\"" > /dev/null 2>&1 || true
  for _ in $(seq 1 20); do running "$1" || return 0; sleep 1; done
  pkill -f "$pattern" || true
  for _ in $(seq 1 10); do running "$1" || return 0; sleep 1; done
  pkill -9 -f "$pattern" || true
  sleep 2
  ! running "$1"
}

# Waits up to 60 s for $1 to exist.
appears() {
  for _ in $(seq 1 60); do
    [ -e "$1" ] && return 0
    sleep 1
  done
  return 1
}

# Prints the diagnostics log in the library folder $1, if there is one.
show_log() {
  if [ -f "$1/diagnostics.log" ]; then
    echo "--- diagnostics.log in $1"
    cat "$1/diagnostics.log"
  fi
}

[ -d "$APP" ] || fail "no app bundle at $APP"
[ ! -e "$CONTAINER" ] \
  || fail "this check needs a Mac with no Vocara data for $BUNDLE_ID, and $CONTAINER exists"
[ ! -e "$LIBRARY" ] \
  || fail "this check needs a Mac with no Vocara data for $BUNDLE_ID, and $LIBRARY exists"

# The signature: the hardened runtime, the microphone, no sandbox.
codesign --display --entitlements - --xml "$APP" > "$WORK/entitlements.plist" 2> /dev/null \
  || fail "$APP is not signed"
if grep -q 'com.apple.security.app-sandbox' "$WORK/entitlements.plist"; then
  fail "$APP is signed with the App Sandbox entitlement: Vocara is not sandboxed any more, and a sandboxed build opens an empty library inside its container"
fi
grep -q 'com.apple.security.device.audio-input' "$WORK/entitlements.plist" \
  || fail "$APP is signed without com.apple.security.device.audio-input: under the hardened runtime the microphone cannot open"
codesign --display --verbose=2 "$APP" > "$WORK/signature.txt" 2>&1 \
  || fail "$APP is not signed"
grep -qE 'flags=.*runtime' "$WORK/signature.txt" \
  || fail "$APP is not signed with the hardened runtime"

NODE=""
if [ -n "$UPDATE" ]; then
  # The last sandboxed release, as users have it: its DMG, checked against
  # the checksum published beside it.
  VERSION="${UPDATE_FROM#v}"
  DMG="Vocara-$VERSION.dmg"
  BASE="https://github.com/$RELEASES_REPO/releases/download/$UPDATE_FROM"
  curl -sSfL --retry 3 -o "$WORK/$DMG" "$BASE/$DMG" \
    || fail "could not download $DMG from $RELEASES_REPO's $UPDATE_FROM release (is it published?)"
  curl -sSfL --retry 3 -o "$WORK/SHA256SUMS" "$BASE/SHA256SUMS" \
    || fail "could not download the SHA256SUMS of $RELEASES_REPO's $UPDATE_FROM release"
  EXPECTED="$(awk -v dmg="./$DMG" '$2 == dmg { print $1 }' "$WORK/SHA256SUMS")"
  ACTUAL="$(shasum -a 256 "$WORK/$DMG" | cut -d ' ' -f 1)"
  if [ -z "$EXPECTED" ] || [ "$EXPECTED" != "$ACTUAL" ]; then
    fail "$DMG does not match the SHA256SUMS of its release"
  fi
  OLD_MOUNT="$(mktemp -d "${RUNNER_TEMP:-/tmp}/previous.XXXXXX")"
  hdiutil attach -nobrowse -readonly -mountpoint "$OLD_MOUNT" "$WORK/$DMG" > /dev/null \
    || fail "could not mount $DMG"
  OLD_APP="$OLD_MOUNT/vocara.app"
  # To a file, then grep: grep -q ending a pipe early fails it under pipefail.
  codesign --display --entitlements - --xml "$OLD_APP" > "$WORK/previous.plist" 2> /dev/null \
    || fail "$UPDATE_FROM's app is not signed"
  grep -q 'com.apple.security.app-sandbox' "$WORK/previous.plist" \
    || fail "$UPDATE_FROM is not a sandboxed release: it makes no container, so there is no library to move out of one (set LAST_SANDBOXED_RELEASE to the last release that was)"

  # Its first launch makes the container and the library in it, as on
  # every user's Mac it ran on.
  open -n "$OLD_APP"
  if ! appears "$CONTAINER_LIBRARY/device_node" || ! appears "$CONTAINER_LIBRARY/vocara.db"; then
    show_log "$CONTAINER_LIBRARY"
    fail "$UPDATE_FROM opened no library in its container ($CONTAINER_LIBRARY)"
  fi
  sleep 5 # its boot finishes
  NODE="$(cat "$CONTAINER_LIBRARY/device_node")"
  [ -n "$NODE" ] || fail "$UPDATE_FROM wrote an empty device node id"
  quit "$OLD_APP" || fail "$UPDATE_FROM would not quit"
  hdiutil detach "$OLD_MOUNT" -force > /dev/null || true
  OLD_MOUNT=""
  [ ! -e "$LIBRARY" ] || fail "$UPDATE_FROM made $LIBRARY: it did not run sandboxed"
  echo "$UPDATE_FROM made its library in its container (device node $NODE), and quit"
fi

open -n "$APP"
# Up to 60 s for the library to open in Application Support.
appears "$LIBRARY/vocara.db" || true
sleep 10 # then it must still be running
EXITED=""
running "$APP" || { echo "::error::the app exited after launch"; EXITED=1; }
show_log "$LIBRARY"

if [ -n "$UPDATE" ]; then
  if [ ! -f "$LIBRARY/vocara.db" ]; then
    # Say why, from what the app logged where it ran: a library kept in the
    # container logs that it stayed there, and the reason.
    show_log "$CONTAINER_LIBRARY"
    STAYED="$(grep -h '\[storage\]' "$CONTAINER_LIBRARY/diagnostics.log" 2> /dev/null | tail -1 || true)"
    if [ -n "$STAYED" ]; then
      fail "the library did not move out of $UPDATE_FROM's container to $LIBRARY. The app said: $STAYED"
    fi
    fail "the library did not move out of $UPDATE_FROM's container to $LIBRARY, and the app logged nothing about it (it may never have got past its start: on macOS 14 and later, a build the system does not take for the container's owner, one signed by another team or ad hoc, is held at, or refused by, the container's App Data protection)"
  fi
  MOVED_NODE="$(cat "$LIBRARY/device_node" 2> /dev/null || true)"
  [ "$MOVED_NODE" = "$NODE" ] \
    || fail "the library in $LIBRARY is a new one (device node '$MOVED_NODE'), not the one $UPDATE_FROM made in its container ('$NODE'): the migration did not happen"
  [ ! -e "$CONTAINER_LIBRARY" ] \
    || fail "the container still holds a library folder ($CONTAINER_LIBRARY) beside $LIBRARY: it was copied, not moved, or made again"
  grep -q '\[storage\] moved the library out of the App Sandbox container' "$LIBRARY/diagnostics.log" \
    || fail "the library is in $LIBRARY, but the app did not log moving it there"
else
  [ -f "$LIBRARY/vocara.db" ] || fail "no library opened in $LIBRARY"
  [ ! -e "$CONTAINER" ] || fail "the app made an App Sandbox container ($CONTAINER): it ran sandboxed"
fi
[ -z "$EXITED" ] || exit 1
if grep -E '\[boot\] (open (failed|refused)|window setup failed)' "$LIBRARY/diagnostics.log"; then
  fail "the app logged a boot failure"
fi
quit "$APP" || true
if [ -n "$UPDATE" ]; then
  echo "update check passed: $APP opened $UPDATE_FROM's library, moved out of its container"
else
  echo "launch check passed: $APP"
fi
