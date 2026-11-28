//! A singing-practice project (port of `lib/domain/models/song_practice.dart`).
//!
//! The lanes, interpretations and FX buses are held behind [`Arc`]s: a Dart
//! object is a shared reference, and [`SongPractice::sharing`] keeps the
//! IDENTITY of every part a re-read reproduces, so whatever compares by
//! identity ([`Arc::ptr_eq`]: kept lanes, channel strips, envelope caches)
//! sees only what actually changed. Cloning a practice is cheap for the same
//! reason (a lane's waveform peaks are not copied).

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{json, Value};

use super::audio_fx::ReverbSettings;
use super::audio_track::{default_take_color_index, AudioTrack, TAKE_COLORS};
use super::fx_bus::FxBus;
use super::json::{as_map, double, field, list_of, nullable, opt_int, opt_num, opt_str, req_str};
use super::lyric_line::LyricLine;
use super::take_version::TakeVersion;
use crate::util::dart_date_time::DateTime;
use crate::util::dart_num::clamp_f64;
use crate::Result;

/// A single singing-practice project — the unit shown as a card on the home
/// screen and opened in the multi-track detail view.
///
/// It holds one optional [accompaniment] track (the imported backing track,
/// rendered at the top of the detail view) and any number of [vocal_tracks]
/// the user records or imports to compare their takes. (Dart's `copyWith` is
/// struct update syntax; `clearAccompaniment` / `clearReference` /
/// `clearSeparationSource` are `None`. Dart's `copyWith` never changes `id`
/// or `createdAt`.)
#[derive(Clone, Debug, PartialEq)]
pub struct SongPractice {
    pub id: String,
    /// User-facing name of the practice (e.g. the song title).
    pub title: String,
    /// The imported backing track. `None` until the user drags one in.
    pub accompaniment: Option<Arc<AudioTrack>>,
    /// The original recording of the song (with the original vocal), to A/B
    /// against the accompaniment. `None` until the user drags one in.
    pub reference: Option<Arc<AudioTrack>>,
    /// Vocal takes across every interpretation, in display order. Each vocal
    /// take's [`AudioTrack::version_id`] says which [versions] entry it belongs to;
    /// the view shows only the active interpretation's takes.
    pub vocal_tracks: Vec<Arc<AudioTrack>>,
    /// The interpretations (emotional/technical readings) of this song. Always at
    /// least one. Vocal takes are grouped under these.
    pub versions: Vec<Arc<TakeVersion>>,
    /// The interpretation currently shown/played. `None` only for a not-yet-migrated
    /// legacy practice (resolved on load).
    pub active_version_id: Option<String>,
    /// Lyrics for the song, in order; each line optionally synced to a timeline
    /// position. Empty until the user pastes/imports lyrics.
    pub lyrics: Vec<LyricLine>,
    /// The total length of the shared timeline, independent of the content (set
    /// by the user, like a project length in Logic). 0 means "not yet set" — the
    /// view model defaults it to fit the content. The view maps time→x with this
    /// so moving clips never rescales the timeline. Never less than [`SongPractice::content_end_ms`].
    pub timeline_duration_ms: i64,
    pub created_at: DateTime,
    pub updated_at: DateTime,
    /// Sandbox path to the kept original full-mix song that the accompaniment +
    /// reference were separated from. `None` when the backing wasn't produced by
    /// stem separation. Preserved across separations so it can be re-separated.
    pub separation_source: Option<String>,
    /// The shared reverb bus settings for this practice's vocal takes. `None` = the default
    /// (a medium room at 25% wet, which is silent until a take's reverb send is raised).
    pub reverb: Option<ReverbSettings>,
    /// The master bus output gain (linear, 0..1, 1.0 = unity; clamped to 1.0 on
    /// load to match the master fader's range) — the overall level the whole mix
    /// passes through before the master soft-clip.
    pub master_volume: f64,
    /// DAW-style FX buses (group/aux tracks) vocal takes can route into. Project-
    /// scoped and shared across interpretations. Empty until the user adds one.
    pub fx_buses: Vec<Arc<FxBus>>,
}

impl SongPractice {
    /// `SongPractice(id:, title:, createdAt:, updatedAt:)` with the defaults:
    /// no backings, takes, interpretations or lyrics, timeline unset, no
    /// separation source, default reverb, unity master, no buses.
    pub fn new(
        id: impl Into<String>,
        title: impl Into<String>,
        created_at: DateTime,
        updated_at: DateTime,
    ) -> SongPractice {
        SongPractice {
            id: id.into(),
            title: title.into(),
            accompaniment: None,
            reference: None,
            vocal_tracks: Vec::new(),
            versions: Vec::new(),
            active_version_id: None,
            lyrics: Vec::new(),
            timeline_duration_ms: 0,
            created_at,
            updated_at,
            separation_source: None,
            reverb: None,
            master_volume: 1.0,
            fx_buses: Vec::new(),
        }
    }

    pub fn bus_by_id(&self, id: Option<&str>) -> Option<&Arc<FxBus>> {
        let id = id?;
        self.fx_buses.iter().find(|b| b.id == id)
    }

    pub fn has_accompaniment(&self) -> bool {
        self.accompaniment.is_some()
    }

    pub fn has_reference(&self) -> bool {
        self.reference.is_some()
    }

    /// The active interpretation (falls back to the first if the id is stale).
    pub fn active_version(&self) -> Option<&Arc<TakeVersion>> {
        self.versions
            .iter()
            .find(|v| Some(v.id.as_str()) == self.active_version_id.as_deref())
            .or_else(|| self.versions.first())
    }

    /// Vocal takes belonging to the active interpretation (see
    /// [`SongPractice::lists_take`]). When no interpretation is resolved
    /// (legacy), all takes are returned.
    pub fn active_vocal_tracks(&self) -> Vec<Arc<AudioTrack>> {
        if self.active_version().is_none() {
            return self.vocal_tracks.clone();
        }
        self.vocal_tracks
            .iter()
            .filter(|t| self.lists_take(t))
            .cloned()
            .collect()
    }

    /// Whether any clip of any track still plays [path]. The asset pool is
    /// content-addressed, so two lanes over the same bytes (a duplicated take,
    /// one file imported twice) share one path: removing one of them must not
    /// discard what the other still shows (its analysis, its file).
    pub fn references_audio(&self, path: &str) -> bool {
        self.accompaniment
            .iter()
            .chain(self.reference.iter())
            .chain(self.vocal_tracks.iter())
            .any(|t| t.clips.iter().any(|c| c.audio_file_path == path))
    }

    /// Whether vocal take [t] is listed under the active interpretation: its
    /// own, or — when its interpretation resolves to none of [versions] — the
    /// active one, so it is never invisible. Sync can leave such a take: the
    /// integrity repair nulls a pointer at an interpretation another device
    /// deleted, and one still in the sync quarantine is not here yet. Only the
    /// listing falls back; the stored pointer is left exactly as it is, since
    /// rewriting it at load would be an edit nobody made (one that outranked
    /// other devices' changes and the parked interpretation's own).
    pub fn lists_take(&self, t: &AudioTrack) -> bool {
        let Some(active) = self.active_version() else {
            return true;
        };
        if t.version_id.as_deref() == Some(active.id.as_str()) {
            return true;
        }
        !self
            .versions
            .iter()
            .any(|v| Some(v.id.as_str()) == t.version_id.as_deref())
    }

    /// The colour a new take is given (ARGB, for its `color_value`), as a DAW
    /// gives each new track the next colour: the palette's ([`TAKE_COLORS`])
    /// after the colour of the most recently created take, so a take never
    /// shares the colour of the one made before it. The palette's first when
    /// there is no take yet, or that take wears a colour off the palette.
    /// (A colour hashed from each take's id gave neighbouring takes the same
    /// one, one take in five.) THE one implementation: the detail view
    /// model's new, recorded, imported, duplicated and split takes and boot
    /// recovery's all take it.
    pub fn next_take_color(&self) -> i64 {
        let latest = self
            .vocal_tracks
            .iter()
            .max_by(|a, b| a.created_at.compare_to(&b.created_at));
        let shown = latest.map(|t| {
            t.color_value
                .unwrap_or(TAKE_COLORS[default_take_color_index(&t.id)])
        });
        let next = shown
            .and_then(|c| {
                TAKE_COLORS
                    .iter()
                    .position(|p| p & 0xFFFF_FFFF == c & 0xFFFF_FFFF)
            })
            .map_or(0, |i| (i + 1) % TAKE_COLORS.len());
        TAKE_COLORS[next]
    }

    /// One past the highest existing "Take N" / "Take N (recovered)" label among
    /// the active interpretation's takes, so a new take never reuses a number —
    /// not one freed by a delete, and not one held by a recovered take. THE one
    /// numbering implementation: normal take naming (the detail view model) and
    /// boot recovery (Persistence) both hand out numbers over the same set, so
    /// each side must count the other's names or the two paths produce
    /// duplicate take numbers.
    pub fn next_take_number(&self) -> i64 {
        let mut max = 0;
        for t in self.active_vocal_tracks() {
            if let Some(digits) = take_number_digits(&t.display_name) {
                // Names are user-editable and sync-delivered, so the digits can be
                // anything: 20 of them overflowed int.parse (every new take and every
                // boot recovery threw), and max-int wrapped max + 1 negative. A
                // number no real numbering reaches is skipped like a foreign label —
                // it can't collide, since handed-out numbers stay far below it.
                if let Ok(n) = digits.parse::<i64>() {
                    if n < 1 << 31 && n > max {
                        max = n;
                    }
                }
            }
        }
        max + 1
    }

    /// The end of the latest-finishing lane (its last clip's end) — the minimum
    /// length the timeline must span so no audio is clipped off the end.
    pub fn content_end_ms(&self) -> i64 {
        let mut max = self.accompaniment.as_ref().map_or(0, |a| a.end_ms());
        let reference_end = self.reference.as_ref().map_or(0, |r| r.end_ms());
        if reference_end > max {
            max = reference_end;
        }
        for track in &self.vocal_tracks {
            if track.end_ms() > max {
                max = track.end_ms();
            }
        }
        max
    }

    /// Total number of notes across every lane, in every interpretation — the
    /// home card semantics ("everything this practice holds"). The reference
    /// lane counts too: no local UI attaches notes to it today, but note
    /// updates route to it (see the detail view model's _updateTrack), so
    /// reference notes — e.g. arriving via sync — must never be silently
    /// uncounted.
    pub fn note_count(&self) -> usize {
        let mut count = self.accompaniment.as_ref().map_or(0, |a| a.notes.len());
        count += self.reference.as_ref().map_or(0, |r| r.notes.len());
        for track in &self.vocal_tracks {
            count += track.notes.len();
        }
        count
    }

    /// Notes across the accompaniment + reference + the active interpretation's
    /// takes. BOTH backings count here even though the workspace shows only one
    /// at a time: the Accompaniment ⇄ Original choice is transient view-state
    /// on the detail screen's mixer (MixerController.playReference), which this
    /// model deliberately cannot know — so this getter is "active
    /// interpretation", not "visible on screen". The count that matches what
    /// the user actually sees lives with that view-state
    /// (MixerController.visibleNoteCount); [`SongPractice::note_count`] keeps the every-lane
    /// total for the home card.
    pub fn active_note_count(&self) -> usize {
        let mut count = self.accompaniment.as_ref().map_or(0, |a| a.notes.len());
        count += self.reference.as_ref().map_or(0, |r| r.notes.len());
        for t in self.active_vocal_tracks() {
            count += t.notes.len();
        }
        count
    }

    /// This aggregate with each lane, FX bus and interpretation that equals
    /// [held]'s (by id) replaced by [held]'s very [`Arc`], and each list whose
    /// every element is then [held]'s, in order, by [held]'s list. The values
    /// are equal, so nothing read from it changes; what compares by identity —
    /// the kept lanes and channel strips, the envelope cache — sees only what
    /// actually differs. A re-read is all-new objects, and taking one in after
    /// a save rebuilt and repainted every lane though one fader had moved.
    /// Another practice's parts (a different id) are ignored: the result is
    /// this practice unchanged.
    pub fn sharing(&self, held: &SongPractice) -> SongPractice {
        if held.id != self.id {
            return self.clone();
        }
        fn one<T: PartialEq>(fresh: &Option<Arc<T>>, old: &Option<Arc<T>>) -> Option<Arc<T>> {
            match (fresh, old) {
                (Some(f), Some(o)) if **f == **o => Some(Arc::clone(o)),
                _ => fresh.clone(),
            }
        }
        fn many<T: PartialEq>(
            fresh: &[Arc<T>],
            old: &[Arc<T>],
            id_of: impl Fn(&T) -> &str,
        ) -> Vec<Arc<T>> {
            // Later entries win on a duplicate id, as in Dart's map literal.
            let by_id: HashMap<&str, &Arc<T>> = old.iter().map(|x| (id_of(x), x)).collect();
            let mut same = fresh.len() == old.len();
            let mut out = Vec::with_capacity(fresh.len());
            for (i, f) in fresh.iter().enumerate() {
                let x = match by_id.get(id_of(f)) {
                    Some(o) if ***o == **f => Arc::clone(o),
                    _ => Arc::clone(f),
                };
                same = same && Arc::ptr_eq(&x, &old[i]);
                out.push(x);
            }
            if same {
                old.to_vec()
            } else {
                out
            }
        }
        SongPractice {
            id: self.id.clone(),
            title: self.title.clone(),
            created_at: self.created_at,
            updated_at: self.updated_at,
            accompaniment: one(&self.accompaniment, &held.accompaniment),
            reference: one(&self.reference, &held.reference),
            vocal_tracks: many(&self.vocal_tracks, &held.vocal_tracks, |t| &t.id),
            versions: many(&self.versions, &held.versions, |v| &v.id),
            active_version_id: self.active_version_id.clone(),
            lyrics: if self.lyrics == held.lyrics {
                held.lyrics.clone()
            } else {
                self.lyrics.clone()
            },
            timeline_duration_ms: self.timeline_duration_ms,
            separation_source: self.separation_source.clone(),
            reverb: self.reverb,
            master_volume: self.master_volume,
            fx_buses: many(&self.fx_buses, &held.fx_buses, |b| &b.id),
        }
    }

    pub fn from_json(json: &Value) -> Result<SongPractice> {
        let j = as_map(json, "SongPractice")?;
        let id = req_str(j, "id")?;
        let created_at = DateTime::parse(&req_str(j, "createdAt")?)?;
        let vocal_tracks = list_of(j, "vocalTracks", AudioTrack::from_json)?;
        let mut versions = list_of(j, "versions", TakeVersion::from_json)?;
        let mut active_version_id = opt_str(j, "activeVersionId")?;

        // Migration: a legacy practice has no interpretations — wrap all existing
        // takes in a default "Interpretation 1" and make it active.
        if versions.is_empty() {
            let def = TakeVersion::new(format!("{id}__v1"), "Interpretation 1", created_at);
            active_version_id = Some(def.id.clone());
            versions = vec![def];
        } else if !active_version_id
            .as_deref()
            .is_some_and(|a| versions.iter().any(|v| v.id == a))
        {
            active_version_id = Some(versions[0].id.clone());
        }
        // Any take whose versionId doesn't resolve to an existing interpretation
        // (an untagged legacy take, or an id orphaned by partial/hand-edited JSON)
        // is reattached to the active interpretation so no take is ever hidden.
        let vocal_tracks = vocal_tracks
            .into_iter()
            .map(|t| {
                let valid = t
                    .version_id
                    .as_deref()
                    .is_some_and(|vid| versions.iter().any(|v| v.id == vid));
                if valid {
                    Arc::new(t)
                } else {
                    Arc::new(AudioTrack {
                        version_id: active_version_id.clone(),
                        ..t
                    })
                }
            })
            .collect();

        let title = req_str(j, "title")?;
        let backing = |key: &str| -> Result<Option<Arc<AudioTrack>>> {
            match field(j, key) {
                None => Ok(None),
                Some(v) => Ok(Some(Arc::new(AudioTrack::from_json(v)?))),
            }
        };
        Ok(SongPractice {
            id,
            title,
            accompaniment: backing("accompaniment")?,
            reference: backing("reference")?,
            vocal_tracks,
            versions: versions.into_iter().map(Arc::new).collect(),
            active_version_id,
            lyrics: list_of(j, "lyrics", LyricLine::from_json)?,
            timeline_duration_ms: opt_int(j, "timelineDurationMs")?.unwrap_or(0),
            created_at,
            updated_at: DateTime::parse(&req_str(j, "updatedAt")?)?,
            separation_source: opt_str(j, "separationSource")?,
            reverb: match field(j, "reverb") {
                Some(r) => Some(ReverbSettings::from_json(r)?),
                None => None,
            },
            // Clamp on load (like ReverbSettings): a synced / migrated / hand-edited
            // value outside the master fader's range must not reach the engine
            // verbatim (the master soft-clip runs after this gain).
            master_volume: clamp_f64(opt_num(j, "masterVolume")?.unwrap_or(1.0), 0.0, 1.0),
            fx_buses: list_of(j, "fxBuses", |e| FxBus::from_json(e).map(Arc::new))?,
        })
    }

    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "title": self.title,
            "accompaniment": self.accompaniment.as_ref().map_or(Value::Null, |a| a.to_json()),
            "reference": self.reference.as_ref().map_or(Value::Null, |r| r.to_json()),
            "vocalTracks": self.vocal_tracks.iter().map(|t| t.to_json()).collect::<Vec<_>>(),
            "versions": self.versions.iter().map(|v| v.to_json()).collect::<Vec<_>>(),
            "activeVersionId": nullable(self.active_version_id.clone()),
            "lyrics": self.lyrics.iter().map(LyricLine::to_json).collect::<Vec<_>>(),
            "timelineDurationMs": self.timeline_duration_ms,
            "createdAt": self.created_at.to_iso8601_string(),
            "updatedAt": self.updated_at.to_iso8601_string(),
            "separationSource": nullable(self.separation_source.clone()),
            "reverb": self.reverb.as_ref().map_or(Value::Null, ReverbSettings::to_json),
            "masterVolume": double(self.master_volume),
            "fxBuses": self.fx_buses.iter().map(|b| b.to_json()).collect::<Vec<_>>(),
        })
    }
}

/// The digits of a `^Take (\d+)(?: \(recovered\))?$` label (ASCII digits, as
/// Dart's `\d`), or `None` for any other label.
fn take_number_digits(name: &str) -> Option<&str> {
    let rest = name.strip_prefix("Take ")?;
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    match &rest[digits..] {
        "" | " (recovered)" => Some(&rest[..digits]),
        _ => None,
    }
}

/// Whether [a] and [b] hold the very same parts, in order (Dart's
/// `identical` on a list [`SongPractice::sharing`] kept).
pub fn same_parts<T>(a: &[Arc<T>], b: &[Arc<T>]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| Arc::ptr_eq(x, y))
}
