//! Microphone permission (port of `lib/data/services/mic_permission.dart` and
//! the Runner's `vocara/mic` channel in `MainFlutterWindow.swift`).
//!
//! The native engine captures via CoreAudio (raw HAL), which macOS feeds
//! SILENCE when the app lacks microphone permission — and raw HAL access,
//! unlike AVFoundation, never triggers the permission prompt or lists the
//! app under Privacy → Microphone. Requesting through AVFoundation (on a
//! foreground user action) is what actually prompts and grants, so the
//! capture gets real audio.

/// What a mic action says when access is off.
pub const DENIED_MESSAGE: &str =
    "Microphone access is off. Enable it in System Settings › Privacy & Security › Microphone.";

/// Requests microphone access (prompts on first use) and returns whether it
/// is granted. Blocks until the user answers the prompt: call it off the UI
/// thread. Off macOS there is nothing to ask, and capture is not blocked.
pub fn request() -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::request()
    }
    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}

/// A short status string for diagnostics: the grant state.
pub fn diagnostic() -> String {
    #[cfg(target_os = "macos")]
    {
        format!(
            "perm={}",
            if macos::authorized() {
                "granted"
            } else {
                "denied"
            }
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        "perm-channel-MISSING".to_string()
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::sync::mpsc;

    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};

    pub fn request() -> bool {
        let (tx, rx) = mpsc::channel::<bool>();
        let block = RcBlock::new(move |granted: Bool| {
            let _ = tx.send(granted.as_bool());
        });
        // SAFETY: AVMediaTypeAudio is a valid extern static NSString, and the
        // completion block outlives the call (AVFoundation copies it).
        unsafe {
            let Some(media) = AVMediaTypeAudio else {
                return true;
            };
            AVCaptureDevice::requestAccessForMediaType_completionHandler(media, &block);
        }
        rx.recv().unwrap_or(false)
    }

    pub fn authorized() -> bool {
        // SAFETY: as above; the call has no other preconditions.
        unsafe {
            let Some(media) = AVMediaTypeAudio else {
                return false;
            };
            AVCaptureDevice::authorizationStatusForMediaType(media)
                == AVAuthorizationStatus::Authorized
        }
    }
}
