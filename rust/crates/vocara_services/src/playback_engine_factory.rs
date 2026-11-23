//! The app's playback engine (port of
//! `lib/data/services/playback_engine_factory.dart`).

use std::sync::Arc;

use crate::graph_audio_playback_service::GraphAudioPlaybackService;

/// Constructs the app's playback engine: the native real-time graph engine
/// ([`GraphAudioPlaybackService`]). If no output device opens, the engine
/// reports [`GraphAudioPlaybackService::is_available`] false and every call
/// no-ops, so the app stays usable (just silent) instead of failing.
///
/// Dart returned it typed as the `PlaybackEngine` interface and the song
/// page's view model checked `is GraphAudioPlaybackService` to reach the
/// device, mic and settings calls; the concrete `Arc` is returned here so the
/// view model keeps both views without a downcast (it coerces to
/// `Arc<dyn PlaybackEngine>` where only the transport is wanted).
pub fn create_playback_engine() -> Arc<GraphAudioPlaybackService> {
    Arc::new(GraphAudioPlaybackService::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playback_engine::PlaybackEngine;

    #[test]
    fn the_factory_makes_the_graph_engine_behind_the_seam() {
        let engine = create_playback_engine();
        let seam: Arc<dyn PlaybackEngine> = engine.clone();
        assert!(seam.supports_effects());
        // Without the device feature there is no engine: silent, not failing.
        #[cfg(not(feature = "device"))]
        assert!(!engine.is_available());
        seam.dispose();
    }
}
