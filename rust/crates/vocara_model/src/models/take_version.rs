//! A named interpretation of a song (port of
//! `lib/domain/models/take_version.dart`).

use serde_json::{json, Value};

use super::json::{as_map, req_str};
use crate::util::dart_date_time::DateTime;
use crate::Result;

/// A named interpretation of a song — one emotional/technical reading the user
/// is working on (e.g. "Tender", "Powerful"). It groups a set of vocal takes:
/// switching the active interpretation swaps which takes are shown and played,
/// against the same shared accompaniment and lyrics. A practice always has at
/// least one. (Dart's `copyWith(name:)` is `TakeVersion { name, ..v }`.)
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TakeVersion {
    pub id: String,
    /// User-facing label shown in the interpretation dropdown.
    pub name: String,
    pub created_at: DateTime,
}

impl TakeVersion {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        created_at: DateTime,
    ) -> TakeVersion {
        TakeVersion {
            id: id.into(),
            name: name.into(),
            created_at,
        }
    }

    pub fn from_json(json: &Value) -> Result<TakeVersion> {
        let j = as_map(json, "TakeVersion")?;
        Ok(TakeVersion {
            id: req_str(j, "id")?,
            name: req_str(j, "name")?,
            created_at: DateTime::parse(&req_str(j, "createdAt")?)?,
        })
    }

    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "createdAt": self.created_at.to_iso8601_string(),
        })
    }
}
