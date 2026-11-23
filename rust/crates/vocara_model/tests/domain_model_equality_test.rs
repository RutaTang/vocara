//! Port of `client/test/domain_model_equality_test.dart`: value-equality
//! semantics for the domain model value types, plus the no-shared-mutable-
//! state guarantees around their list fields.
//!
//! Dart's "distinct instances" and "defensive copy" checks become ownership in
//! Rust (a value never aliases a caller's list); they are kept as checks that
//! an edit to one value leaves another untouched.

use std::sync::Arc;

use vocara_model::models::audio_device::AudioDevice;
use vocara_model::models::audio_fx::*;
use vocara_model::models::audio_track::{AudioClip, AudioTrack, AudioTrackKind};
use vocara_model::models::fx_bus::FxBus;
use vocara_model::models::lyric_line::LyricLine;
use vocara_model::models::practice_note::PracticeNote;
use vocara_model::models::song_practice::SongPractice;
use vocara_model::models::spectrogram::Spectrogram;
use vocara_model::models::take_version::TakeVersion;
use vocara_model::models::technique::Technique;
use vocara_model::models::technique_analysis::TechniqueAnalysis;
use vocara_model::models::technique_media::{TechniqueMedia, TechniqueMediaKind};
use vocara_model::models::voice_metrics::{Ltas, RegisterMap, VoiceMetrics, VoiceProfile};
use vocara_model::util::dart_date_time::DateTime;

fn t0() -> DateTime {
    DateTime::utc_ymd(2026, 1, 1)
}
fn t1() -> DateTime {
    DateTime::utc_ymd(2026, 1, 2)
}

/// Asserts [a] and [b] are equal by value, and that both differ from [other].
fn expect_value_equality<T: PartialEq + std::fmt::Debug>(a: &T, b: &T, other: &T) {
    assert_eq!(a, b);
    assert_ne!(a, other);
    assert_ne!(b, other);
}

fn rich_fx() -> TrackFx {
    let mut order = FxKind::VALUES.to_vec();
    order.reverse();
    TrackFx {
        eq_on: true,
        comp: CompSettings {
            threshold_db: -22.0,
            ratio: 4.0,
            ..CompSettings::DEFAULT
        },
        comp_on: true,
        delay: DelaySettings {
            time_ms: 250.0,
            feedback: 0.4,
            mix: 0.2,
        },
        send_db: -9.0,
        order,
        ..TrackFx::flat()
    }
}

fn metrics() -> Arc<VoiceMetrics> {
    let flat = |v: f32| vec![v; 4];
    Arc::new(VoiceMetrics {
        frames: 4,
        hop: 512,
        fft_size: 2048,
        sample_rate: 44100,
        ring: flat(0.0),
        tilt: flat(0.0),
        cpp: flat(0.0),
        loudness: flat(-20.0),
        f0: flat(200.0),
        f1: flat(f32::NAN),
        f2: flat(f32::NAN),
        f3: flat(f32::NAN),
        h1h2: flat(f32::NAN),
        active: vec![0; 4],
        ring_avg: 0.0,
        tilt_avg: 0.0,
        cpp_avg: 0.0,
        peak_dbfs: -6.0,
        clipped_fraction: 0.0,
        snr_db: f64::NAN,
    })
}

// --- TrackFx.flat aliasing regression ---

#[test]
fn editing_one_flat_instance_via_with_band_leaves_another_untouched() {
    let a = TrackFx::flat();
    let b = TrackFx::flat();
    let edited = a.with_band(
        0,
        EqBand {
            gain_db: 6.0,
            ..a.eq[0]
        },
    );
    assert_eq!(edited.eq[0].gain_db, 6.0);
    assert!(a.eq.iter().all(|band| band.gain_db == 0.0));
    assert!(b.eq.iter().all(|band| band.gain_db == 0.0));
}

#[test]
fn the_from_json_flat_fallback_shares_no_state_either() {
    let mut from_empty = TrackFx::from_json(&serde_json::json!({})).unwrap();
    from_empty.eq[0].gain_db = 6.0;
    assert_eq!(TrackFx::flat().eq[0].gain_db, 0.0);
    assert_eq!(FLAT_BANDS[0].gain_db, 0.0);
}

// --- copyWith defensive copies ---

#[test]
fn track_fx_copy_with_owns_the_provided_eq_and_order_lists() {
    let mut bands = TrackFx::flat().eq.clone();
    let mut order = FxKind::VALUES.to_vec();
    let fx = TrackFx {
        eq: bands.clone(),
        order: order.clone(),
        ..TrackFx::flat()
    };
    bands[0].gain_db = 12.0;
    order.pop();
    assert_eq!(fx.eq[0].gain_db, 0.0);
    assert_eq!(fx.order, FxKind::VALUES);
}

#[test]
fn fx_bus_copy_with_owns_the_provided_sends_list() {
    let mut sends = vec![TrackSend::new("b2", -6.0)];
    let bus = FxBus {
        sends: sends.clone(),
        ..FxBus::new("b1", "Bus", TrackFx::flat())
    };
    sends.clear();
    assert_eq!(bus.sends.len(), 1);
}

#[test]
fn song_practice_copy_with_owns_the_provided_lists() {
    let mut lyrics = vec![LyricLine::new("la", Some(100))];
    let mut versions = vec![Arc::new(TakeVersion::new("v1", "One", t0()))];
    let practice = SongPractice {
        lyrics: lyrics.clone(),
        versions: versions.clone(),
        ..SongPractice::new("p1", "Song", t0(), t0())
    };
    lyrics.clear();
    versions.clear();
    assert_eq!(practice.lyrics.len(), 1);
    assert_eq!(practice.versions.len(), 1);
}

#[test]
fn technique_copy_with_owns_the_provided_lists() {
    let mut tags = vec!["Quality".to_string()];
    let technique = Technique {
        tags: tags.clone(),
        ..Technique::new("t1", "Twang", t0(), t0())
    };
    tags.push("Register".into());
    assert_eq!(technique.tags, vec!["Quality".to_string()]);
}

// --- FX settings value equality ---

#[test]
fn eq_band() {
    let a = EqBand {
        q: 1.2,
        gain_db: 3.0,
        ..EqBand::new(EqBandKind::Peaking, 700.0)
    };
    expect_value_equality(&a, &a.clone(), &EqBand { gain_db: 4.0, ..a });
}

#[test]
fn comp_settings() {
    let a = CompSettings {
        threshold_db: -22.0,
        ratio: 4.0,
        ..CompSettings::DEFAULT
    };
    expect_value_equality(&a, &a.clone(), &CompSettings { ratio: 5.0, ..a });
}

#[test]
fn de_esser_settings() {
    let a = DeEsserSettings {
        freq: 7000.0,
        ..DeEsserSettings::DEFAULT
    };
    expect_value_equality(&a, &a.clone(), &DeEsserSettings { freq: 7500.0, ..a });
}

#[test]
fn gate_settings() {
    let a = GateSettings {
        threshold_db: -40.0,
        ..GateSettings::DEFAULT
    };
    expect_value_equality(
        &a,
        &a.clone(),
        &GateSettings {
            threshold_db: -42.0,
            ..a
        },
    );
}

#[test]
fn delay_settings() {
    let a = DelaySettings {
        time_ms: 250.0,
        mix: 0.2,
        ..DelaySettings::DEFAULT
    };
    expect_value_equality(
        &a,
        &a.clone(),
        &DelaySettings {
            time_ms: 300.0,
            ..a
        },
    );
}

#[test]
fn saturation_settings() {
    let a = SaturationSettings {
        drive_db: 9.0,
        mix: 0.3,
    };
    expect_value_equality(&a, &a.clone(), &SaturationSettings { mix: 0.4, ..a });
}

#[test]
fn doubler_settings() {
    let a = DoublerSettings {
        delay_ms: 14.0,
        mix: 0.35,
        ..DoublerSettings::DEFAULT
    };
    expect_value_equality(
        &a,
        &a.clone(),
        &DoublerSettings {
            delay_ms: 16.0,
            ..a
        },
    );
}

#[test]
fn limiter_settings() {
    let a = LimiterSettings {
        ceiling_db: -0.5,
        ..LimiterSettings::DEFAULT
    };
    expect_value_equality(
        &a,
        &a.clone(),
        &LimiterSettings {
            ceiling_db: -1.0,
            ..a
        },
    );
}

#[test]
fn reverb_settings() {
    let a = ReverbSettings {
        room_size: 0.6,
        wet: 0.3,
        ..ReverbSettings::DEFAULT
    };
    expect_value_equality(&a, &a.clone(), &ReverbSettings { wet: 0.35, ..a });
}

#[test]
fn track_send() {
    let a = TrackSend::new("b1", -6.0);
    expect_value_equality(
        &a,
        &a.clone(),
        &TrackSend {
            bus_id: "b2".into(),
            ..a.clone()
        },
    );
}

// --- TrackFx value equality ---

#[test]
fn track_fx_equal_by_value_across_distinct_list_instances() {
    expect_value_equality(
        &rich_fx(),
        &rich_fx(),
        &TrackFx {
            eq_on: false,
            ..rich_fx()
        },
    );
}

#[test]
fn track_fx_copy_with_round_trip_preserves_equality() {
    let fx = rich_fx();
    assert_eq!(fx.clone(), fx);
    let there = TrackFx {
        send_db: 0.0,
        ..fx.clone()
    };
    assert_eq!(
        TrackFx {
            send_db: fx.send_db,
            ..there
        },
        fx
    );
}

#[test]
fn track_fx_json_round_trip_preserves_equality() {
    let fx = rich_fx();
    assert_eq!(TrackFx::from_json(&fx.to_json()).unwrap(), fx);
}

#[test]
fn track_fx_a_differing_band_or_order_breaks_equality() {
    let fx = rich_fx();
    assert_ne!(
        fx.with_band(
            1,
            EqBand {
                gain_db: 2.0,
                ..fx.eq[1]
            }
        ),
        fx
    );
    assert_ne!(
        TrackFx {
            order: FxKind::VALUES.to_vec(),
            ..fx.clone()
        },
        fx
    );
}

// --- aggregate value equality ---

fn bus() -> FxBus {
    FxBus {
        color_value: Some(0xFF112233),
        volume: 0.8,
        pan: -0.2,
        output_bus_id: Some("b2".into()),
        sends: vec![TrackSend::new("b3", -12.0)],
        ..FxBus::new("b1", "Verb", rich_fx())
    }
}

#[test]
fn fx_bus() {
    expect_value_equality(
        &bus(),
        &bus(),
        &FxBus {
            muted: true,
            ..bus()
        },
    );
    assert_eq!(FxBus::from_json(&bus().to_json()).unwrap(), bus());
}

fn practice() -> SongPractice {
    SongPractice {
        versions: vec![Arc::new(TakeVersion::new("v1", "One", t0()))],
        active_version_id: Some("v1".into()),
        lyrics: vec![LyricLine::new("la", Some(100))],
        timeline_duration_ms: 60000,
        reverb: Some(ReverbSettings {
            wet: 0.3,
            ..ReverbSettings::DEFAULT
        }),
        master_volume: 0.9,
        fx_buses: vec![Arc::new(bus())],
        ..SongPractice::new("p1", "Song", t0(), t1())
    }
}

#[test]
fn song_practice() {
    expect_value_equality(
        &practice(),
        &practice(),
        &SongPractice {
            title: "Other".into(),
            ..practice()
        },
    );
    assert_eq!(practice().clone(), practice());
    assert_eq!(
        SongPractice::from_json(&practice().to_json()).unwrap(),
        practice()
    );
}

#[test]
fn take_version() {
    expect_value_equality(
        &TakeVersion::new("v1", "One", t0()),
        &TakeVersion::new("v1", "One", t0()),
        &TakeVersion::new("v1", "Two", t0()),
    );
}

#[test]
fn practice_note() {
    let note = || PracticeNote {
        id: "n1".into(),
        position_ms: 1000,
        end_ms: Some(2000),
        content: "breath".into(),
        created_at: t0(),
        updated_at: t1(),
    };
    expect_value_equality(
        &note(),
        &note(),
        &PracticeNote {
            content: "pitch".into(),
            ..note()
        },
    );
}

fn media() -> TechniqueMedia {
    TechniqueMedia {
        id: "m1".into(),
        path: "/sandbox/clip.m4a".into(),
        kind: TechniqueMediaKind::Audio,
        label: "Example".into(),
        added_at: t0(),
    }
}

#[test]
fn technique_media() {
    expect_value_equality(
        &media(),
        &media(),
        &TechniqueMedia {
            label: "Other".into(),
            ..media()
        },
    );
}

fn technique() -> Technique {
    Technique {
        aliases: vec!["Edge".into()],
        tags: vec!["Quality".into()],
        summary: "Bright, brassy quality.".into(),
        media: vec![media()],
        ..Technique::new("t1", "Twang", t0(), t1())
    }
}

#[test]
fn technique_value_equality() {
    expect_value_equality(
        &technique(),
        &technique(),
        &Technique {
            name: "Curbing".into(),
            ..technique()
        },
    );
    assert_eq!(
        Technique::from_json(&technique().to_json()).unwrap(),
        technique()
    );
}

#[test]
fn audio_device_compares_every_field() {
    let device = |name: &str| AudioDevice {
        rate: 48000,
        is_input: true,
        ..AudioDevice::new("u1", name)
    };
    // Same physical device (uid) but renamed: no longer equal by value.
    expect_value_equality(&device("Mic"), &device("Mic"), &device("Mic 2"));
}

// --- AudioClip value equality ---

fn clip() -> AudioClip {
    AudioClip {
        start_ms: 1500,
        waveform_peaks: vec![0.1, 0.8, 0.4],
        ..AudioClip::new("c1", "/sandbox/take.wav", 4000)
    }
}

#[test]
fn audio_clip_equal_by_value() {
    expect_value_equality(
        &clip(),
        &clip().clone(),
        &AudioClip {
            start_ms: 0,
            ..clip()
        },
    );
}

#[test]
fn audio_clip_differing_waveform_peaks_break_equality() {
    assert_ne!(
        AudioClip {
            waveform_peaks: vec![0.1, 0.8, 0.5],
            ..clip()
        },
        clip()
    );
}

#[test]
fn audio_clip_copy_with_round_trip_and_json_round_trip_preserve_equality() {
    let c = clip();
    let moved = AudioClip {
        start_ms: 0,
        ..c.clone()
    };
    assert_eq!(
        AudioClip {
            start_ms: c.start_ms,
            ..moved
        },
        c
    );
    assert_eq!(AudioClip::from_json(&c.to_json()).unwrap(), c);
}

#[test]
fn audio_clip_copy_with_owns_the_provided_waveform_peaks_list() {
    let mut peaks = vec![0.2, 0.6];
    let c = AudioClip {
        waveform_peaks: peaks.clone(),
        ..clip()
    };
    peaks[0] = 0.9;
    assert_eq!(c.waveform_peaks, vec![0.2, 0.6]);
}

// --- AudioTrack value equality ---

fn lane_clip(id: &str, start_ms: i64) -> AudioClip {
    AudioClip {
        start_ms,
        waveform_peaks: vec![0.3, 0.7],
        ..AudioClip::new(id, format!("/sandbox/{id}.wav"), 2000)
    }
}

fn track() -> AudioTrack {
    AudioTrack {
        clips: vec![lane_clip("c1", 0), lane_clip("c2", 5000)],
        notes: vec![PracticeNote {
            id: "n1".into(),
            position_ms: 1000,
            end_ms: None,
            content: "breath".into(),
            created_at: t0(),
            updated_at: t1(),
        }],
        color_value: Some(0xFF445566),
        volume: 0.8,
        pan: -0.1,
        version_id: Some("v1".into()),
        fx: Some(rich_fx()),
        bus_id: Some("b1".into()),
        sends: vec![TrackSend::new("b2", -6.0)],
        ..AudioTrack::new("tr1", AudioTrackKind::Vocal, "Take 1", t0())
    }
}

#[test]
fn audio_track_equal_by_value() {
    expect_value_equality(
        &track(),
        &track(),
        &AudioTrack {
            muted: true,
            ..track()
        },
    );
}

#[test]
fn audio_track_copy_with_round_trip_preserves_equality() {
    let t = track();
    assert_eq!(t.clone(), t);
    let quiet = AudioTrack {
        volume: 0.5,
        ..t.clone()
    };
    assert_eq!(
        AudioTrack {
            volume: t.volume,
            ..quiet
        },
        t
    );
}

#[test]
fn audio_track_json_round_trip_preserves_equality() {
    let t = track();
    assert_eq!(AudioTrack::from_json(&t.to_json()).unwrap(), t);
}

#[test]
fn two_tracks_differing_only_in_clip_order_are_not_equal() {
    // Clips are kept in creation order (a punch-in can land before an
    // existing clip), so order is part of the lane's value.
    let a = track();
    let mut reversed = a.clips.clone();
    reversed.reverse();
    let b = AudioTrack {
        clips: reversed,
        ..a.clone()
    };
    assert_ne!(b, a);
}

#[test]
fn a_differing_nested_clip_note_or_send_breaks_equality() {
    let t = track();
    assert_ne!(
        AudioTrack {
            clips: vec![lane_clip("c1", 0), lane_clip("c2", 5001)],
            ..t.clone()
        },
        t
    );
    assert_ne!(
        AudioTrack {
            notes: vec![],
            ..t.clone()
        },
        t
    );
    assert_ne!(
        AudioTrack {
            sends: vec![TrackSend::new("b2", -7.0)],
            ..t.clone()
        },
        t
    );
}

#[test]
fn audio_track_copy_with_owns_the_provided_clips_notes_and_sends_lists() {
    let mut clips = vec![lane_clip("c1", 0)];
    let mut sends = vec![TrackSend::new("b2", -6.0)];
    let t = AudioTrack {
        clips: clips.clone(),
        notes: vec![],
        sends: sends.clone(),
        ..track()
    };
    clips.clear();
    sends.clear();
    assert_eq!(t.clips.len(), 1);
    assert_eq!(t.sends.len(), 1);
}

// --- TechniqueAnalysis equality ---

#[test]
fn technique_analysis_field_wise_with_heavy_members_compared_by_identity() {
    let m = metrics();
    let spectrogram = Arc::new(Spectrogram {
        frames: 2,
        bins: 2,
        max_hz: 5000.0,
        sample_rate: 44100,
        fft_size: 2048,
        hop: 512,
        db: vec![0.0; 4],
        min_db: -80.0,
        max_db: 0.0,
    });
    let ltas = Arc::new(Ltas {
        db: vec![0.0; 4],
        bin_hz: 21.5,
        max_hz: 5000.0,
        max_db: -20.0,
        alpha_ratio: -12.0,
        singers_formant_db: 3.0,
        frame_count: 2,
    });
    let profile = Arc::new(VoiceProfile {
        low_midi: Some(48),
        high_midi: Some(72),
        tess_lo_midi: Some(55),
        tess_hi_midi: Some(65),
        passaggio_midi: None,
        passaggio_jump_db: 0.0,
        soft_db: -30.0,
        loud_db: -10.0,
        median_db: -20.0,
    });
    let register = Arc::new(RegisterMap {
        score: vec![0.0; 4],
        chest_pct: 50.0,
        mix_pct: 30.0,
        head_pct: 20.0,
        voiced_frames: 4,
    });
    let analysis = |metrics: Arc<VoiceMetrics>| TechniqueAnalysis {
        metrics,
        spectrogram: Arc::clone(&spectrogram),
        ltas: Arc::clone(&ltas),
        profile: Arc::clone(&profile),
        register: Arc::clone(&register),
        duration_ms: 1000,
    };
    // Same artifact instances: equal by value. A content-identical but
    // DISTINCT VoiceMetrics instance is unequal: the data-heavy analysis
    // types keep identity equality on purpose (comparing big Float32 buffers
    // element-wise is a perf trap).
    expect_value_equality(
        &analysis(Arc::clone(&m)),
        &analysis(Arc::clone(&m)),
        &analysis(metrics()),
    );
}
