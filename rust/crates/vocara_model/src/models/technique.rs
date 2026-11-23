//! A technique library entry (port of `lib/domain/models/technique.dart`).

use serde_json::{json, Value};

use super::json::{as_map, list_of, opt_str, req_str};
use super::technique_media::TechniqueMedia;
use crate::util::dart_date_time::DateTime;
use crate::util::dart_string;
use crate::{Error, Result};

/// A single vocal technique entry in the user's technique library.
///
/// Framework agnostic by design: it carries whatever name, aliases, and tags
/// the singer's own system (EVT, CVT, SLS, bel canto, or a personal vocabulary)
/// uses, plus free-text how-to notes. Nothing here assumes one school of
/// thought — the user organizes it their way.
///
/// [acoustic_signature] is Vocara's angle: the singer can describe how the
/// technique reads in the app's own measurements (RING, spectral tilt, CPP,
/// formants) so they can later verify it against their own takes. Free text in
/// this phase; structured, analysis-linked targets come later. (Dart's
/// `copyWith` is struct update syntax; it never changes `id` or `createdAt`.)
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Technique {
    pub id: String,
    /// The technique's primary name (e.g. "Twang", "Curbing", "Mixed voice").
    pub name: String,
    /// Other names the same technique goes by in different systems (e.g. a Twang
    /// quality is close to CVT's "Edge"). Shown as chips and searchable.
    pub aliases: Vec<String>,
    /// User-defined labels for grouping/filtering (e.g. "Register", "Quality",
    /// "Ornament", "Figure"). Not a fixed taxonomy — each singer organizes their
    /// own way.
    pub tags: Vec<String>,
    /// One-line gloss shown in the technique list.
    pub summary: String,
    /// The full explanation of what the technique is.
    pub description: String,
    /// How to produce it — cues, sensations, step by step.
    pub cues: String,
    /// Practice drills / exercises for the technique.
    pub exercises: String,
    /// How the technique reads in Vocara's acoustic measurements.
    pub acoustic_signature: String,
    /// Audio/video example clips for the technique, in display order. Each file
    /// lives in the app sandbox (copied on import).
    pub media: Vec<TechniqueMedia>,
    pub created_at: DateTime,
    pub updated_at: DateTime,
}

impl Technique {
    /// `Technique(id:, name:, createdAt:, updatedAt:)` with everything else
    /// empty.
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        created_at: DateTime,
        updated_at: DateTime,
    ) -> Technique {
        Technique {
            id: id.into(),
            name: name.into(),
            aliases: Vec::new(),
            tags: Vec::new(),
            summary: String::new(),
            description: String::new(),
            cues: String::new(),
            exercises: String::new(),
            acoustic_signature: String::new(),
            media: Vec::new(),
            created_at,
            updated_at,
        }
    }

    /// Whether the entry has any body content beyond its name (used to show a
    /// gentle "nothing written yet" hint in the viewer).
    pub fn has_body(&self) -> bool {
        let filled = |s: &str| !dart_string::trim(s).is_empty();
        filled(&self.summary)
            || filled(&self.description)
            || filled(&self.cues)
            || filled(&self.exercises)
            || filled(&self.acoustic_signature)
            || !self.media.is_empty()
    }

    pub fn from_json(json: &Value) -> Result<Technique> {
        let j = as_map(json, "Technique")?;
        let strings = |key: &str| {
            list_of(j, key, |e| match e {
                Value::String(s) => Ok(s.clone()),
                _ => Err(Error::Type(format!("'{key}': an entry is not a String"))),
            })
        };
        Ok(Technique {
            id: req_str(j, "id")?,
            name: req_str(j, "name")?,
            aliases: strings("aliases")?,
            tags: strings("tags")?,
            summary: opt_str(j, "summary")?.unwrap_or_default(),
            description: opt_str(j, "description")?.unwrap_or_default(),
            cues: opt_str(j, "cues")?.unwrap_or_default(),
            exercises: opt_str(j, "exercises")?.unwrap_or_default(),
            acoustic_signature: opt_str(j, "acousticSignature")?.unwrap_or_default(),
            media: list_of(j, "media", TechniqueMedia::from_json)?,
            created_at: DateTime::parse(&req_str(j, "createdAt")?)?,
            updated_at: DateTime::parse(&req_str(j, "updatedAt")?)?,
        })
    }

    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "aliases": self.aliases,
            "tags": self.tags,
            "summary": self.summary,
            "description": self.description,
            "cues": self.cues,
            "exercises": self.exercises,
            "acousticSignature": self.acoustic_signature,
            "media": self.media.iter().map(TechniqueMedia::to_json).collect::<Vec<_>>(),
            "createdAt": self.created_at.to_iso8601_string(),
            "updatedAt": self.updated_at.to_iso8601_string(),
        })
    }
}
