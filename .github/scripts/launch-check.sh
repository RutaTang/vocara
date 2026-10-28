#!/bin/bash
# Launches the macOS app bundle given as $1 and checks that it boots inside
# its App Sandbox: the signature carries the sandbox entitlement, the library
# opens in the app's container, the process is still up 10 s later, and the
# diagnostics log holds no boot failure. Quits the app at the end. Used by
# CI (an ad-hoc signed Release-Prod build) and by the release workflow (the
# notarized app, before it is published).
set -euo pipefail

APP="$1"
BUNDLE_ID="${2:-com.rutatang.vocara}"
DATA="$HOME/Library/Containers/$BUNDLE_ID/Data"

codesign --display --entitlements - --xml "$APP" > "${RUNNER_TEMP:-/tmp}/launch-ent.plist"
grep -q 'com.apple.security.app-sandbox' "${RUNNER_TEMP:-/tmp}/launch-ent.plist" \
  || { echo "::error::$APP is not sandboxed"; exit 1; }

open -n "$APP"
# Up to 60 s for the library to open inside the container.
DB=""
for _ in $(seq 1 60); do
  DB="$(find "$DATA/Library" -name vocara.db 2>/dev/null | head -1 || true)"
  [ -n "$DB" ] && break
  sleep 1
done
sleep 10 # then it must still be running
EXITED=""
pgrep -f "$(basename "$APP")/Contents/MacOS/" > /dev/null \
  || { echo "::error::the app exited after launch"; EXITED=1; }
LOG="$(find "$DATA/Library" -name diagnostics.log 2>/dev/null | head -1 || true)"
if [ -n "$LOG" ]; then echo "--- diagnostics.log"; cat "$LOG"; fi
[ -n "$DB" ] || { echo "::error::no library opened in $DATA"; exit 1; }
[ -z "$EXITED" ] || exit 1
if [ -n "$LOG" ] && grep -E '\[boot\] (open (failed|refused)|window setup failed)' "$LOG"; then
  echo "::error::the app logged a boot failure"; exit 1
fi
osascript -e "quit app id \"$BUNDLE_ID\"" || true
sleep 5
pkill -f "$(basename "$APP")/Contents/MacOS/" || true
echo "launch check passed: $APP"
