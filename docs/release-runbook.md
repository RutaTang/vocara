# Release runbook

How to cut a Vocara macOS release. The artifact is a signed, notarized,
stapled universal (arm64 + x86_64) DMG: `dist/Vocara-<version>.dmg`.

## Bundle-id freeze warning

`com.rutatang.vocara` becomes the app's permanent data identity at the first
real install: the sandbox container (`~/Library/Containers/com.rutatang.vocara`),
keychain items, and the TCC microphone grant are all keyed off it. Changing it
after release strands every user's data. It is frozen as of v1.0.0. (Dev/test
flavors use `com.rutatang.vocara.dev` / `.test` and are not affected.)

## One-time setup (local releases)

1. **Developer ID certificate** — already in the login keychain:
   `Developer ID Application: Weizhi Tang (TP7LCYW3NY)`. If it is ever lost,
   re-download or re-create it at <https://developer.apple.com/account/resources/certificates>
   (the private key only exists where the CSR was made, so keep a `.p12` backup).
2. **Notary credentials** — create an app-specific password at
   <https://account.apple.com> (Sign-In and Security → App-Specific Passwords),
   then run once:

   ```sh
   xcrun notarytool store-credentials vocara-notary \
     --apple-id "<your Apple ID email>" \
     --team-id TP7LCYW3NY \
     --password "<app-specific password>"
   ```

   `scripts/release.sh` detects a missing profile and prints this command.
3. **Intel Rust target** — `rustup target add x86_64-apple-darwin`.

## One-time setup (CI releases)

Add these repository secrets (Settings → Secrets and variables → Actions):

| Secret | How to get it |
| --- | --- |
| `DEVELOPER_ID_P12` | Keychain Access → My Certificates → right-click the Developer ID Application cert → Export as `.p12` with a password, then `base64 -i cert.p12 | pbcopy` |
| `P12_PASSWORD` | the password chosen during that export |
| `NOTARY_KEY` | App Store Connect → Users and Access → Integrations → App Store Connect API → Team Keys → create a key with **Developer** role; paste the downloaded `.p8` file contents |
| `NOTARY_KEY_ID` | key ID shown next to that key |
| `NOTARY_ISSUER_ID` | issuer ID shown at the top of the Team Keys page |

The `.p8` can only be downloaded once — keep it somewhere safe too.

## Cutting a release

1. Bump `version:` in `client/pubspec.yaml`; commit everything (the release
   script refuses a dirty tree).
2. Either:
   - **CI**: `git tag v<version> && git push origin v<version>` — the
     `release` workflow builds, signs, notarizes, staples, and attaches the
     DMG to a GitHub release. The tag must match the pubspec version.
   - **Local**: `scripts/release.sh` from the repo root; the DMG lands in
     `dist/`.
3. **Verify on a second machine** (or at minimum a different user account):
   download the DMG through a browser so it is quarantined, then:

   ```sh
   spctl --assess --type open --context context:primary-signature -v Vocara-<version>.dmg
   # expect: accepted, source=Notarized Developer ID
   ```

   Drag the app to `/Applications`, launch it from Finder (first launch must
   show no Gatekeeper block), record a take (mic prompt appears once), and
   confirm audio works. On an Intel Mac, or with
   `arch -x86_64 /Applications/vocara.app/Contents/MacOS/vocara`, confirm the
   x86_64 slice runs and audio is alive (the Rust engine must be fat).

## What the pieces do

- **Signing config** — Release/Release-{Dev,Test,Prod} configs of the Runner
  target use manual signing with `Developer ID Application`, team
  `TP7LCYW3NY`, and hardened runtime. Debug/Profile stay ad-hoc for dev speed.
- **Universal build** — the Rust engine builds arm64-only unless
  `VOCARA_UNIVERSAL=1` (see `client/macos/build_rust.sh`); the release script
  sets it and hard-fails if either the app binary or the embedded
  `vocara_engine` is not fat.
- **Deep re-sign** — Xcode does not re-sign third-party frameworks (the
  mpv/ffmpeg stack in `media_kit`) with hardened runtime + timestamp, which
  notarization requires; the script re-signs every Mach-O inside-out before
  building the DMG.
- **Release entitlements** (`client/macos/Runner/Release.entitlements`):
  - `app-sandbox` — the shipping app is fully sandboxed.
  - `device.audio-input` — microphone (recording, live monitoring, analysis).
  - `network.client` — outbound HTTP for the update check.
  - `files.user-selected.read-only` / `.read-write` — open panels for imports,
    save panels for exports.

## Troubleshooting

- `notarytool submit` rejected: `xcrun notarytool log <submission-id>
  --keychain-profile vocara-notary` lists the offending binaries (usually a
  new unsigned dylib — the deep re-sign in `scripts/release.sh` should catch
  it; check the find patterns still match the bundle layout).
- `spctl` rejects a locally built app before notarization: expected.
  Gatekeeper only accepts after notarize + staple (or an online ticket check).
