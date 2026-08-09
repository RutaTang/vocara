# Resound — task runner. Run `just` to list recipes.
# Prereqs: flutter, just, fswatch (brew install just fswatch)

default:
    @just --list

# The build is flavor-only, so a flavor is always required. fswatch pipes "r"
# to flutter run for hot reload on lib/ changes.
# Run the DEV flavor on macOS with hot-reload.
dev:
    #!/bin/bash
    cd client
    (sleep 5 && fswatch -o lib/ | while read; do echo "r"; done) | flutter run --flavor dev -d macos

# Run an arbitrary flavor without hot-reload, e.g. `just run prod` / `just run test`.
run flavor="dev":
    cd client && flutter run --flavor {{flavor}} -d macos

# Static analysis
analyze:
    cd client && flutter analyze

# Run tests
test:
    cd client && flutter test

# All run on the TEST flavor, so automated runs use an isolated sandbox store
# and never touch dev or prod data.
# Every integration suite: UI journeys plus service-level pipelines.
e2e: e2e-home e2e-library e2e-detail e2e-technique e2e-settings e2e-pipelines

# Home / settings / library navigation — boots on Home, no deep-link.
e2e-home:
    cd client && flutter test integration_test/home_settings_journey_test.dart -d macos --flavor test

# Technique library list (create / search / tag filter) — deep-links to /techniques.
e2e-library:
    cd client && flutter test integration_test/technique_library_journey_test.dart -d macos --flavor test --dart-define=START=/techniques

# START must match the seed id the suite inserts (kSeedPracticeId).
# Song-practice detail arrange surface.
e2e-detail:
    cd client && flutter test integration_test/detail_arrange_journey_test.dart -d macos --flavor test --dart-define=START=/song/E2EPRACTICESEED00000000000

# Technique detail editor — deep-links into the seeded technique (kSeedTechniqueId).
e2e-technique:
    cd client && flutter test integration_test/technique_detail_journey_test.dart -d macos --flavor test --dart-define=START=/techniques/E2ETECHNIQUESEED0000000000

# Settings screen interactions (reverb slider) — deep-links to /settings.
e2e-settings:
    cd client && flutter test integration_test/settings_journey_test.dart -d macos --flavor test --dart-define=START=/settings

# These need the real embedder (native engine decode/playback, path_provider, a
# real drift file) but drive services and repositories directly rather than the
# widget tree. Kept inside `just e2e` because an integration suite nothing runs
# is an integration suite that rots — these sat unwired long enough for one of
# them to go stale against an API that had moved on.
# The service-level integration suites (no widget tree).
e2e-pipelines: e2e-audio e2e-analysis e2e-persistence e2e-sync e2e-separation e2e-techlib e2e-ui

# Import → waveform analysis → multi-track playback, through the native engine.
e2e-audio:
    cd client && flutter test integration_test/audio_pipeline_test.dart -d macos --flavor test

# DSP + spectrogram + MediaAnalysisService against generated tones.
e2e-analysis:
    cd client && flutter test integration_test/analysis_pipeline_test.dart -d macos --flavor test

# View-model → drift → reload round-trips, asset dedup and the GC sweep.
e2e-persistence:
    cd client && flutter test integration_test/persistence_roundtrip_test.dart -d macos --flavor test

# Sync applier / row builder against a real drift store.
e2e-sync:
    cd client && flutter test integration_test/sync_roundtrip_test.dart -d macos --flavor test

# Stem separation service (skips itself when the Python sidecar is absent).
e2e-separation:
    cd client && flutter test integration_test/source_separation_test.dart -d macos --flavor test

# Technique library + media draft + ground-truth analysis, service level.
e2e-techlib:
    cd client && flutter test integration_test/technique_library_test.dart -d macos --flavor test

# Home → new practice → detail → back, on the real app tree.
e2e-ui:
    cd client && flutter test integration_test/ui_journey_test.dart -d macos --flavor test

# Format code
fmt:
    cd client && dart format lib test integration_test

# Fetch dependencies
get:
    cd client && flutter pub get

# Remove build artifacts
clean:
    cd client && flutter clean
