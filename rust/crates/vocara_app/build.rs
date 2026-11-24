//! Link settings for the `vocara` binary.
//!
//! On macOS, CoreAudio is linked weakly. cpal 0.18's loopback capture
//! imports `AudioHardwareCreateProcessTap` and
//! `AudioHardwareDestroyProcessTap`, which macOS has only from 14.2, and
//! every cpal stream's drop glue calls the second (for a loopback device the
//! stream never has here). A strong import of a symbol the system lacks
//! stops dyld from loading the app at all, before macOS 14.2 (on 10.15, the
//! app's minimum, as on 11 to 14.1). Weakly linked, those two are null
//! there and every other CoreAudio symbol binds as before (all are in
//! 10.15). Nothing calls the two: the engine's only cpal input path, the one
//! that can make a loopback device, is compiled for non-Apple targets only
//! (macOS records through VoiceProcessingIO). CI checks that both, if
//! imported, are weak (`.github/scripts/macos-minimum-check.sh`).

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg-bins=-Wl,-weak_framework,CoreAudio");
    }
}
