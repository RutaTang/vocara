//! A technique's example clip (port of
//! `lib/domain/models/technique_media.dart`).

use serde_json::{json, Value};

use super::json::{as_map, opt_str, req_str};
use crate::util::dart_date_time::DateTime;
use crate::util::{dart_path, dart_string};
use crate::Result;

/// The kind of a technique example clip, which decides whether it plays in an
/// audio bar or a video surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TechniqueMediaKind {
    Audio,
    Video,
}

/// An audio or video example clip attached to a technique. The file is copied
/// into the app sandbox on import, so [path] is an absolute sandbox path the
/// technique owns (it never points at the user's original file). (Dart's
/// `copyWith(label:)` is `TechniqueMedia { label, ..media }`.)
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TechniqueMedia {
    pub id: String,
    /// Absolute sandbox path to the copied media file.
    pub path: String,
    pub kind: TechniqueMediaKind,
    /// Optional caption shown under/next to the clip (defaults to the file name
    /// when blank).
    pub label: String,
    pub added_at: DateTime,
}

/// File extensions recognized as video; everything else importable is treated
/// as audio.
const VIDEO_EXTENSIONS: [&str; 6] = [".mp4", ".mov", ".m4v", ".webm", ".mkv", ".avi"];

const AUDIO_EXTENSIONS: [&str; 12] = [
    ".mp3", ".m4a", ".aac", ".wav", ".aif", ".aiff", ".flac", ".ogg", ".oga", ".caf",
    // Played by media_kit, not the engine: an example the engine cannot
    // decode still plays, and only its analysis fails (unlike a song
    // import, which the engine must play — see the song screen's list).
    ".opus", ".wma",
];

/// Audio media_kit plays but the engine has no decoder for: an example in
/// one plays, and its analysis is never attempted.
const PLAY_ONLY_EXTENSIONS: [&str; 2] = [".opus", ".wma"];

impl TechniqueMedia {
    pub fn is_video(&self) -> bool {
        self.kind == TechniqueMediaKind::Video
    }

    /// The label to display: the user's caption, or the file name as a fallback
    /// — unless the app named the file (an import's generated id, or the
    /// content hash the asset pool stores it under), which says nothing to the
    /// user: an uncaptioned example then reads by its kind.
    pub fn display_label(&self) -> String {
        let caption = dart_string::trim(&self.label);
        if !caption.is_empty() {
            return caption.to_string();
        }
        if is_generated_name(&dart_path::basename_without_extension(&self.path)) {
            return if self.is_video() {
                "Video example"
            } else {
                "Audio example"
            }
            .to_string();
        }
        dart_path::basename(&self.path)
    }

    pub fn from_json(json: &Value) -> Result<TechniqueMedia> {
        let j = as_map(json, "TechniqueMedia")?;
        Ok(TechniqueMedia {
            id: req_str(j, "id")?,
            path: req_str(j, "path")?,
            kind: if opt_str(j, "kind")?.as_deref() == Some("video") {
                TechniqueMediaKind::Video
            } else {
                TechniqueMediaKind::Audio
            },
            label: opt_str(j, "label")?.unwrap_or_default(),
            added_at: DateTime::parse(&req_str(j, "addedAt")?)?,
        })
    }

    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "path": self.path,
            "kind": if self.is_video() { "video" } else { "audio" },
            "label": self.label,
            "addedAt": self.added_at.to_iso8601_string(),
        })
    }

    /// Whether the engine can decode [path] for analysis.
    pub fn is_analyzable(path: &str) -> bool {
        !PLAY_ONLY_EXTENSIONS.contains(&lower_extension(path).as_str())
    }

    /// Whether [path] looks like an importable audio or video file.
    pub fn is_supported_media(path: &str) -> bool {
        let ext = lower_extension(path);
        VIDEO_EXTENSIONS.contains(&ext.as_str()) || AUDIO_EXTENSIONS.contains(&ext.as_str())
    }

    /// Every importable extension WITHOUT the leading dot, for a file picker's
    /// type filter. Derived from the same sets as [`TechniqueMedia::is_supported_media`] so the Browse
    /// dialog and drag-drop can never accept different formats (audio first,
    /// as Dart's `{..._audioExtensions, ..._videoExtensions}`).
    pub fn picker_extensions() -> Vec<String> {
        AUDIO_EXTENSIONS
            .iter()
            .chain(VIDEO_EXTENSIONS.iter())
            .map(|e| e[1..].to_string())
            .collect()
    }

    /// Classifies a file path as audio or video by its extension.
    pub fn kind_for_path(path: &str) -> TechniqueMediaKind {
        if VIDEO_EXTENSIONS.contains(&lower_extension(path).as_str()) {
            TechniqueMediaKind::Video
        } else {
            TechniqueMediaKind::Audio
        }
    }
}

fn lower_extension(path: &str) -> String {
    dart_path::extension(path).to_lowercase()
}

/// `^(?:[0-9a-f]{64}|[0-9A-HJKMNP-TV-Z]{26})$`: a 64-hex content hash, or a
/// 26-character ULID.
fn is_generated_name(name: &str) -> bool {
    let b = name.as_bytes();
    let hex = b.len() == 64 && b.iter().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'));
    let ulid = b.len() == 26
        && b.iter().all(|c| {
            matches!(c, b'0'..=b'9' | b'A'..=b'H' | b'J' | b'K' | b'M' | b'N' | b'P'..=b'T' | b'V'..=b'Z')
        });
    hex || ulid
}
