# Release runbook

How to cut a Vocara macOS release. Releases go through GitHub Actions ONLY
(the `Release` workflow, `.github/workflows/release.yml`); there is no local
release script. The artifact is a signed, notarized, stapled universal
(arm64 + x86_64) DMG attached to a GitHub release: `Vocara-<version>.dmg`.
This repository is private, so the release users download, and the
`latest.json` the app's update check reads, are published to the public
releases repo `RutaTang/vocara-releases` (`RELEASES_REPO` in the workflow).

## Bundle-id freeze warning

`com.rutatang.vocara` becomes the app's permanent data identity at the first
real install: the sandbox container (`~/Library/Containers/com.rutatang.vocara`),
keychain items, and the TCC microphone grant are all keyed off it. Changing it
after release strands every user's data. It is frozen as of v1.0.0. (Dev/test
flavors use `com.rutatang.vocara.dev` / `.test` and are not affected.)

## One-time setup

Add these repository secrets (Settings → Secrets and variables → Actions).
The names match the convention used by the owner's other release pipelines,
and notarization authenticates with Apple ID + app-specific password (the
same method `notarytool` also supports via an App Store Connect API key —
either works; this repo standardizes on the former).

| Secret | How to get it |
| --- | --- |
| `MACOS_CERTIFICATE` | Keychain Access → My Certificates → right-click `Developer ID Application: Weizhi Tang (TP7LCYW3NY)` → Export as `.p12` with a password, then `base64 -i cert.p12 | pbcopy`. If the cert is ever lost, re-create it at <https://developer.apple.com/account/resources/certificates> (the private key only exists where the CSR was made, so keep a `.p12` backup). |
| `MACOS_CERTIFICATE_PASSWORD` | the password chosen during that export |
| `APPLE_ID` | the Apple ID email of the developer account |
| `APPLE_TEAM_ID` | `TP7LCYW3NY` (shown on the certificate and at <https://developer.apple.com/account> → Membership) |
| `APPLE_APP_SPECIFIC_PASSWORD` | <https://account.apple.com> → Sign-In and Security → App-Specific Passwords → generate one (e.g. named `vocara-ci`) |
| `RELEASES_TOKEN` | <https://github.com/settings/personal-access-tokens> → Generate new token (fine-grained) → Repository access: only `RutaTang/vocara-releases` → Permissions: Contents: Read and write. It expires: renew it before then, or the next release stops at its guard. |

Then create the public releases repo itself: <https://github.com/new>, name
`vocara-releases`, **Public**, with a README (a release needs a commit to
tag, and an empty repo has none). The workflow's guard checks all of it
(token set and allowed to create releases there, repo public, repo not
empty) before it builds anything; its write check creates a draft release
and deletes it at once.

## Cutting a release

1. Bump `version:` in `client/pubspec.yaml`; commit to master.
2. `git tag -a v<version> -m "Vocara <version>" -m "<what changed>" && git push origin v<version>`
   — the tag's message becomes the public release notes and the notes the
   app's update dialog shows (a lightweight `git tag` publishes none). The
   `Release` workflow runs the full pipeline:
   - **Guard**: the tag must sit on master HEAD and match the pubspec version.
   - **Validate**: format, analyze, unit tests, Rust engine tests (both
     feature modes) + clippy, and all twelve integration suites (the five
     UI journeys plus the seven service-level pipelines; the same list as
     `just e2e`).
   - **Build**: universal prod build → deep re-sign → DMG → notarize →
     staple → Gatekeeper assessment → launch the app from the DMG and check
     it boots inside its sandbox (`.github/scripts/launch-check.sh`).
   - **Publish**: the DMG, a `SHA256SUMS` file and `latest.json` as a
     release of the public releases repo, then the DMG and `SHA256SUMS` as
     this repo's GitHub release. A tag
     containing a `-` (e.g. `v1.0.0-rc.1`) publishes as a prerelease, which
     the update check never offers.
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

All of these live as steps of the `build-macos` job in
`.github/workflows/release.yml`:

- **Signing config** — Release/Release-{Dev,Test,Prod} configs of the Runner
  target use manual signing with `Developer ID Application`, team
  `TP7LCYW3NY`, and hardened runtime. Debug/Profile stay ad-hoc for dev speed.
- **Universal build** — the Rust engine builds arm64-only unless
  `VOCARA_UNIVERSAL=1` (see `client/macos/build_rust.sh`); the workflow sets
  it and hard-fails if either the app binary or the embedded `vocara_engine`
  is not fat.
- **Deep re-sign** — Xcode does not re-sign third-party frameworks (the
  mpv/ffmpeg stack in `media_kit`) with hardened runtime + timestamp, which
  notarization requires; the workflow re-signs every Mach-O inside-out before
  building the DMG.
- **Release entitlements** (`client/macos/Runner/Release.entitlements`):
  - `app-sandbox` — the shipping app is fully sandboxed.
  - `device.audio-input` — microphone (recording, live monitoring, analysis).
  - `network.client` — outbound HTTP for the update check.
  - `files.user-selected.read-only` / `.read-write` — open panels for imports,
    save panels for exports.

## In-app update check

`UpdateCheckService` reads
`https://github.com/RutaTang/vocara-releases/releases/latest/download/latest.json`
(once per run, and from Settings) and shows a notify-only banner (no
auto-install). GitHub serves that path from the newest release of the public
repo that is not a prerelease; the release workflow writes the file:

```json
{"latest": "1.2.0", "url": "https://github.com/RutaTang/vocara-releases/releases/tag/v1.2.0", "notes": "..."}
```

`update_check_manifest_sync_test.dart` fails if `manifestUrl` and the
workflow's `RELEASES_REPO` stop naming the same repo. v0.1.0 read this
private repo's releases API instead, which answers 404 without a login, so
v0.1.0 installs never see an update: their users need the next DMG by hand.
Auto-update via Sparkle was considered and deliberately deferred.

## Troubleshooting

- Notarization rejected: the workflow's Notarize step prints
  `notarytool log` output for the failed submission automatically (usually a
  new unsigned dylib — the deep re-sign step should catch it; check its find
  patterns still match the bundle layout).
- Testing the release build locally without notarizing:
  `cd client && VOCARA_UNIVERSAL=1 flutter build macos --release --flavor prod`
  builds the same app bundle the workflow signs. An unsigned/un-notarized
  local build is rejected by `spctl` — expected; Gatekeeper only accepts
  after notarize + staple.
