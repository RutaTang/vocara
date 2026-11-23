//! A note pinned to a lane's timeline (port of
//! `lib/domain/models/practice_note.dart`).

use serde_json::{json, Value};

use super::json::{as_map, nullable, opt_int, req_int, req_str};
use crate::util::dart_date_time::DateTime;
use crate::Result;

/// A single note pinned to a track on the timeline — either at one point, or
/// across a range when [end_ms] is set.
///
/// Notes capture observations about the user's singing practice (e.g. "breath
/// runs out here", "pitch flat on the high note"). A point note marks a moment;
/// a range note ([end_ms] > [position_ms]) marks a span the observation applies to
/// (e.g. "keep twang through this phrase"). (Dart's `copyWith(clearEnd: true)`
/// is `PracticeNote { end_ms: None, ..note }`.)
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PracticeNote {
    /// Stable unique identifier.
    pub id: String,
    /// Start (or, for a point note, the only) position on the shared timeline, in
    /// milliseconds.
    pub position_ms: i64,
    /// End of the range, in milliseconds, or `None` for a point note. When set it is
    /// always greater than [position_ms] (see [`PracticeNote::is_range`]).
    pub end_ms: Option<i64>,
    /// Free-text body of the note.
    pub content: String,
    pub created_at: DateTime,
    pub updated_at: DateTime,
}

impl PracticeNote {
    /// True when this note spans a range rather than a single point.
    pub fn is_range(&self) -> bool {
        self.end_ms.is_some_and(|end| end > self.position_ms)
    }

    pub fn from_json(json: &Value) -> Result<PracticeNote> {
        let j = as_map(json, "PracticeNote")?;
        Ok(PracticeNote {
            id: req_str(j, "id")?,
            position_ms: req_int(j, "positionMs")?,
            end_ms: opt_int(j, "endMs")?,
            content: req_str(j, "content")?,
            created_at: DateTime::parse(&req_str(j, "createdAt")?)?,
            updated_at: DateTime::parse(&req_str(j, "updatedAt")?)?,
        })
    }

    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "positionMs": self.position_ms,
            "endMs": nullable(self.end_ms),
            "content": self.content,
            "createdAt": self.created_at.to_iso8601_string(),
            "updatedAt": self.updated_at.to_iso8601_string(),
        })
    }
}
