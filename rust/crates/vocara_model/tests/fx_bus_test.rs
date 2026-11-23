//! Port of `client/test/fx_bus_test.dart`.

use std::sync::Arc;

use serde_json::Value;
use vocara_model::models::audio_fx::{TrackFx, TrackSend};
use vocara_model::models::audio_track::{AudioTrack, AudioTrackKind};
use vocara_model::models::fx_bus::FxBus;
use vocara_model::models::song_practice::SongPractice;
use vocara_model::util::dart_date_time::DateTime;

fn without(mut v: Value, key: &str) -> Value {
    v.as_object_mut().unwrap().remove(key);
    v
}

fn take() -> AudioTrack {
    AudioTrack::new(
        "t1",
        AudioTrackKind::Vocal,
        "Take 1",
        DateTime::local_ymd(2026, 6, 19),
    )
    .with_audio("/x.wav", 0, 1000, vec![])
}

#[test]
fn round_trips_through_json_with_its_chain_and_routing() {
    let bus = FxBus {
        color_value: Some(0xFF112233),
        volume: 0.8,
        muted: true,
        output_bus_id: Some("bus2".into()),
        ..FxBus::new(
            "bus1",
            "Lead Vox",
            TrackFx {
                eq_on: true,
                send_db: -9.0,
                ..TrackFx::flat()
            },
        )
    };
    let back = FxBus::from_json(&bus.to_json()).unwrap();
    assert_eq!(back.id, "bus1");
    assert_eq!(back.name, "Lead Vox");
    assert_eq!(back.color_value, Some(0xFF112233));
    assert_eq!(back.volume, 0.8);
    assert!(back.muted);
    assert_eq!(back.output_bus_id.as_deref(), Some("bus2"));
    assert!(back.fx.eq_on);
    assert_eq!(back.fx.send_db, -9.0);
}

#[test]
fn bus_id_round_trips_copy_with_can_set_and_clear_it() {
    let t = AudioTrack {
        bus_id: Some("bus1".into()),
        pan: -0.5,
        ..take()
    };
    assert_eq!(t.bus_id.as_deref(), Some("bus1"));
    assert_eq!(t.pan, -0.5);
    let back = AudioTrack::from_json(&t.to_json()).unwrap();
    assert_eq!(back.bus_id.as_deref(), Some("bus1"));
    assert_eq!(back.pan, -0.5);
    // Legacy JSON without pan → centered.
    let no_pan = without(t.to_json(), "pan");
    assert_eq!(AudioTrack::from_json(&no_pan).unwrap().pan, 0.0);

    // Legacy JSON without busId → None (no migration needed).
    let legacy = without(t.to_json(), "busId");
    assert!(AudioTrack::from_json(&legacy).unwrap().bus_id.is_none());

    let rerouted = AudioTrack {
        bus_id: Some("bus2".into()),
        ..t.clone()
    };
    assert_eq!(rerouted.bus_id.as_deref(), Some("bus2"));
    let cleared = AudioTrack { bus_id: None, ..t };
    assert!(cleared.bus_id.is_none());
}

#[test]
fn track_send_round_trips_on_tracks_and_buses() {
    let t = AudioTrack {
        sends: vec![TrackSend::new("b1", -12.0), TrackSend::new("b2", 0.0)],
        ..take()
    };
    let back = AudioTrack::from_json(&t.to_json()).unwrap();
    assert_eq!(back.sends.len(), 2);
    assert_eq!(back.sends[0].bus_id, "b1");
    assert_eq!(back.sends[0].level_db, -12.0);
    // Legacy JSON without sends → empty.
    let legacy = without(t.to_json(), "sends");
    assert!(AudioTrack::from_json(&legacy).unwrap().sends.is_empty());

    let bus = FxBus {
        sends: vec![TrackSend::new("b2", -6.0)],
        ..FxBus::new("b1", "Reverb", TrackFx::flat())
    };
    let back_bus = FxBus::from_json(&bus.to_json()).unwrap();
    assert_eq!(back_bus.sends.len(), 1);
    assert_eq!(back_bus.sends[0].bus_id, "b2");
    assert_eq!(back_bus.sends[0].level_db, -6.0);
}

#[test]
fn song_practice_fx_buses_round_trip_legacy_json_has_no_buses() {
    let d = DateTime::local_ymd(2026, 6, 19);
    let p = SongPractice {
        fx_buses: vec![
            Arc::new(FxBus::new("b1", "Bus 1", TrackFx::flat())),
            Arc::new(FxBus {
                output_bus_id: Some("b1".into()),
                ..FxBus::new("b2", "Bus 2", TrackFx::flat())
            }),
        ],
        ..SongPractice::new("p1", "Song", d, d)
    };
    let back = SongPractice::from_json(&p.to_json()).unwrap();
    assert_eq!(back.fx_buses.len(), 2);
    assert_eq!(back.fx_buses[1].output_bus_id.as_deref(), Some("b1"));
    assert_eq!(
        back.bus_by_id(Some("b2")).map(|b| b.name.as_str()),
        Some("Bus 2")
    );

    let legacy = without(p.to_json(), "fxBuses");
    assert!(SongPractice::from_json(&legacy)
        .unwrap()
        .fx_buses
        .is_empty());
}
