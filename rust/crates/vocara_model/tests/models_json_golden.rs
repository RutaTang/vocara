//! Every line of `golden/models_json.txt` is what the Flutter app's own model
//! code made of a stored JSON shape (written by `golden/models_json.gen.dart`
//! on the Dart SDK): parsing the same input with the Rust model and encoding
//! it again must give the Dart output back byte for byte, and a shape the
//! Dart factory refused must be refused here too.

use serde_json::Value;
use vocara_model::models::audio_fx::{
    CompSettings, DeEsserSettings, DelaySettings, DoublerSettings, EqBand, GateSettings,
    LimiterSettings, ReverbSettings, SaturationSettings, TrackFx, TrackSend,
};
use vocara_model::models::audio_track::{AudioClip, AudioTrack};
use vocara_model::models::fx_bus::FxBus;
use vocara_model::models::fx_presets::FxPresets;
use vocara_model::models::lyric_line::LyricLine;
use vocara_model::models::practice_note::PracticeNote;
use vocara_model::models::song_practice::SongPractice;
use vocara_model::models::take_version::TakeVersion;
use vocara_model::models::technique::Technique;
use vocara_model::models::technique_marker::TechniqueMarker;
use vocara_model::models::technique_media::TechniqueMedia;
use vocara_model::util::dart_date_time::DateTime;
use vocara_model::util::dart_json::encode;

fn round_trip(kind: &str, input: &Value) -> Option<Value> {
    macro_rules! via {
        ($t:ty) => {
            <$t>::from_json(input).ok().map(|m| m.to_json())
        };
    }
    match kind {
        "EqBand" => EqBand::from_json(input, None).ok().map(|m| m.to_json()),
        "CompSettings" => via!(CompSettings),
        "DeEsserSettings" => via!(DeEsserSettings),
        "GateSettings" => via!(GateSettings),
        "DelaySettings" => via!(DelaySettings),
        "SaturationSettings" => via!(SaturationSettings),
        "DoublerSettings" => via!(DoublerSettings),
        "LimiterSettings" => via!(LimiterSettings),
        "ReverbSettings" => via!(ReverbSettings),
        "TrackSend" => via!(TrackSend),
        "TrackFx" => via!(TrackFx),
        "AudioClip" => via!(AudioClip),
        "AudioTrack" => via!(AudioTrack),
        "FxBus" => via!(FxBus),
        "LyricLine" => via!(LyricLine),
        "PracticeNote" => via!(PracticeNote),
        "TechniqueMarker" => via!(TechniqueMarker),
        "TakeVersion" => via!(TakeVersion),
        "TechniqueMedia" => via!(TechniqueMedia),
        "Technique" => via!(Technique),
        "SongPractice" => via!(SongPractice),
        other => panic!("unknown kind {other}"),
    }
}

/// Inputs decoded differently before the model sees them: Dart's `jsonDecode`
/// reads the integer literal `-0` as the int 0, serde_json as the double
/// -0.0. Dart's `jsonEncode` never writes `-0` (an int zero is `0`, a
/// negative-zero double `-0.0`), so only a hand-edited row can hold it.
const DECODER_DEVIATIONS: [&str; 1] = [r#"{"gainDb":-0}"#];

#[test]
fn rust_models_read_and_write_every_golden_shape_as_dart_does() {
    let golden = include_str!("golden/models_json.txt");
    let presets = FxPresets::vocal();
    let reverb_presets = FxPresets::reverb();
    let mut checked = 0;
    for line in golden.lines().filter(|l| !l.is_empty()) {
        let mut cols = line.splitn(3, '\t');
        let (kind, input, expected) = (
            cols.next().unwrap(),
            cols.next().unwrap(),
            cols.next().unwrap(),
        );
        if DECODER_DEVIATIONS.contains(&input) {
            continue;
        }
        let input_value: Value = serde_json::from_str(input).expect(line);
        let actual = match kind {
            "datetime" => {
                let text = input_value.as_str().unwrap();
                match DateTime::parse(text) {
                    Err(_) => "THROWS".to_string(),
                    Ok(d) if d.is_utc() => {
                        format!("{}|true|{}", d.to_iso8601_string(), d.micros_since_epoch())
                    }
                    Ok(d) => d.to_iso8601_string(),
                }
            }
            "preset" => {
                let name = input_value.as_str().unwrap();
                let p = presets.iter().find(|p| p.name == name).expect(name);
                encode(&p.fx.to_json()).unwrap()
            }
            "reverbPreset" => {
                let name = input_value.as_str().unwrap();
                let p = reverb_presets.iter().find(|p| p.name == name).expect(name);
                encode(&p.reverb.to_json()).unwrap()
            }
            "flat" => encode(&TrackFx::flat().to_json()).unwrap(),
            kind => match round_trip(kind, &input_value) {
                None => "THROWS".to_string(),
                Some(v) => encode(&v).unwrap(),
            },
        };
        assert_eq!(actual, expected, "{kind} {input}");
        checked += 1;
    }
    assert_eq!(presets.len(), 12);
    assert!(checked > 170, "only {checked} golden lines");
}
