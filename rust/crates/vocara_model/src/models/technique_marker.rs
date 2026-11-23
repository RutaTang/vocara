//! A technique annotation on a take's timeline (port of
//! `lib/domain/models/technique_marker.dart`).

use serde_json::{json, Value};

use super::json::{as_map, nullable, opt_int, opt_str, req_int, req_str};
use crate::util::dart_date_time::DateTime;
use crate::Result;

/// A technique annotation pinned to a vocal take at a point on the timeline.
///
/// Like a [`super::practice_note::PracticeNote`], it captures "what's happening at this moment" — but
/// instead of free text it references a technique from the user's technique
/// library, so the singer can mark where they're using (or aiming for) Twang,
/// a register shift, vibrato, etc. The marker is created at the playback cursor.
///
/// [technique_id] points at the library technique; [technique_name] is a snapshot
/// of its name at creation time so the marker still reads sensibly if the
/// library entry is later renamed or deleted. (Dart's `copyWith(clearEnd:
/// true)` is `TechniqueMarker { end_ms: None, ..marker }`.)
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TechniqueMarker {
    /// Stable unique identifier.
    pub id: String,
    /// Start (or, for a point marker, the only) position on the shared timeline,
    /// in milliseconds.
    pub position_ms: i64,
    /// End of the range, in milliseconds, or `None` for a point marker. When set it
    /// is always greater than [position_ms] (see [`TechniqueMarker::is_range`]) — marking a span the
    /// technique applies across, e.g. "keep twang through this phrase".
    pub end_ms: Option<i64>,
    /// The referenced library technique's id.
    pub technique_id: String,
    /// The referenced technique's name, snapshotted for display.
    pub technique_name: String,
    pub created_at: DateTime,
    pub updated_at: DateTime,
}

impl TechniqueMarker {
    /// True when this marker spans a range rather than a single point.
    pub fn is_range(&self) -> bool {
        self.end_ms.is_some_and(|end| end > self.position_ms)
    }

    pub fn from_json(json: &Value) -> Result<TechniqueMarker> {
        let j = as_map(json, "TechniqueMarker")?;
        Ok(TechniqueMarker {
            id: req_str(j, "id")?,
            position_ms: req_int(j, "positionMs")?,
            end_ms: opt_int(j, "endMs")?,
            technique_id: opt_str(j, "techniqueId")?.unwrap_or_default(),
            technique_name: opt_str(j, "techniqueName")?.unwrap_or_default(),
            created_at: DateTime::parse(&req_str(j, "createdAt")?)?,
            updated_at: DateTime::parse(&req_str(j, "updatedAt")?)?,
        })
    }

    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "positionMs": self.position_ms,
            "endMs": nullable(self.end_ms),
            "techniqueId": self.technique_id,
            "techniqueName": self.technique_name,
            "createdAt": self.created_at.to_iso8601_string(),
            "updatedAt": self.updated_at.to_iso8601_string(),
        })
    }
}
