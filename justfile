# Resound — task runner. Run `just` to list recipes.
# Prereqs: flutter, just, fswatch (brew install just fswatch)

default:
    @just --list

# Run the DEV flavor on macOS with hot-reload on lib/ changes (fswatch pipes "r"
# to flutter run). The build is flavor-only, so a flavor is always required.
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

# Run all UI-driven end-to-end (integration) suites on macOS. These run on the
# TEST flavor so automated runs use an isolated sandbox store, never touching
# dev or prod data.
e2e: e2e-home e2e-library e2e-detail e2e-technique e2e-settings

# Home / settings / library navigation — boots on Home, no deep-link.
e2e-home:
    cd client && flutter test integration_test/home_settings_journey_test.dart -d macos --flavor test

# Technique library list (create / search / tag filter) — deep-links to /techniques.
e2e-library:
    cd client && flutter test integration_test/technique_library_journey_test.dart -d macos --flavor test --dart-define=START=/techniques

# Song-practice detail arrange surface — deep-links into the seeded practice.
# START must match the seed id the suite inserts (kSeedPracticeId).
e2e-detail:
    cd client && flutter test integration_test/detail_arrange_journey_test.dart -d macos --flavor test --dart-define=START=/song/E2EPRACTICESEED00000000000

# Technique detail editor — deep-links into the seeded technique (kSeedTechniqueId).
e2e-technique:
    cd client && flutter test integration_test/technique_detail_journey_test.dart -d macos --flavor test --dart-define=START=/techniques/E2ETECHNIQUESEED0000000000

# Settings screen interactions (reverb slider) — deep-links to /settings.
e2e-settings:
    cd client && flutter test integration_test/settings_journey_test.dart -d macos --flavor test --dart-define=START=/settings

# Format code
fmt:
    cd client && dart format lib test

# Fetch dependencies
get:
    cd client && flutter pub get

# Remove build artifacts
clean:
    cd client && flutter clean
