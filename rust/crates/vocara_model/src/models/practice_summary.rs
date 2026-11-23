//! What the Home list shows of a practice (port of
//! `lib/domain/models/practice_summary.dart`).

use super::song_practice::SongPractice;
use crate::util::dart_date_time::DateTime;

/// What the Home list shows of a practice, read without hydrating it: the
/// list needs a title, a date and three counts, and a full load per card —
/// every clip's waveform peaks and a file check per clip — was paid again on
/// every return to Home. (No value equality, as in Dart.)
#[derive(Clone, Debug)]
pub struct PracticeSummary {
    pub id: String,
    pub title: String,
    pub updated_at: DateTime,
    /// Every vocal take, in every interpretation.
    pub take_count: usize,
    pub has_accompaniment: bool,
    /// Notes on every lane.
    pub note_count: usize,
}

impl PracticeSummary {
    /// The summary of an aggregate already in hand (e.g. one just created),
    /// by the same rules the summary read applies to the stored rows.
    pub fn of(p: &SongPractice) -> PracticeSummary {
        PracticeSummary {
            id: p.id.clone(),
            title: p.title.clone(),
            updated_at: p.updated_at,
            take_count: p.vocal_tracks.len(),
            has_accompaniment: p.has_accompaniment(),
            note_count: p.note_count(),
        }
    }
}
