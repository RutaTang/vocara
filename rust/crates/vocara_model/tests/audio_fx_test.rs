//! Port of `client/test/audio_fx_test.dart`.

use serde_json::{json, Value};
use vocara_model::models::audio_fx::*;
use vocara_model::models::audio_track::{AudioTrack, AudioTrackKind};
use vocara_model::models::fx_presets::FxPresets;
use vocara_model::models::song_practice::SongPractice;
use vocara_model::util::dart_date_time::DateTime;

fn without(mut v: Value, key: &str) -> Value {
    v.as_object_mut().unwrap().remove(key);
    v
}

fn with(mut v: Value, key: &str, value: Value) -> Value {
    v.as_object_mut().unwrap().insert(key.into(), value);
    v
}

#[test]
fn flat_default_is_transparent_and_off() {
    let fx = TrackFx::flat();
    assert_eq!(fx.eq.len(), 6);
    assert!(fx.eq.iter().all(|b| b.gain_db == 0.0));
    assert!(!fx.eq_on);
    assert!(!fx.comp_on);
    assert!(!fx.has_reverb_send());
}

#[test]
fn round_trips_through_json() {
    let fx = TrackFx {
        eq_on: true,
        comp_on: true,
        comp: CompSettings {
            threshold_db: -22.0,
            ratio: 4.0,
            makeup_db: 3.0,
            ..CompSettings::DEFAULT
        },
        send_db: -9.0,
        ..TrackFx::flat()
    }
    .with_band(
        0,
        EqBand {
            gain_db: 3.0,
            ..EqBand::with_q(EqBandKind::LowShelf, 100.0, 0.7)
        },
    );

    let back = TrackFx::from_json(&fx.to_json()).unwrap();
    assert!(back.eq_on);
    assert!(back.comp_on);
    assert_eq!(back.comp.threshold_db, -22.0);
    assert_eq!(back.comp.makeup_db, 3.0);
    assert_eq!(back.send_db, -9.0);
    assert_eq!(back.eq[0].gain_db, 3.0);
    assert_eq!(back.eq[0].kind, EqBandKind::LowShelf);
}

#[test]
fn legacy_4_band_eq_migrates_onto_the_6_fixed_slots_by_role() {
    // A pre-migration take saved with the old [lowShelf, peak, peak, highShelf]
    // layout. The high shelf must NOT end up on a peaking engine slot.
    let band = |kind, freq, q, gain_db| EqBand {
        kind,
        freq,
        q,
        gain_db,
        enabled: true,
    };
    let legacy = TrackFx::new(vec![
        band(EqBandKind::LowShelf, 120.0, 0.7, 4.0),
        band(EqBandKind::Peaking, 500.0, 1.0, -2.0),
        band(EqBandKind::Peaking, 3000.0, 1.0, 1.0),
        band(EqBandKind::HighShelf, 8000.0, 0.7, 3.0),
    ]);
    let back = TrackFx::from_json(&legacy.to_json()).unwrap();

    assert_eq!(back.eq.len(), 6);
    assert_eq!(
        back.eq.iter().map(|b| b.kind).collect::<Vec<_>>(),
        vec![
            EqBandKind::LowShelf,
            EqBandKind::Peaking,
            EqBandKind::Peaking,
            EqBandKind::Peaking,
            EqBandKind::Peaking,
            EqBandKind::HighShelf,
        ]
    );
    // Shelves preserved at the shelf slots (0 and 5), not lost to a peak slot.
    assert_eq!(back.eq[0].freq, 120.0);
    assert_eq!(back.eq[0].gain_db, 4.0);
    assert_eq!(back.eq[5].freq, 8000.0);
    assert_eq!(back.eq[5].gain_db, 3.0);
    // The two old peaks fill the first two peak slots.
    assert_eq!(back.eq[1].freq, 500.0);
    assert_eq!(back.eq[2].freq, 3000.0);
}

#[test]
fn insert_chain_order_round_trips_legacy_json_normalizes_to_all_kinds() {
    // Default chain = every plugin kind, in canonical order.
    assert_eq!(TrackFx::flat().order, FxKind::VALUES);

    // A saved custom order keeps its leading arrangement; missing kinds are
    // restored (so newly-added plugin types always show up).
    let fx = TrackFx {
        order: vec![FxKind::Comp, FxKind::Eq],
        ..TrackFx::flat()
    };
    let back = TrackFx::from_json(&fx.to_json()).unwrap();
    assert_eq!(back.order[..2], [FxKind::Comp, FxKind::Eq]);
    assert!(FxKind::VALUES.iter().all(|k| back.order.contains(k)));
    assert_eq!(back.order.len(), FxKind::VALUES.len());

    // Legacy take without an `order` key → all kinds, canonical order.
    let legacy = without(TrackFx::flat().to_json(), "order");
    assert_eq!(TrackFx::from_json(&legacy).unwrap().order, FxKind::VALUES);
}

#[test]
fn unknown_insert_kinds_are_dropped_not_aliased_to_eq() {
    // A newer-schema take with a plugin this build doesn't know. The unknown
    // kind must be dropped outright — before the fix it parsed as FxKind.eq,
    // which both kept a phantom entry and RELOCATED the real EQ insert to the
    // unknown plugin's chain position (an audible, persistent reorder).
    let j = with(
        TrackFx::flat().to_json(),
        "order",
        json!(["spaceEcho", "comp", "eq", "deEsser"]),
    );
    let back = TrackFx::from_json(&j).unwrap();

    // Unknown kind gone; known kinds keep their saved relative order (eq must
    // NOT jump in front of comp via the unknown slot).
    assert_eq!(back.order[..3], [FxKind::Comp, FxKind::Eq, FxKind::DeEsser]);
    // Missing known kinds are restored, so the chain is still complete.
    assert!(FxKind::VALUES.iter().all(|k| back.order.contains(k)));
    assert_eq!(back.order.len(), FxKind::VALUES.len());
}

#[test]
fn a_kind_missing_from_an_old_order_comes_back_at_its_default_place_not_after_the_reverb() {
    let names: Vec<&str> = FxKind::VALUES
        .iter()
        .filter(|k| **k != FxKind::DeEsser)
        .map(|k| k.name())
        .collect();
    let j = with(TrackFx::flat().to_json(), "order", json!(names));
    assert_eq!(TrackFx::from_json(&j).unwrap().order, FxKind::VALUES);

    // In a reordered chain: right after its default predecessors.
    let reordered = with(
        TrackFx::flat().to_json(),
        "order",
        json!(["reverb", "comp", "eq"]),
    );
    assert_eq!(
        TrackFx::from_json(&reordered).unwrap().order[..4],
        [FxKind::Reverb, FxKind::Comp, FxKind::Eq, FxKind::DeEsser]
    );
}

#[test]
fn an_eq_band_with_no_kind_keeps_its_slot_in_the_6_band_layout() {
    // Read as a peak, a low shelf missing its kind shifted every band one
    // slot over and dropped the 6 kHz band.
    let mut bands: Vec<Value> = TrackFx::flat()
        .eq
        .iter()
        .map(|b| EqBand { gain_db: 3.0, ..*b }.to_json())
        .collect();
    bands[0] = without(bands[0].clone(), "kind");
    let j = with(TrackFx::flat().to_json(), "eq", json!(bands));
    let back = TrackFx::from_json(&j).unwrap();
    assert_eq!(
        back.eq.iter().map(|b| b.kind).collect::<Vec<_>>(),
        TrackFx::flat()
            .eq
            .iter()
            .map(|b| b.kind)
            .collect::<Vec<_>>()
    );
    assert!(back.eq.iter().all(|b| b.gain_db == 3.0));
}

#[test]
fn from_json_clamps_out_of_range_values_to_the_editable_ranges() {
    // EqBand: gainDb to the EQ graph's ±15 dB (_eqMaxDb), q to the graph's
    // scroll-edit range 0.3..14. A stored gainDb=200 would otherwise become a
    // 10^10 shelf amplitude in the engine.
    let hot = EqBand::from_json(
        &json!({"kind": "lowShelf", "freq": 100.0, "q": 100.0, "gainDb": 200.0}),
        None,
    )
    .unwrap();
    assert_eq!(hot.gain_db, 15.0);
    assert_eq!(hot.q, 14.0);
    let cold = EqBand::from_json(
        &json!({"kind": "peaking", "freq": 1000.0, "q": 0.01, "gainDb": -200.0}),
        None,
    )
    .unwrap();
    assert_eq!(cold.gain_db, -15.0);
    assert_eq!(cold.q, 0.3);
    // In-range values pass through untouched.
    let ok = EqBand::from_json(
        &json!({"kind": "peaking", "freq": 700.0, "q": 2.0, "gainDb": -4.5}),
        None,
    )
    .unwrap();
    assert_eq!(ok.q, 2.0);
    assert_eq!(ok.gain_db, -4.5);

    // CompSettings: bounds mirror the panel sliders (-48..0, 1..12, 0..18).
    let comp =
        CompSettings::from_json(&json!({"thresholdDb": -60.0, "ratio": 20.0, "makeupDb": 24.0}))
            .unwrap();
    assert_eq!(comp.threshold_db, -48.0);
    assert_eq!(comp.ratio, 12.0);
    assert_eq!(comp.makeup_db, 18.0);

    // TrackSend: a dB level in [SEND_OFF_DB, 0]; a positive value would
    // over-drive the bus.
    let loud = TrackSend::from_json(&json!({"busId": "b1", "levelDb": 24.0})).unwrap();
    assert_eq!(loud.level_db, 0.0);
    let silent = TrackSend::from_json(&json!({"busId": "b1", "levelDb": -300.0})).unwrap();
    assert_eq!(silent.level_db, SEND_OFF_DB);
    let mid = TrackSend::from_json(&json!({"busId": "b1", "levelDb": -9.0})).unwrap();
    assert_eq!(mid.level_db, -9.0);
}

#[test]
fn reverb_settings_round_trips() {
    let r = ReverbSettings {
        room_size: 0.8,
        damping: 0.3,
        wet: 0.4,
        width: 0.9,
    };
    let back = ReverbSettings::from_json(&r.to_json()).unwrap();
    assert_eq!(back.room_size, 0.8);
    assert_eq!(back.damping, 0.3);
    assert_eq!(back.wet, 0.4);
    assert_eq!(back.width, 0.9);
}

// --- non-breaking persistence ---

#[test]
fn audio_track_with_fx_round_trips_legacy_json_has_null_fx() {
    let t = AudioTrack {
        fx: Some(TrackFx {
            eq_on: true,
            send_db: -6.0,
            ..TrackFx::flat()
        }),
        ..AudioTrack::new(
            "t1",
            AudioTrackKind::Vocal,
            "Take 1",
            DateTime::local_ymd(2026, 6, 18),
        )
        .with_audio("/x.wav", 0, 1000, vec![])
    };
    let back = AudioTrack::from_json(&t.to_json()).unwrap();
    let fx = back.fx.expect("fx survives");
    assert!(fx.eq_on);
    assert_eq!(fx.send_db, -6.0);

    // Legacy JSON without 'fx' → None (no migration step needed).
    let legacy = without(t.to_json(), "fx");
    assert!(AudioTrack::from_json(&legacy).unwrap().fx.is_none());
}

#[test]
fn song_practice_with_reverb_round_trips_legacy_json_has_null_reverb() {
    let d = DateTime::local_ymd(2026, 6, 18);
    let p = SongPractice {
        reverb: Some(ReverbSettings {
            wet: 0.3,
            ..ReverbSettings::DEFAULT
        }),
        ..SongPractice::new("p1", "Song", d, d)
    };
    let back = SongPractice::from_json(&p.to_json()).unwrap();
    assert_eq!(back.reverb.expect("reverb survives").wet, 0.3);

    let legacy = without(p.to_json(), "reverb");
    assert!(SongPractice::from_json(&legacy).unwrap().reverb.is_none());
}

// --- presets ---

#[test]
fn vocal_presets_are_well_formed() {
    let presets = FxPresets::vocal();
    assert!(presets.len() >= 12);
    for name in ["Flat", "Warm Lead", "Bright Lead", "Pop Throw"] {
        assert!(presets.iter().any(|p| p.name == name), "{name}");
    }
    // First slot is the dry reference with everything bypassed.
    let flat = &presets[0];
    assert_eq!(flat.name, "Flat");
    assert!(!flat.fx.eq_on);
    assert!(!flat.fx.comp_on);
    assert!(!flat.fx.reverb_on);
    for p in &presets {
        assert_eq!(p.fx.eq.len(), 6);
        assert!(!p.description.is_empty());
    }
    let warm = &presets.iter().find(|p| p.name == "Warm Lead").unwrap().fx;
    assert!(warm.comp_on);
}

#[test]
fn every_enabled_plugin_param_is_within_its_allowed_range() {
    let within = |v: f64, lo: f64, hi: f64| v >= lo && v <= hi;
    for p in FxPresets::vocal() {
        let fx = &p.fx;
        for b in &fx.eq {
            assert!(within(b.gain_db, -12.0, 12.0), "{}", p.name);
        }
        if fx.comp_on {
            // Bounds match the panel sliders + CompSettings::from_json clamps: a
            // preset must never be silently altered by its own app's clamps.
            assert!(within(fx.comp.threshold_db, -48.0, 0.0), "{}", p.name);
            assert!(within(fx.comp.ratio, 1.0, 12.0), "{}", p.name);
            assert!(within(fx.comp.makeup_db, 0.0, 18.0), "{}", p.name);
        }
        if fx.de_esser_on {
            assert!(within(fx.de_esser.freq, 2000.0, 16000.0), "{}", p.name);
            assert!(within(fx.de_esser.range_db, 0.0, 24.0), "{}", p.name);
        }
        if fx.gate_on {
            assert!(within(fx.gate.threshold_db, -80.0, 0.0), "{}", p.name);
            assert!(within(fx.gate.range_db, 0.0, 80.0), "{}", p.name);
        }
        if fx.delay_on {
            assert!(within(fx.delay.time_ms, 1.0, 1500.0), "{}", p.name);
            assert!(within(fx.delay.feedback, 0.0, 0.95), "{}", p.name);
            // An enabled time-based FX must be audible (non-zero mix).
            assert!(fx.delay.mix > 0.0, "{}", p.name);
        }
        if fx.saturation_on {
            assert!(within(fx.saturation.drive_db, 0.0, 36.0), "{}", p.name);
            assert!(fx.saturation.mix > 0.0, "{}", p.name);
        }
        if fx.doubler_on {
            assert!(within(fx.doubler.delay_ms, 1.0, 50.0), "{}", p.name);
            assert!(fx.doubler.mix > 0.0, "{}", p.name);
        }
        if fx.limiter_on {
            assert!(within(fx.limiter.ceiling_db, -12.0, 0.0), "{}", p.name);
        }
        if fx.reverb_on {
            assert!(fx.reverb.wet > 0.0, "{}", p.name);
            assert!(within(fx.reverb.wet, 0.0, 1.0), "{}", p.name);
        }
    }
}

#[test]
fn reverb_presets_include_dry_silent_and_wet_rooms() {
    let presets = FxPresets::reverb();
    let dry = presets.iter().find(|p| p.name == "Dry").unwrap().reverb;
    assert_eq!(dry.wet, 0.0);
    let hall = presets.iter().find(|p| p.name == "Hall").unwrap().reverb;
    assert!(hall.wet > 0.0);
}
