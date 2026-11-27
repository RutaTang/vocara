# Release runbook

How to cut a Vocara macOS release. Releases go through GitHub Actions ONLY
(the `Release` workflow, `.github/workflows/release.yml`); there is no local
release script. The artifact is a signed, notarized, stapled universal
(arm64 + x86_64) DMG attached to a GitHub release: `Vocara-<version>.dmg`.
The release users download, and the `latest.json` the app's update check
reads, are this public repository's (`RELEASES_REPO` in the workflow; its
guard checks that it is this repository and that it is public), published
with the workflow's own token: no other repository or token is needed.

## Bundle-id freeze warning

`com.rutatang.vocara` becomes the app's permanent data identity at the first
real install: the library folder (`~/Library/Application Support/com.rutatang.vocara`),
the App Sandbox container releases up to 0.3.0 kept it in
(`~/Library/Containers/com.rutatang.vocara`, which the first launch of a later
release moves it out of — see [Leaving the App Sandbox](#leaving-the-app-sandbox)),
keychain items, and the TCC microphone grant are all keyed off it. Changing it
after release strands every user's data. So does changing the signing team:
macOS 14 and later only let the container's owner (the same bundle id, signed by
the same team) read a sandboxed app's container without asking. It is frozen as
of v1.0.0. (Dev/test flavors use `com.rutatang.vocara.dev` / `.test` and are not
affected.)

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

## Cutting a release

1. Bump the workspace `version` in `rust/Cargo.toml`, and with it
   `UpdateCheckService::APP_VERSION` (the same version: a test pins the two
   together, so a stale one fails the release's Validate phase after the tag
   is pushed) and `UpdateCheckService::APP_BUILD_NUMBER` (the bundle's
   `CFBundleVersion`), both in
   `rust/crates/vocara_services/src/update_check_service.rs`; commit to master.
2. `git tag -a v<version> -m "Vocara <version>" -m "<what changed>" && git push origin v<version>`
   — the tag's message becomes the public release notes and the notes the
   app's update dialog shows (a lightweight `git tag` publishes none). The
   `Release` workflow runs the full pipeline:
   - **Guard**: the tag must sit on master HEAD and match the workspace
     version, and this repository must be public (the app reads its updates
     without a login).
   - **Validate**: fmt, clippy and the tests of the whole Rust workspace in
     both feature modes (the app's journeys run as headless iced tests), and
     the Lean proofs with their vectors (the same as `just check` and
     `just proofs`).
   - **Build**: universal prod build → deep re-sign → DMG → notarize →
     staple → Gatekeeper assessment → launch the app from the DMG and check
     it boots unsandboxed, under the hardened runtime, with its library in
     `~/Library/Application Support/com.rutatang.vocara` → then update to it
     from the last sandboxed release (v0.3.0's published DMG, launched first
     so that it makes a real container and library) and check the library
     moved out of the container whole and opened
     (`.github/scripts/launch-check.sh`, `--update` for the second). That
     release must be published (`LAST_SANDBOXED_RELEASE` in the script).
   - **Publish**: the DMG, a `SHA256SUMS` file and `latest.json` as this
     repository's GitHub release. A tag containing a `-` (e.g.
     `v1.0.0-rc.1`) publishes as a prerelease, which the update check never
     offers.
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

   For the first release after 0.3.0, also update a Mac that has a library
   in 0.3.0: replace the app in `/Applications` with the new one, launch it,
   and confirm the library is all there, that
   `~/Library/Application Support/com.rutatang.vocara` now holds it, that
   `~/Library/Containers/com.rutatang.vocara/Data/Library/Application Support`
   no longer does, that no "would like to access data from other apps"
   prompt appeared, and that its diagnostics log (in that folder) says
   `[storage] moved the library out of the App Sandbox container`.

## What the pieces do

All of these live as steps of the `build-macos` job in
`.github/workflows/release.yml`:

- **Bundle** — `rust/packaging/macos/bundle.sh --flavor prod --universal`
  builds the app for arm64 and x86_64, joins them with `lipo`, and lays out
  `vocara.app` (Info.plist with the flavor's bundle id and the workspace
  version, the icon, an ad-hoc signature). The flavor is baked into the
  binary at compile time. The workflow hard-fails if the executable is not
  fat, or if it could not load on macOS 10.15, the app's minimum
  (`.github/scripts/macos-minimum-check.sh`: the x86_64 slice is built for
  10.15, arm64 for 11.0, and they link only what 10.15 ships).
- **Signing** — the workflow re-signs with `Developer ID Application`, team
  `TP7LCYW3NY`, hardened runtime and a secure timestamp, which notarization
  requires; any framework or dylib bundled later is signed inside-out first.
- **Release entitlements** (`rust/packaging/macos/Release.entitlements`; the
  file's comment says why each key is there and what re-adding one takes):
  - `device.audio-input` — microphone (recording, live monitoring, analysis):
    under the hardened runtime the mic cannot open without it.
  - Nothing else. The app is not App-Sandboxed (it replaces its own bundle
    when it updates, which a sandboxed app cannot), so the sandbox key and the
    sandbox-only ones releases up to 0.3.0 carried (`network.client`,
    `files.user-selected.read-only` / `.read-write`) are gone. `bundle.sh`
    signs every local bundle with the same file (a release one with the
    hardened runtime too, as here).

## Leaving the App Sandbox

Every release up to 0.3.0 was sandboxed, so it kept everything — the library
(database, asset pool, recordings, technique media), the settings, the stems
cache and separation models, the diagnostics log — in its container:
`~/Library/Containers/<bundle id>/Data/Library/Application Support/<bundle id>`.
Later releases resolve the same folder to
`~/Library/Application Support/<bundle id>`. Before anything opens a store,
their first launch moves that one folder there with a single atomic rename
(`rust/crates/vocara_services/src/sandbox_migration.rs`, whose doc has the
whole decision table):

- container holds data, Application Support holds nothing (missing, or empty
  but for Finder's `.DS_Store`): **moved**;
- Application Support already holds a library (someone ran an unsandboxed
  build of the same flavor beside a sandboxed release): **it opens**, and the
  container's is left exactly as it is, nothing merged or overwritten
  (logged);
- the move cannot be done — another Vocara (an older, sandboxed one) still
  has the library open, the folders are on different volumes, the system
  refuses, Application Support holds stray files but no library — **the
  library stays in the container and the whole app opens it there** for that
  launch (logged with the reason); a later launch moves it if it then can.
  The user never meets an empty library while theirs sits in the container.

A crash at any point leaves the folder whole in one place; a second launch
finds it where it is. Only that folder moves: the rest of the container
(preferences, saved window state, caches) is the system's and stays. Every
outcome but "nothing to move" is logged under `[storage]` in the diagnostics
log of the folder the app opened.

macOS 14 and later protect a sandboxed app's container ("App Data"): another
app reading it asks the user. The container's owner — the same bundle id,
signed by the same team, which every Developer ID release is — is expected to
read it without asking; the release workflow's update check proves it on the
notarized build before each release is published. A build signed otherwise (an
ad-hoc local bundle, or CI's manual `release-launch` job) may be asked about or
refused; refused, the app opens the container, fails there and says why in its
fatal window, rather than opening an empty library.

## In-app update check

`UpdateCheckService` reads
`https://github.com/RutaTang/vocara/releases/latest/download/latest.json`
(once per run, and from Settings) and shows a notify-only banner (no
auto-install). GitHub serves that path from the newest release of this
repository that is not a prerelease; the release workflow writes the file:

```json
{"latest": "1.2.0", "url": "https://github.com/RutaTang/vocara/releases/tag/v1.2.0", "notes": "..."}
```

`rust/crates/vocara_services/tests/update_check_pins.rs` fails if `MANIFEST_URL` and the
workflow's `RELEASES_REPO` stop naming the same repo. v0.1.0 reads this
repository's releases API (`api.github.com/repos/RutaTang/vocara/releases/latest`:
the release's tag, page and body, which is the tag's message), which
answers without a login now that the repository is public, so v0.1.0
installs are offered the next release too.
Auto-update via Sparkle was considered and deliberately deferred.

## Troubleshooting

- Notarization rejected: the workflow's Notarize step prints
  `notarytool log` output for the failed submission automatically (usually a
  new unsigned dylib — the deep re-sign step should catch it; check its find
  patterns still match the bundle layout).
- Testing the release build locally without notarizing:
  `rust/packaging/macos/bundle.sh --flavor prod --universal` builds the
  same app bundle the workflow signs (ad hoc, with the hardened runtime and
  `Release.entitlements`). An un-notarized
  local build is rejected by `spctl` — expected; Gatekeeper only accepts
  after notarize + staple.
- The update check failed ("the library did not move out of v0.3.0's
  container"): the error quotes the app's own `[storage]` entry, which says
  why the library stayed in the container (another Vocara had it open, the
  move failed and how, ...). If the app logged nothing, it never got past its
  start; on macOS 14 and later that is the container's App Data protection
  holding a build it does not take for the container's owner. On the
  notarized build that is a real failure: do not publish. On CI's manual
  ad-hoc job it is the signature. "could not download Vocara-0.3.0.dmg":
  the last sandboxed release is not published (or `LAST_SANDBOXED_RELEASE` in
  `.github/scripts/launch-check.sh` names the wrong one).
