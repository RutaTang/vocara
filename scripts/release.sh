#!/usr/bin/env bash
# Vocara release cutter: universal build -> Developer ID deep re-sign -> DMG
# -> notarize -> staple. Produces dist/Vocara-<version>.dmg at the repo root.
#
# One-time setup (details in docs/release-runbook.md):
#   * 'Developer ID Application' certificate in the login keychain
#   * a notarytool keychain profile (the script tells you the exact command
#     to run if it is missing)
#
# Usage: scripts/release.sh
#
# Environment overrides:
#   VOCARA_SIGN_IDENTITY    codesign identity (default: "Developer ID Application")
#   VOCARA_NOTARY_PROFILE   notarytool keychain profile name (default: vocara-notary)
#   NOTARY_KEY_PATH / NOTARY_KEY_ID / NOTARY_ISSUER_ID
#                           App Store Connect API key (CI path; used instead of
#                           the keychain profile when NOTARY_KEY_PATH is set)
#   VOCARA_SKIP_NOTARIZE=1  stop after the signed DMG (it will NOT pass
#                           Gatekeeper on other machines; local testing only)
#   VOCARA_ALLOW_DIRTY=1    skip the clean-tree check (local testing only)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CLIENT="$ROOT/client"
APP="$CLIENT/build/macos/Build/Products/Release-Prod/vocara.app"
ENTITLEMENTS="$CLIENT/macos/Runner/Release.entitlements"
DIST="$ROOT/dist"

IDENTITY="${VOCARA_SIGN_IDENTITY:-Developer ID Application}"
NOTARY_PROFILE="${VOCARA_NOTARY_PROFILE:-vocara-notary}"

fail() {
  echo "" >&2
  echo "RELEASE FAILED: $*" >&2
  exit 1
}

step() { echo ""; echo "==> $*"; }

# --- 0. Preconditions -------------------------------------------------------

step "Preconditions"
command -v flutter >/dev/null || fail "flutter not on PATH"
command -v cargo >/dev/null || fail "cargo not on PATH (Rust engine build needs it)"
rustup target list --installed | grep -q x86_64-apple-darwin \
  || fail "missing Rust target; run: rustup target add x86_64-apple-darwin"
security find-identity -v -p codesigning | grep -q "Developer ID Application" \
  || fail "no 'Developer ID Application' certificate in the keychain"

if [[ "${VOCARA_ALLOW_DIRTY:-0}" != "1" ]]; then
  # dist/ holds this script's own output; a previous release must not make the
  # tree count as dirty.
  if [[ -n "$(git -C "$ROOT" status --porcelain | grep -v ' dist/' || true)" ]]; then
    git -C "$ROOT" status --short >&2
    fail "working tree is dirty; commit/stash first (or VOCARA_ALLOW_DIRTY=1 for local testing)"
  fi
fi

VERSION="$(sed -n 's/^version: *\([0-9][^+ ]*\).*/\1/p' "$CLIENT/pubspec.yaml")"
[[ -n "$VERSION" ]] || fail "could not read version from client/pubspec.yaml"
DMG="$DIST/Vocara-$VERSION.dmg"
echo "version $VERSION -> $DMG"

# --- 1. Universal release build --------------------------------------------

step "Building universal prod release"
(cd "$CLIENT" && VOCARA_UNIVERSAL=1 flutter build macos --release --flavor prod)
[[ -d "$APP" ]] || fail "expected app bundle missing: $APP"

# Both the app and the embedded Rust engine must be fat, or Intel users get a
# dead app / dead audio. Hard-fail rather than ship a broken artifact.
step "Verifying universal binaries"
for bin in \
  "$APP/Contents/MacOS/vocara" \
  "$APP/Contents/Frameworks/vocara_engine.framework/Versions/A/vocara_engine"; do
  lipo -info "$bin"
  lipo "$bin" -verify_arch x86_64 arm64 || fail "not universal: $bin"
done

# --- 2. Deep re-sign, inside-out -------------------------------------------
# Hardened runtime + secure timestamp on every Mach-O, innermost first, so the
# outer seals stay valid. Required for notarization; Xcode's own signing does
# not re-sign third-party frameworks (mpv/ffmpeg stack) with these flags.

step "Re-signing (hardened runtime + timestamp), inside-out"
SIGN=(codesign --force --timestamp --options runtime --sign "$IDENTITY")

# 1) Loose dylibs nested inside frameworks (ffmpeg/mpv libs and friends).
while IFS= read -r -d '' bin; do
  "${SIGN[@]}" "$bin"
done < <(find "$APP/Contents/Frameworks" -type f \( -name "*.dylib" -o -name "*.so" \) -print0)

# 2) The framework bundles themselves.
while IFS= read -r -d '' fw; do
  "${SIGN[@]}" "$fw"
done < <(find "$APP/Contents/Frameworks" -maxdepth 1 -name "*.framework" -print0)

# 3) Any bundled helper executables (none today; harmless if absent).
for dir in "$APP/Contents/Helpers" "$APP/Contents/XPCServices"; do
  [[ -d "$dir" ]] || continue
  while IFS= read -r -d '' bin; do
    "${SIGN[@]}" "$bin"
  done < <(find "$dir" -type f -perm +111 -print0)
done

# 4) The app itself, last, with its sandbox entitlements.
"${SIGN[@]}" --entitlements "$ENTITLEMENTS" "$APP"

step "Verifying signature"
codesign --verify --deep --strict --verbose=2 "$APP"
codesign --display --verbose=2 "$APP" 2>&1 | grep -E "Authority=Developer ID|flags=.*runtime" \
  || fail "app is not Developer ID signed with hardened runtime"

# --- 3. DMG -----------------------------------------------------------------

step "Creating DMG"
mkdir -p "$DIST"
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
ditto "$APP" "$STAGE/vocara.app"
ln -s /Applications "$STAGE/Applications"
rm -f "$DMG"
hdiutil create -volname "Vocara" -srcfolder "$STAGE" -fs HFS+ -format UDZO -ov "$DMG"
codesign --force --timestamp --sign "$IDENTITY" "$DMG"

if [[ "${VOCARA_SKIP_NOTARIZE:-0}" == "1" ]]; then
  echo ""
  echo "VOCARA_SKIP_NOTARIZE=1: stopping before notarization."
  echo "WARNING: $DMG is signed but NOT notarized; Gatekeeper will reject it"
  echo "on other machines. Do not distribute this artifact."
  exit 0
fi

# --- 4. Notarize + staple ----------------------------------------------------

step "Notarizing"
NOTARY_ARGS=()
if [[ -n "${NOTARY_KEY_PATH:-}" ]]; then
  # CI path: App Store Connect API key from secrets.
  [[ -n "${NOTARY_KEY_ID:-}" && -n "${NOTARY_ISSUER_ID:-}" ]] \
    || fail "NOTARY_KEY_PATH set but NOTARY_KEY_ID / NOTARY_ISSUER_ID missing"
  NOTARY_ARGS=(--key "$NOTARY_KEY_PATH" --key-id "$NOTARY_KEY_ID" --issuer "$NOTARY_ISSUER_ID")
else
  # Local path: keychain profile. Detect its absence up front and say exactly
  # what to run once (needs an app-specific password from account.apple.com).
  if ! xcrun notarytool history --keychain-profile "$NOTARY_PROFILE" >/dev/null 2>&1; then
    cat >&2 <<EOF

No notarytool keychain profile '$NOTARY_PROFILE' found (or it is unusable).
Run this ONCE, then re-run the release script:

  xcrun notarytool store-credentials $NOTARY_PROFILE \\
    --apple-id "<your Apple ID email>" \\
    --team-id TP7LCYW3NY \\
    --password "<app-specific password from account.apple.com>"

EOF
    fail "notarytool credentials missing"
  fi
  NOTARY_ARGS=(--keychain-profile "$NOTARY_PROFILE")
fi

# notarytool's exit code is not reliable across Xcode versions for a rejected
# submission, so check the reported status explicitly.
SUBMIT_LOG="$(mktemp)"
xcrun notarytool submit "$DMG" "${NOTARY_ARGS[@]}" --wait 2>&1 | tee "$SUBMIT_LOG"
grep -q "status: Accepted" "$SUBMIT_LOG" \
  || fail "notarization not accepted; inspect with: xcrun notarytool log <submission-id> ${NOTARY_ARGS[*]}"

step "Stapling"
xcrun stapler staple "$DMG"

# --- 5. Final Gatekeeper check ----------------------------------------------

step "Gatekeeper assessment"
spctl --assess --type open --context context:primary-signature -v "$DMG" \
  || fail "Gatekeeper rejected the stapled DMG"

echo ""
echo "RELEASE OK: $DMG"
echo "Next: verify on a second machine (see docs/release-runbook.md)."
