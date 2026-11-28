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

/// Which track a library card's waveform is of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewSource {
    /// The practice's first vocal take (the active interpretation's first).
    Take,
    /// Its backing, when it has no take with audio.
    Backing,
}

/// One clip a preview is drawn from: where it sits on the timeline and its
/// cached waveform peaks (0 to 1, evenly over its length).
#[derive(Clone, Debug, PartialEq)]
pub struct PreviewClip {
    pub start_ms: i64,
    pub duration_ms: i64,
    pub peaks: Vec<f64>,
}

/// What a library card draws of a practice besides its summary: its length
/// and a small waveform, read from the waveform cache without decoding any
/// audio (`SongPracticeRepository::list_practice_previews`).
#[derive(Clone, Debug, PartialEq)]
pub struct PracticePreview {
    pub id: String,
    /// The practice's length: its timeline's, or its clips' end.
    pub length_ms: i64,
    /// The track the waveform is of; none when no take or backing has
    /// audio with cached peaks.
    pub source: Option<PreviewSource>,
    /// That track's id and its tint, if it has one: the card draws the
    /// waveform in the track's colour on the practice page.
    pub track_id: String,
    pub color_value: Option<i64>,
    /// The waveform: [`PracticePreview::BUCKETS`] peak levels (0 to 1)
    /// evenly across `length_ms`; empty with no source.
    pub peaks: Vec<f32>,
}

impl PracticePreview {
    /// How many levels a preview's waveform has.
    pub const BUCKETS: usize = 240;

    /// The preview of practice `id`, `length_ms` long, its waveform drawn
    /// from `clips` (of `source`'s track): each bucket the highest peak of
    /// the clips over it, silence where no clip is.
    pub fn compose(
        id: impl Into<String>,
        length_ms: i64,
        source: Option<PreviewSource>,
        clips: &[PreviewClip],
    ) -> PracticePreview {
        let length = clips
            .iter()
            .map(|c| c.start_ms + c.duration_ms.max(0))
            .fold(length_ms.max(0), i64::max);
        let drawn = clips
            .iter()
            .any(|c| !c.peaks.is_empty() && c.duration_ms > 0);
        let source = source.filter(|_| drawn && length > 0);
        let mut peaks = Vec::new();
        if source.is_some() {
            peaks = vec![0.0f32; Self::BUCKETS];
            let per_bucket = length as f64 / Self::BUCKETS as f64;
            for clip in clips
                .iter()
                .filter(|c| c.duration_ms > 0 && !c.peaks.is_empty())
            {
                let n = clip.peaks.len();
                let (start, duration) = (clip.start_ms as f64, clip.duration_ms as f64);
                let end = start + duration;
                // Every bucket the clip covers takes the loudest of the
                // peaks over its stretch of time (one at least, so a clip
                // with fewer peaks than buckets leaves no gaps).
                let first = (start / per_bucket).floor().max(0.0) as usize;
                let last = ((end / per_bucket).ceil() as usize).min(Self::BUCKETS);
                for (bucket, slot) in peaks.iter_mut().enumerate().take(last).skip(first) {
                    let t0 = (bucket as f64 * per_bucket).max(start);
                    let t1 = ((bucket + 1) as f64 * per_bucket).min(end);
                    if t1 <= t0 {
                        continue;
                    }
                    let i0 = (((t0 - start) / duration * n as f64).floor() as usize).min(n - 1);
                    let i1 =
                        (((t1 - start) / duration * n as f64).ceil() as usize).clamp(i0 + 1, n);
                    let loudest = clip.peaks[i0..i1]
                        .iter()
                        .fold(0.0f64, |m, p| m.max(p.abs().min(1.0)));
                    *slot = slot.max(loudest as f32);
                }
            }
        }
        PracticePreview {
            id: id.into(),
            length_ms: length,
            source,
            track_id: String::new(),
            color_value: None,
            peaks,
        }
    }

    /// The preview, its waveform of track `id`, tinted `color_value`.
    pub fn of_track(self, id: impl Into<String>, color_value: Option<i64>) -> PracticePreview {
        PracticePreview {
            track_id: id.into(),
            color_value,
            ..self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_preview_spreads_its_clips_peaks_over_the_practice_s_length() {
        // A 10 s practice, one clip over its second half.
        let clip = PreviewClip {
            start_ms: 5_000,
            duration_ms: 5_000,
            peaks: vec![0.5; 100],
        };
        let p = PracticePreview::compose("P", 10_000, Some(PreviewSource::Take), &[clip]);
        assert_eq!(p.length_ms, 10_000);
        assert_eq!(p.peaks.len(), PracticePreview::BUCKETS);
        let half = PracticePreview::BUCKETS / 2;
        assert!(p.peaks[..half - 1].iter().all(|v| *v == 0.0));
        assert!(p.peaks[half + 1..].iter().all(|v| (*v - 0.5).abs() < 1e-6));
    }

    #[test]
    fn a_clip_past_the_timeline_lengthens_it_and_no_peaks_draw_nothing() {
        let clip = PreviewClip {
            start_ms: 0,
            duration_ms: 4_000,
            peaks: vec![1.0; 10],
        };
        let p = PracticePreview::compose("P", 0, Some(PreviewSource::Backing), &[clip]);
        assert_eq!(p.length_ms, 4_000);
        assert_eq!(p.source, Some(PreviewSource::Backing));
        let empty = PreviewClip {
            start_ms: 0,
            duration_ms: 4_000,
            peaks: Vec::new(),
        };
        let p = PracticePreview::compose("P", 0, Some(PreviewSource::Take), &[empty]);
        assert_eq!((p.source, p.peaks.len(), p.length_ms), (None, 0, 4_000));
        let p = PracticePreview::compose("P", 0, None, &[]);
        assert_eq!((p.source, p.length_ms), (None, 0));
    }
}
