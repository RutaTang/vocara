# Vocara — task runner. Run `just` to list recipes.
# Prereqs: a Rust toolchain (rustup), just; Lean 4 (the version in
# rust/verification/lean-toolchain) for `just proofs`.

default:
    @just --list

# Run the app on macOS with the live audio engine, e.g. `just run prod`.
# The flavor picks the bundle id, and with it the library folder, so dev,
# test and prod never share data.
run flavor="dev":
    cd rust && VOCARA_FLAVOR={{flavor}} cargo run -p vocara_app --features device

# Run the DEV flavor.
dev: (run "dev")

# Everything CI checks on the Rust side: format, lint, tests.
check: fmt-check lint test

# Format the workspace.
fmt:
    cd rust && cargo fmt --all

fmt-check:
    cd rust && cargo fmt --all --check

# Clippy in both feature modes (the device build compiles the engine's C ABI,
# CoreAudio, VoiceProcessingIO and the AVFoundation players).
lint:
    cd rust && cargo clippy --workspace --all-targets -- -D warnings
    cd rust && cargo clippy --workspace --all-targets --features vocara_app/device -- -D warnings

# The tests in both feature modes (the app's journeys run headless).
test:
    cd rust && cargo test --workspace
    cd rust && cargo test --workspace --features vocara_app/device

# Build the Lean proofs and regenerate the vectors the Rust tests replay.
proofs:
    cd rust/verification && lake build && lake exe vectors vectors

# Build the macOS app bundle, e.g. `just bundle prod` (see the script's flags).
bundle flavor="prod":
    rust/packaging/macos/bundle.sh --flavor {{flavor}}

# Remove build artifacts
clean:
    cd rust && cargo clean
