//! Lanes and clips (port of `lib/domain/models/audio_track.dart`).

use serde_json::{json, Value};

use super::audio_fx::{TrackFx, TrackSend};
use super::json::{
    as_map, double, doubles, field, list_of, nullable, opt_bool, opt_int, opt_list, opt_num,
    opt_str, req_str,
};
use super::practice_note::PracticeNote;
use super::technique_marker::TechniqueMarker;
use crate::util::dart_date_time::DateTime;
use crate::util::dart_num::clamp_f64;
use crate::{Error, Result};

/// Distinguishes the backing track (imported by the user) from the vocal
/// takes the user records or imports to compare against it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AudioTrackKind {
    /// The song's backing track / instrumental, shown at the top of the detail
    /// view. A practice has at most one.
    Accompaniment,
    /// The original recording of the song (with the original vocal), used as a
    /// reference to A/B against the accompaniment. A practice has at most one.
    Reference,
    /// A vocal take. A practice can hold many so the user can compare their
    /// own performances.
    Vocal,
}

impl AudioTrackKind {
    pub const VALUES: [AudioTrackKind; 3] = [
        AudioTrackKind::Accompaniment,
        AudioTrackKind::Reference,
        AudioTrackKind::Vocal,
    ];

    /// The Dart enum name, as stored in `kind`.
    pub fn name(self) -> &'static str {
        match self {
            AudioTrackKind::Accompaniment => "accompaniment",
            AudioTrackKind::Reference => "reference",
            AudioTrackKind::Vocal => "vocal",
        }
    }

    /// `AudioTrackKind.values.byName(name)`: an unknown name is an error, as
    /// Dart's `ArgumentError`.
    pub fn by_name(name: &str) -> Result<AudioTrackKind> {
        AudioTrackKind::VALUES
            .into_iter()
            .find(|k| k.name() == name)
            .ok_or_else(|| Error::Type(format!("no AudioTrackKind named \"{name}\"")))
    }
}

/// The takes' colours, ARGB as [`AudioTrack::color_value`] stores them, in
/// the order new takes take them: `DESIGN.md`'s five track colours, coral,
/// ochre, sage, periwinkle and lilac (the app's `Studio::TRACK_COLORS`).
pub const TAKE_COLORS: [i64; 5] = [0xFFD98C7E, 0xFFC9A46E, 0xFF6FB39A, 0xFF8FA3D9, 0xFFB48FD0];

/// Which of [`TAKE_COLORS`] the take `id` shows while it has no colour of
/// its own (a take from before new takes were given one): the palette's at
/// a hash of its id (FNV-1a, the same across runs and builds), so it stays
/// the take's however the takes around it are added, removed or reordered.
pub fn default_take_color_index(id: &str) -> usize {
    let hash = id.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    (hash % TAKE_COLORS.len() as u64) as usize
}

/// A single audio clip on a lane: an audio file placed at [start_ms] on the
/// shared timeline, with its own pre-computed waveform. A lane ([`AudioTrack`])
/// holds zero or more clips (Logic / Cubase style), so recording onto a lane
/// drops a new, independent clip rather than merging into the existing audio.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioClip {
    pub id: String,
    /// Absolute path to the audio file. Files live in the app support directory
    /// so they travel with the practice.
    pub audio_file_path: String,
    /// Duration of the clip's audio in milliseconds.
    pub duration_ms: i64,
    /// Start offset on the shared timeline (ms).
    pub start_ms: i64,
    /// Down-sampled peak amplitudes in [0, 1], one per render bucket. Empty until
    /// analysis completes.
    pub waveform_peaks: Vec<f64>,
}

/// The order a lane's clips are stacked in, bottom first: the lane's own order, each clip
/// laid over the ones before it. A recording or a drop is appended to its lane, so the newest
/// clip is on top, as the lane draws them. Where clips overlap only the topmost is heard and
/// analyzed, as Logic, Cubase and Pro Tools play overlapping regions: playback
/// ([`lane_visible_spans`]) and the lane's merged analyses (`merge_voice_metrics`,
/// `merge_spectrograms`) all lay clips down in this order, so what the singer hears is what the
/// pitch and spectrogram lanes show. Playback used to sum every clip whole, so re-singing a
/// phrase over a take played both performances at once.
pub fn lane_stack_order(clips: usize) -> std::ops::Range<usize> {
    0..clips
}

/// One stretch of the timeline where one of a lane's clips is heard (see
/// [`lane_visible_spans`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VisibleSpan {
    /// The clip: an index into the spans [`lane_visible_spans`] was given.
    pub clip: usize,
    pub start_ms: i64,
    pub end_ms: i64,
}

/// What of each clip a lane plays: `spans` holds each clip's `(start_ms, end_ms)` in the lane's
/// order, and each clip comes back as the stretches of it no clip stacked above it covers
/// ([`lane_stack_order`]): clips in the lane's order, each one's stretches in timeline order.
/// A clip wholly covered has none; an empty span plays nothing.
pub fn lane_visible_spans(spans: &[(i64, i64)]) -> Vec<VisibleSpan> {
    let order: Vec<usize> = lane_stack_order(spans.len()).collect();
    let mut out = Vec::new();
    for (pos, &clip) in order.iter().enumerate() {
        let (start, end) = spans[clip];
        if end <= start {
            continue;
        }
        let mut above: Vec<(i64, i64)> = order[pos + 1..]
            .iter()
            .map(|&j| spans[j])
            .filter(|&(s, e)| e > s && e > start && s < end)
            .collect();
        above.sort_unstable();
        let mut at = start;
        for (s, e) in above {
            if s > at {
                out.push(VisibleSpan {
                    clip,
                    start_ms: at,
                    end_ms: s.min(end),
                });
            }
            at = at.max(e);
            if at >= end {
                break;
            }
        }
        if at < end {
            out.push(VisibleSpan {
                clip,
                start_ms: at,
                end_ms: end,
            });
        }
    }
    out
}

impl AudioClip {
    /// `AudioClip(id:, audioFilePath:, durationMs:)` at start 0, no peaks.
    pub fn new(
        id: impl Into<String>,
        audio_file_path: impl Into<String>,
        duration_ms: i64,
    ) -> AudioClip {
        AudioClip {
            id: id.into(),
            audio_file_path: audio_file_path.into(),
            duration_ms,
            start_ms: 0,
            waveform_peaks: Vec::new(),
        }
    }

    pub fn end_ms(&self) -> i64 {
        self.start_ms + self.duration_ms
    }

    pub fn has_waveform(&self) -> bool {
        !self.waveform_peaks.is_empty()
    }

    pub fn from_json(json: &Value) -> Result<AudioClip> {
        let j = as_map(json, "AudioClip")?;
        Ok(AudioClip {
            id: req_str(j, "id")?,
            audio_file_path: opt_str(j, "audioFilePath")?.unwrap_or_default(),
            duration_ms: opt_int(j, "durationMs")?.unwrap_or(0),
            start_ms: opt_int(j, "startMs")?.unwrap_or(0),
            waveform_peaks: doubles(j, "waveformPeaks")?,
        })
    }

    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "audioFilePath": self.audio_file_path,
            "durationMs": self.duration_ms,
            "startMs": self.start_ms,
            "waveformPeaks": self.waveform_peaks.iter().map(|p| double(*p)).collect::<Vec<_>>(),
        })
    }
}

/// A single lane in the multi-track editor: a list of audio [clips] plus its own
/// lane of notes, mute/solo, volume, color and effect chain. (Dart's
/// `copyWith` is struct update syntax; its `clearColor` / `clearFx` /
/// `clearBus` flags are `color_value: None` / `fx: None` / `bus_id: None`.)
#[derive(Clone, Debug, PartialEq)]
pub struct AudioTrack {
    pub id: String,
    pub kind: AudioTrackKind,
    /// Human-readable label shown on the lane header.
    pub display_name: String,
    pub created_at: DateTime,
    /// The clips on this lane, in creation order — NOT necessarily timeline
    /// order (a punch-in can land before an existing clip). Empty = an empty
    /// take. The lane-span getters below are order-independent.
    pub clips: Vec<AudioClip>,
    /// Notes pinned to this lane's timeline — the lane's "note track".
    pub notes: Vec<PracticeNote>,
    /// Technique annotations pinned to this lane's timeline — the lane's
    /// "technique track" (each references a library technique at a moment).
    pub technique_markers: Vec<TechniqueMarker>,
    /// Optional ARGB color used to tint the lane (helps distinguish takes).
    pub color_value: Option<i64>,
    pub muted: bool,
    /// When any track is soloed, only soloed tracks are audible.
    pub soloed: bool,
    /// Linear playback gain in the range [0.0, 1.0].
    pub volume: f64,
    /// Stereo pan in [-1, 1] (0 = center). Backing tracks keep 0.
    pub pan: f64,
    /// For vocal takes: the interpretation (TakeVersion) this take belongs to.
    /// `None` for backing tracks and for legacy takes before migration.
    pub version_id: Option<String>,
    /// Effect chain (EQ / compressor / reverb send) for vocal takes. `None` = no
    /// effects configured (dry); backing tracks leave this `None`.
    pub fx: Option<TrackFx>,
    /// Output routing (vocal takes): the FX bus id this take feeds, or `None` =
    /// straight to the master. Backing tracks leave this `None`.
    pub bus_id: Option<String>,
    /// Post-fader aux sends to FX buses (independent of the main output).
    pub sends: Vec<TrackSend>,
}

impl AudioTrack {
    /// The factory's defaults with no clip: no notes or markers, no color,
    /// unmuted, unsoloed, volume 1, centered, no interpretation, dry, to the
    /// master, no sends. Pass the clips with struct update syntax, or use
    /// [`AudioTrack::with_audio`] for the single-clip convenience.
    pub fn new(
        id: impl Into<String>,
        kind: AudioTrackKind,
        display_name: impl Into<String>,
        created_at: DateTime,
    ) -> AudioTrack {
        AudioTrack {
            id: id.into(),
            kind,
            display_name: display_name.into(),
            created_at,
            clips: Vec::new(),
            notes: Vec::new(),
            technique_markers: Vec::new(),
            color_value: None,
            muted: false,
            soloed: false,
            volume: 1.0,
            pan: 0.0,
            version_id: None,
            fx: None,
            bus_id: None,
            sends: Vec::new(),
        }
    }

    /// The factory's single-clip convenience fields (audioFilePath / startMs
    /// / durationMs / waveformPeaks), which become the lane's one clip
    /// `'<id>-c0'` — the common way imports and recordings create takes. An
    /// empty [audio_file_path] makes an empty lane.
    pub fn with_audio(
        mut self,
        audio_file_path: impl Into<String>,
        start_ms: i64,
        duration_ms: i64,
        waveform_peaks: Vec<f64>,
    ) -> AudioTrack {
        let audio_file_path = audio_file_path.into();
        self.clips = if audio_file_path.is_empty() {
            Vec::new()
        } else {
            vec![AudioClip {
                id: format!("{}-c0", self.id),
                audio_file_path,
                duration_ms,
                start_ms,
                waveform_peaks,
            }]
        };
        self
    }

    pub fn is_accompaniment(&self) -> bool {
        self.kind == AudioTrackKind::Accompaniment
    }

    pub fn is_reference(&self) -> bool {
        self.kind == AudioTrackKind::Reference
    }

    /// Backing tracks (accompaniment or reference) are the song you sing against.
    pub fn is_backing(&self) -> bool {
        matches!(
            self.kind,
            AudioTrackKind::Accompaniment | AudioTrackKind::Reference
        )
    }

    /// The lane's first clip — for file-centric operations that require a
    /// single-clip lane (byte-level split/merge) or one representative clip.
    /// `None` when the lane is empty. Time/analysis consumers should use the
    /// lane-span fields ([`AudioTrack::start_ms`]/[`AudioTrack::end_ms`]/
    /// [`AudioTrack::duration_ms`]) or iterate [clips].
    pub fn primary_clip(&self) -> Option<&AudioClip> {
        self.clips.first()
    }

    pub fn clip_by_id(&self, clip_id: &str) -> Option<&AudioClip> {
        self.clips.iter().find(|c| c.id == clip_id)
    }

    // --- Lane-span time model: the lane's time fields cover ALL of its clips,
    // so multi-clip takes (punch-in recordings, dropped clips) read correctly
    // everywhere. For a single-clip lane they equal the clip's own fields. ---

    /// Start of the lane's content on the shared timeline: the earliest clip
    /// start (0 for an empty lane).
    pub fn start_ms(&self) -> i64 {
        match self.clips.first() {
            None => 0,
            Some(first) => {
                self.clips.iter().fold(
                    first.start_ms,
                    |m, c| if c.start_ms < m { c.start_ms } else { m },
                )
            }
        }
    }

    /// End of the lane's content on the shared timeline: the latest clip end
    /// (0 for an empty lane).
    pub fn end_ms(&self) -> i64 {
        self.clips
            .iter()
            .fold(0, |m, c| if c.end_ms() > m { c.end_ms() } else { m })
    }

    /// The lane's content span in ms ([start_ms]..[end_ms]), gaps included.
    pub fn duration_ms(&self) -> i64 {
        self.end_ms() - self.start_ms()
    }

    pub fn has_clip(&self) -> bool {
        !self.clips.is_empty()
    }

    pub fn is_empty_take(&self) -> bool {
        self.kind == AudioTrackKind::Vocal && self.clips.is_empty()
    }

    pub fn from_json(json: &Value) -> Result<AudioTrack> {
        let j = as_map(json, "AudioTrack")?;
        let clips = match opt_list(j, "clips")? {
            Some(items) => items
                .iter()
                .map(AudioClip::from_json)
                .collect::<Result<Vec<_>>>()?,
            None => {
                // Legacy take: a single inline clip (or empty).
                let path = opt_str(j, "audioFilePath")?.unwrap_or_default();
                if path.is_empty() {
                    Vec::new()
                } else {
                    vec![AudioClip {
                        // `'${json['id']}-c0'`: a non-string id fails the
                        // load below anyway.
                        id: format!(
                            "{}-c0",
                            field(j, "id").and_then(Value::as_str).unwrap_or("null")
                        ),
                        audio_file_path: path,
                        duration_ms: opt_int(j, "durationMs")?.unwrap_or(0),
                        start_ms: opt_int(j, "startMs")?.unwrap_or(0),
                        waveform_peaks: doubles(j, "waveformPeaks")?,
                    }]
                }
            }
        };
        Ok(AudioTrack {
            id: req_str(j, "id")?,
            kind: AudioTrackKind::by_name(&req_str(j, "kind")?)?,
            display_name: req_str(j, "displayName")?,
            created_at: DateTime::parse(&req_str(j, "createdAt")?)?,
            clips,
            notes: list_of(j, "notes", PracticeNote::from_json)?,
            technique_markers: list_of(j, "techniqueMarkers", TechniqueMarker::from_json)?,
            color_value: opt_int(j, "colorValue")?,
            muted: opt_bool(j, "muted")?.unwrap_or(false),
            soloed: opt_bool(j, "soloed")?.unwrap_or(false),
            // Clamp on load (like ReverbSettings): a synced / migrated / hand-edited
            // value outside the fader's range must not reach the engine verbatim.
            volume: clamp_f64(opt_num(j, "volume")?.unwrap_or(1.0), 0.0, 1.0),
            pan: clamp_f64(opt_num(j, "pan")?.unwrap_or(0.0), -1.0, 1.0),
            version_id: opt_str(j, "versionId")?,
            fx: match field(j, "fx") {
                Some(fx) => Some(TrackFx::from_json(fx)?),
                None => None,
            },
            bus_id: opt_str(j, "busId")?,
            sends: list_of(j, "sends", TrackSend::from_json)?,
        })
    }

    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "kind": self.kind.name(),
            "displayName": self.display_name,
            "clips": self.clips.iter().map(AudioClip::to_json).collect::<Vec<_>>(),
            "notes": self.notes.iter().map(PracticeNote::to_json).collect::<Vec<_>>(),
            "techniqueMarkers": self.technique_markers.iter().map(TechniqueMarker::to_json).collect::<Vec<_>>(),
            "colorValue": nullable(self.color_value),
            "muted": self.muted,
            "soloed": self.soloed,
            "volume": double(self.volume),
            "pan": double(self.pan),
            "createdAt": self.created_at.to_iso8601_string(),
            "versionId": nullable(self.version_id.clone()),
            "fx": self.fx.as_ref().map_or(Value::Null, TrackFx::to_json),
            "busId": nullable(self.bus_id.clone()),
            "sends": self.sends.iter().map(TrackSend::to_json).collect::<Vec<_>>(),
        })
    }
}

#[cfg(test)]
mod lane_tests {
    use super::*;

    fn span(clip: usize, start_ms: i64, end_ms: i64) -> VisibleSpan {
        VisibleSpan {
            clip,
            start_ms,
            end_ms,
        }
    }

    #[test]
    fn a_phrase_sung_again_over_a_take_hides_that_stretch_of_it() {
        // The take 0-60 s, the phrase re-sung 20-30 s on top.
        let got = lane_visible_spans(&[(0, 60_000), (20_000, 30_000)]);
        assert_eq!(
            got,
            [
                span(0, 0, 20_000),
                span(0, 30_000, 60_000),
                span(1, 20_000, 30_000)
            ]
        );
    }

    #[test]
    fn the_newest_clip_is_on_top_wherever_it_starts() {
        // A take from 10 s, then one recorded over it from 0 s.
        let got = lane_visible_spans(&[(10_000, 60_000), (0, 30_000)]);
        assert_eq!(got, [span(0, 30_000, 60_000), span(1, 0, 30_000)]);
    }

    #[test]
    fn a_clip_covered_whole_plays_nothing_and_gaps_stay_gaps() {
        let got = lane_visible_spans(&[(1_000, 2_000), (0, 5_000), (8_000, 9_000), (7, 7)]);
        assert_eq!(got, [span(1, 0, 5_000), span(2, 8_000, 9_000)]);
    }

    #[test]
    fn several_clips_above_leave_the_stretches_between_them() {
        let got =
            lane_visible_spans(&[(0, 10_000), (2_000, 3_000), (2_500, 4_000), (6_000, 7_000)]);
        assert_eq!(
            got,
            [
                span(0, 0, 2_000),
                span(0, 4_000, 6_000),
                span(0, 7_000, 10_000),
                span(1, 2_000, 2_500),
                span(2, 2_500, 4_000),
                span(3, 6_000, 7_000)
            ]
        );
    }
}
