//! The domain model (ported from `lib/domain/models`).
//!
//! Conventions, the same for every type:
//! - value types derive `Clone, Debug, PartialEq` with Dart's `==` (every
//!   field), and their fields are public: Dart's `copyWith` is struct update
//!   syntax (`AudioTrack { muted: true, ..track.clone() }`), and its
//!   `clearX: true` flags are a plain `x: None`;
//! - `to_json(&self) -> serde_json::Value` writes the keys in the order of
//!   Dart's map literal, a Dart `double` as a JSON double and an `int` as an
//!   integer, so `util::dart_json::encode` of it is byte-identical to the
//!   Flutter app's `jsonEncode`;
//! - `from_json(&Value) -> Result<Self>` is as lenient as the Dart factory
//!   (missing keys, legacy shapes, clamping) and fails where its casts throw;
//! - timestamps are [`crate::util::dart_date_time::DateTime`], which reads
//!   and writes the ISO text exactly as Dart's `DateTime` does.

pub mod audio_device;
pub mod audio_fx;
pub mod audio_track;
pub mod fx_bus;
pub mod fx_presets;
mod json;
pub mod lyric_line;
pub mod practice_note;
pub mod practice_summary;
pub mod song_practice;
pub mod spectrogram;
pub mod take_version;
pub mod technique;
pub mod technique_analysis;
pub mod technique_marker;
pub mod technique_media;
pub mod voice_metrics;
