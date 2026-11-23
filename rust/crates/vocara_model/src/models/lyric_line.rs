//! One line of lyrics (port of `lib/domain/models/lyric_line.dart`).

use serde_json::{Map, Value};

use super::json::{as_map, opt_int, opt_str};
use crate::util::dart_string;
use crate::Result;

/// One line of lyrics, optionally pinned to a time on the shared timeline.
///
/// [time_ms] is `None` until the line is synced (via .lrc import or tap-to-time);
/// synced lines drive the karaoke highlight and auto-scroll. (Dart's
/// `copyWith(clearTime: true)` is `LyricLine { time_ms: None, ..line }`.)
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LyricLine {
    pub text: String,
    /// Start time on the shared timeline (ms). `None` = not yet synced.
    pub time_ms: Option<i64>,
}

impl LyricLine {
    pub fn new(text: impl Into<String>, time_ms: Option<i64>) -> LyricLine {
        LyricLine {
            text: text.into(),
            time_ms,
        }
    }

    pub fn is_synced(&self) -> bool {
        self.time_ms.is_some()
    }

    pub fn is_blank(&self) -> bool {
        dart_string::trim(&self.text).is_empty()
    }

    pub fn from_json(json: &Value) -> Result<LyricLine> {
        let j = as_map(json, "LyricLine")?;
        Ok(LyricLine {
            text: opt_str(j, "text")?.unwrap_or_default(),
            time_ms: opt_int(j, "timeMs")?,
        })
    }

    /// `{text, timeMs?}`: an unsynced line has no `timeMs` key at all.
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("text".into(), Value::from(self.text.clone()));
        if let Some(t) = self.time_ms {
            m.insert("timeMs".into(), Value::from(t));
        }
        Value::Object(m)
    }
}
