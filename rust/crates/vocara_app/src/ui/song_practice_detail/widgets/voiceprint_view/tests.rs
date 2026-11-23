//! The voiceprint tabs' tests:
//!
//! - `voiceprint_tabs_test.dart`'s rendering cases — every tab against the
//!   analysis the REAL DSP produces from degenerate audio (no voiced frame,
//!   one frame, an all-NaN F0, a single spectrogram column: the inputs that
//!   divide by frames - 1, index [0] or normalise by a range of zero), and
//!   against a live monitor fed what a misbehaving driver could deliver.
//!   Range and Dynamics, which that test also rendered, are
//!   `voice_profile_view`'s; the monitor's own assertions are ported with
//!   `monitor_service`.
//! - The voiceprint parts of `analysis_frame_timing_test.dart` (the dock
//!   spectrogram's seek and the readouts under the playhead; its image and
//!   overlay placement are `spectrogram_painter`'s tests) and of
//!   `playhead_repaint_test.dart` (a playhead or screen tick leaves the
//!   tabs' still layers alone; the rest of that file needs the detail
//!   screen).
//!
//! A tab "renders" when the headless renderer draws it: layout, every canvas
//! and every text, without a panic (Flutter's `takeException`).

use std::f64::consts::PI;
use std::panic::{catch_unwind, AssertUnwindSafe};

use iced::advanced::renderer::Headless;
use iced::{Font, Settings};
use iced_test::runtime::user_interface::{self, UserInterface};
use iced_test::selector::Candidate;
use iced_test::Simulator;
use vocara_model::models::audio_track::AudioTrackKind;
use vocara_model::models::voice_metrics::{
    analyze_register, analyze_voice_profile, compute_ltas, VoiceProfile, VoiceProfileOptions,
};
use vocara_model::util::dart_date_time::DateTime;
use vocara_services::audio_dsp::{compute_spectrogram, compute_voice_metrics, AnalysisParams};

use super::*;
use crate::ui::core::theme::app_theme;
use crate::ui::core::widgets::spectrogram_painter::spectrogram_image_rect;
use crate::ui::song_practice_detail::widgets::live_spectrogram::tests::{fed, sine};

const SR: i64 = 48000;
const HOP: i64 = 512;
const FFT: i64 = 2048;

// ------------------------------------------------------------ the source

/// The test's [`AnalysisSource`] (`_Source`).
#[derive(Default)]
struct Source {
    track: Option<Arc<AudioTrack>>,
    metrics: Option<Arc<VoiceMetrics>>,
    spectrogram: Option<Arc<Spectrogram>>,
    profile: Option<Arc<VoiceProfile>>,
    ltas: Option<Arc<Ltas>>,
    register: Option<Arc<RegisterMap>>,
    loading: bool,
    /// The metrics half's flag; `loading` when unset.
    metrics_loading: Option<bool>,
    duration_ms: i64,
    /// Set for a live source, which also supplies the monitor.
    monitor: Option<Arc<MonitorService>>,
    position: Duration,
    revision: u64,
}

impl AnalysisSource for Source {
    fn selected_track(&self) -> Option<Arc<AudioTrack>> {
        self.track.clone()
    }
    fn analysis_start_ms(&self) -> i64 {
        0
    }
    fn analysis_duration_ms(&self) -> i64 {
        self.duration_ms
    }
    fn selected_metrics(&self) -> Option<Arc<VoiceMetrics>> {
        self.metrics.clone()
    }
    fn selected_spectrogram(&self) -> Option<Arc<Spectrogram>> {
        self.spectrogram.clone()
    }
    fn selected_profile(&self) -> Option<Arc<VoiceProfile>> {
        self.profile.clone()
    }
    fn selected_ltas(&self) -> Option<Arc<Ltas>> {
        self.ltas.clone()
    }
    fn selected_register(&self) -> Option<Arc<RegisterMap>> {
        self.register.clone()
    }
    fn spectrogram_loading(&self) -> bool {
        self.loading
    }
    fn metrics_loading(&self) -> bool {
        self.metrics_loading.unwrap_or(self.loading)
    }
    fn live_monitor(&self) -> bool {
        self.monitor.is_some()
    }
    fn monitor(&self) -> &MonitorService {
        self.monitor
            .as_deref()
            .expect("a non-live source must never reach the monitor")
    }
    fn position(&self) -> Duration {
        self.position
    }
    fn revision(&self) -> u64 {
        self.revision
    }
}

fn track(duration_ms: i64) -> Arc<AudioTrack> {
    Arc::new(
        AudioTrack::new(
            "t1",
            AudioTrackKind::Vocal,
            "Take 1",
            DateTime::utc_ymd(2026, 1, 1),
        )
        .with_audio("/x.wav", 0, duration_ms, Vec::new()),
    )
}

/// A source carrying the analysis the real DSP produces for `samples`.
fn analyzed(samples: &[f32]) -> Source {
    let params = AnalysisParams::default();
    let metrics = Arc::new(compute_voice_metrics(samples, SR, params));
    let spec = Arc::new(compute_spectrogram(samples, SR, params));
    let ms = round_to_i64(samples.len() as f64 / SR as f64 * 1000.0);
    Source {
        track: Some(track(ms)),
        profile: Some(Arc::new(analyze_voice_profile(
            &metrics,
            VoiceProfileOptions::default(),
        ))),
        ltas: Some(Arc::new(compute_ltas(&spec, Some(&metrics)))),
        register: Some(Arc::new(analyze_register(
            &metrics,
            RegisterOptions::default(),
        ))),
        metrics: Some(metrics),
        spectrogram: Some(spec),
        duration_ms: ms,
        ..Source::default()
    }
}

/// A live source whose monitor has been fed `samples` from the mic.
fn live(samples: &[f32]) -> Source {
    Source {
        monitor: Some(Arc::new(fed(samples))),
        ..Source::default()
    }
}

// ------------------------------------------------------------- the tabs

type Tab = for<'a> fn(&'a dyn AnalysisSource, &'a VoiceprintState) -> Element<'a, Message>;

fn spectrogram<'a>(s: &'a dyn AnalysisSource, state: &'a VoiceprintState) -> Element<'a, Message> {
    spectrogram_tab(s, state)
}
fn power<'a>(s: &'a dyn AnalysisSource, _: &'a VoiceprintState) -> Element<'a, Message> {
    power_tab(s)
}
fn vowel<'a>(s: &'a dyn AnalysisSource, _: &'a VoiceprintState) -> Element<'a, Message> {
    vowel_tab(s)
}
fn pitch<'a>(s: &'a dyn AnalysisSource, _: &'a VoiceprintState) -> Element<'a, Message> {
    pitch_tab(s)
}
fn timbre<'a>(s: &'a dyn AnalysisSource, _: &'a VoiceprintState) -> Element<'a, Message> {
    timbre_tab(s)
}
fn ltas<'a>(s: &'a dyn AnalysisSource, _: &'a VoiceprintState) -> Element<'a, Message> {
    ltas_tab(s)
}
fn vibrato<'a>(s: &'a dyn AnalysisSource, _: &'a VoiceprintState) -> Element<'a, Message> {
    vibrato_tab(s)
}
fn register<'a>(s: &'a dyn AnalysisSource, _: &'a VoiceprintState) -> Element<'a, Message> {
    register_tab(s)
}

const TABS: [(&str, Tab); 8] = [
    ("Spectrogram", spectrogram),
    ("Power", power),
    ("Vowel", vowel),
    ("Pitch", pitch),
    ("Timbre", timbre),
    ("LTAS", ltas),
    ("Vibrato", vibrato),
    ("Register", register),
];

/// The dock is a wide, short strip: a realistic one, so the tabs lay out the
/// way they ship.
const DOCK: Size = Size {
    width: 1440.0,
    height: 260.0,
};

/// The host's image state for `src`, its render landed (as the dock's task
/// would land it).
fn synced(src: &dyn AnalysisSource) -> VoiceprintState {
    let mut state = VoiceprintState::new();
    if let Some(request) = state.sync_request(src) {
        state.rendered(request.run());
    }
    state
}

fn simulator<'a>(
    tab: Tab,
    src: &'a dyn AnalysisSource,
    state: &'a VoiceprintState,
    size: Size,
) -> Simulator<'a, Message> {
    Simulator::with_size(Settings::default(), size, tab(src, state))
}

/// Every tab, one at a time: a tab that panics must not be masked by a
/// sibling, and the failure names which one.
fn pump_tabs(src: &Source) {
    let state = synced(src);
    for (name, tab) in TABS {
        let drawn = catch_unwind(AssertUnwindSafe(|| {
            let mut ui = simulator(tab, src, &state, DOCK);
            ui.snapshot(&app_theme()).expect("snapshot");
        }));
        assert!(drawn.is_ok(), "the {name} tab panicked");
    }
}

/// Every text on screen.
fn texts(ui: &mut Simulator<'_, Message>) -> Vec<String> {
    let mut found = Vec::new();
    let _ = ui.find(|candidate: Candidate<'_>| {
        if let Candidate::Text { content, .. } = candidate {
            found.push(content.to_string());
        }
        None::<()>
    });
    found
}

// ----------------------------------------------- voiceprint_tabs_test

// One circle per voiced frame (a paint and a colour lerp each) was ~56k
// calls per paint of a 10-minute take, on every frame of a dock resize.
#[test]
fn the_register_map_draws_its_dots_in_batches_not_one_by_one() {
    let src = analyzed(&sine(48000, 220.0, SR as f64));
    let m = src.metrics.clone().unwrap();
    let reg = src.register.clone().unwrap();
    let voiced = (0..m.frames)
        .filter(|&fr| m.f0[fr] > 0.0 && !reg.score[fr].is_nan())
        .count();
    assert!(voiced > 50, "the take must have voiced frames");
    let range = pitch_range_of(&m);
    let batches = register_batches(
        &m,
        &reg,
        range.lo,
        range.hi,
        src.duration_ms,
        Size::new(1358.0, 200.0),
    );
    assert!(!batches.is_empty());
    assert!(batches.len() <= REGISTER_SHADES, "one fill per shade");
    let dots: usize = batches.iter().map(|b| b.dots.len()).sum();
    assert_eq!(dots, voiced, "every voiced frame, once");
    // Chest to head.
    for pair in batches.windows(2) {
        assert!(pair[0].color.r >= pair[1].color.r);
    }
    let state = VoiceprintState::new();
    simulator(register, &src, &state, DOCK)
        .snapshot(&app_theme())
        .expect("the register tab draws");
}

#[test]
fn every_tab_renders_a_normal_take() {
    pump_tabs(&analyzed(&sine(48000, 220.0, SR as f64)));
}

#[test]
fn every_tab_renders_a_silent_take() {
    // The muted-input case: metrics exist, but nothing is voiced, so every
    // per-frame curve is empty of usable values and the aggregates are NaN.
    let src = analyzed(&[0.0; 48000]);
    assert!(src.metrics.as_ref().unwrap().frames > 0);
    pump_tabs(&src);
}

#[test]
fn every_tab_renders_a_take_stopped_after_one_buffer() {
    pump_tabs(&analyzed(&sine(256, 220.0, SR as f64)));
}

#[test]
fn every_tab_renders_a_single_sample_take() {
    pump_tabs(&analyzed(&sine(1, 220.0, SR as f64)));
}

#[test]
fn every_tab_renders_a_dc_take() {
    pump_tabs(&analyzed(&[1.0; 48000]));
}

#[test]
fn every_tab_renders_audio_carrying_non_finite_samples() {
    let mut data = sine(48000, 220.0, SR as f64);
    data[100] = f32::NAN;
    data[200] = f32::INFINITY;
    data[300] = f32::NEG_INFINITY;
    pump_tabs(&analyzed(&data));
}

#[test]
fn every_tab_renders_with_nothing_selected() {
    let src = Source::default();
    pump_tabs(&src);
    let state = VoiceprintState::new();
    for (name, tab) in TABS {
        let mut ui = simulator(tab, &src, &state, DOCK);
        assert!(ui.find(SELECT_TAKE_HINT).is_ok(), "{name}");
    }
}

#[test]
fn a_backing_track_is_not_analyzed() {
    let mut backing = AudioTrack::clone(&track(1000));
    backing.kind = AudioTrackKind::Accompaniment;
    let src = Source {
        track: Some(Arc::new(backing)),
        ..analyzed(&sine(4800, 220.0, SR as f64))
    };
    let state = synced(&src);
    for (name, tab) in TABS {
        let mut ui = simulator(tab, &src, &state, DOCK);
        assert!(ui.find(SELECT_TAKE_HINT).is_ok(), "{name}");
    }
}

#[test]
fn every_tab_renders_while_the_analysis_is_still_computing() {
    pump_tabs(&Source {
        track: Some(track(1000)),
        loading: true,
        duration_ms: 1000,
        ..Source::default()
    });
}

#[test]
fn a_take_without_analysis_says_what_can_be_analyzed() {
    let src = Source {
        track: Some(track(1000)),
        duration_ms: 1000,
        ..Source::default()
    };
    let state = VoiceprintState::new();
    let mut ui = simulator(spectrogram, &src, &state, DOCK);
    assert!(ui.find(NO_SPECTROGRAM_HINT).is_ok());
    let mut ui = simulator(timbre, &src, &state, DOCK);
    assert!(ui.find(NO_METRICS_HINT).is_ok());
}

#[test]
fn each_tab_waits_only_on_the_half_of_the_analysis_it_reads() {
    // The halves compute apart (the byte budget drops one, a folded-out lane
    // asks for its own): one flag for both hid a held spectrogram behind a
    // spinner, and split the wrong way the metric tabs said a take still
    // computing could not be analyzed.
    let whole = analyzed(&sine(48000, 220.0, SR as f64));
    let reads_spectrogram_only = ["Spectrogram", "Power"];
    let spins = |tab: Tab, src: &Source| {
        let state = synced(src);
        let mut ui = simulator(tab, src, &state, DOCK);
        ui.snapshot(&app_theme()).expect("draws");
        ui.find(ANALYZING_HINT).is_ok()
    };
    let metrics_computing = Source {
        track: whole.track.clone(),
        spectrogram: whole.spectrogram.clone(),
        metrics_loading: Some(true),
        duration_ms: whole.duration_ms,
        ..Source::default()
    };
    let spectrogram_computing = Source {
        track: whole.track.clone(),
        metrics: whole.metrics.clone(),
        profile: whole.profile.clone(),
        register: whole.register.clone(),
        loading: true,
        metrics_loading: Some(false),
        duration_ms: whole.duration_ms,
        ..Source::default()
    };
    for (name, tab) in TABS {
        let spectral = reads_spectrogram_only.contains(&name);
        assert_eq!(
            spins(tab, &metrics_computing),
            !spectral,
            "{name}, spectrogram held, metrics computing"
        );
        if name == "LTAS" {
            continue; // reads both halves
        }
        assert_eq!(
            spins(tab, &spectrogram_computing),
            spectral,
            "{name}, metrics held, spectrogram computing"
        );
    }
}

#[test]
fn outside_the_take_the_readouts_are_empty_not_its_edge_frame() {
    // The playhead was clamped into the subject's span, so past the take's
    // end the metrics bar (and the spectrum, vowel and vibrato readouts)
    // kept showing its LAST frame as if it were playing.
    let mut src = analyzed(&sine(24000, 220.0, SR as f64)); // 0.5 s, voiced
    let state = VoiceprintState::new();
    src.position = Duration::from_millis(250);
    let shown = texts(&mut simulator(timbre, &src, &state, DOCK));
    assert!(
        shown.iter().any(|t| t.contains("Hz")),
        "over the take: {shown:?}"
    );

    src.position = Duration::from_secs(3);
    let shown = texts(&mut simulator(timbre, &src, &state, DOCK));
    let hz_readout = |t: &String| {
        t.strip_suffix(" Hz")
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    };
    assert!(!shown.iter().any(hz_readout), "{shown:?}");
    // The other readouts under the playhead read nothing there either.
    let m = src.metrics.clone().unwrap();
    let spec = src.spectrogram.clone().unwrap();
    let (column, formants) = power_readout(src.position, src.duration_ms, 0, &spec, Some(&m));
    assert!(column.is_empty() && formants.is_empty());
    let (f1, f2) = vowel_formants(src.position, src.duration_ms, 0, &m);
    assert!(f1.is_nan() && f2.is_nan());
    assert!(playhead_vibrato(src.position, src.duration_ms, 0, &m).is_none());
    // Over the take they read its frame.
    let at = Duration::from_millis(250);
    let (column, formants) = power_readout(at, src.duration_ms, 0, &spec, Some(&m));
    assert_eq!(column.len(), spec.bins);
    assert_eq!(formants.len(), 3);
}

#[test]
fn a_subject_with_a_zero_length_span_does_not_divide_by_it() {
    // The views map the playhead onto the subject by dividing by its span.
    let base = analyzed(&sine(4800, 220.0, SR as f64));
    pump_tabs(&Source {
        track: Some(track(0)),
        duration_ms: 0,
        ..base
    });
}

#[test]
fn the_ltas_stats_read_a_dash_rather_than_fail_on_a_non_finite_value() {
    // Dart's int.round() threw on NaN / Infinity, which replaced the whole
    // tab with an error widget; a stat that cannot be measured reads as a
    // dash, like the rest of the dock's readouts.
    let base = analyzed(&sine(48000, 220.0, SR as f64));
    let l = Ltas::clone(base.ltas.as_ref().unwrap());
    let src = Source {
        ltas: Some(Arc::new(Ltas {
            alpha_ratio: f64::NAN,
            singers_formant_db: f64::INFINITY,
            ..l
        })),
        ..base
    };
    let state = synced(&src);
    let mut ui = simulator(ltas, &src, &state, DOCK);
    ui.snapshot(&app_theme()).expect("draws");
    let dashes = texts(&mut ui).iter().filter(|t| *t == "—").count();
    assert_eq!(dashes, 2);
}

#[test]
fn the_ltas_tab_names_its_average_and_its_stats() {
    let src = analyzed(&sine(48000, 220.0, SR as f64));
    let state = synced(&src);
    let shown = texts(&mut simulator(ltas, &src, &state, DOCK));
    assert!(
        shown.iter().any(|t| t == "LTAS · AVERAGE SPECTRUM"),
        "{shown:?}"
    );
    assert!(shown.iter().any(|t| t == "SINGER'S FORMANT  "));
    assert!(shown.iter().any(|t| t.ends_with(" dB")));
}

// -------------------------------------------------- the LIVE monitor path

// The views the mic drives: a rolling spectrogram, the tuner, the live
// spectrum strip, the live LTAS, vibrato and register panels.
// MonitorService::feed_samples takes raw mic samples straight from the
// input driver, so these feed it what a misbehaving driver could deliver.

#[test]
fn live_renders_a_fed_monitor() {
    let src = live(&sine(24000, 220.0, SR as f64));
    pump_tabs(&src);
    let state = VoiceprintState::new();
    let shown = texts(&mut simulator(spectrogram, &src, &state, DOCK));
    assert!(shown.iter().any(|t| t == "LIVE"), "{shown:?}");
    assert!(!shown
        .iter()
        .any(|t| t
            == crate::ui::song_practice_detail::widgets::live_spectrogram::LIVE_SPECTROGRAM_HINT));
}

#[test]
fn live_renders_a_monitor_fed_silence() {
    let src = live(&[0.0; 24000]);
    pump_tabs(&src);
    let state = VoiceprintState::new();
    assert!(simulator(ltas, &src, &state, DOCK)
        .find("Sing into the mic to build your live average spectrum.")
        .is_ok());
    assert!(simulator(register, &src, &state, DOCK)
        .find("Sing into the mic to map your registers live.")
        .is_ok());
    assert!(simulator(pitch, &src, &state, DOCK)
        .find("Sing a sustained note to tune…")
        .is_ok());
}

#[test]
fn live_renders_a_monitor_fed_non_finite_samples() {
    let mut data = sine(24000, 220.0, SR as f64);
    data[100] = f32::NAN;
    data[200] = f32::INFINITY;
    data[300] = f32::NEG_INFINITY;
    pump_tabs(&live(&data));
}

#[test]
fn live_a_nan_inside_a_sustained_note_leaves_the_tabs_drawable() {
    // The frame carrying the NaN still counts as voiced (the gate rides
    // through a few unvoiced frames), so it reaches the running sums; the
    // monitor scrubs it (its own test), and the LTAS tab formats the result.
    let mut data = sine(48000, 220.0, SR as f64);
    data[30000] = f32::NAN;
    let src = live(&data);
    {
        let m = src.monitor().read();
        let ltas = m.live_ltas().expect("a held note accumulates an LTAS");
        assert!(ltas.alpha_ratio.is_finite());
        assert!(ltas.singers_formant_db.is_finite());
        assert!(m.tilt_avg().is_finite() && m.cpp_avg().is_finite());
    }
    pump_tabs(&src);
    let state = VoiceprintState::new();
    let shown = texts(&mut simulator(ltas, &src, &state, DOCK));
    assert!(
        shown.iter().any(|t| t == "LTAS · RUNNING AVERAGE"),
        "{shown:?}"
    );
}

#[test]
fn live_renders_a_monitor_started_but_never_fed() {
    let monitor = MonitorService::default();
    monitor.start();
    pump_tabs(&Source {
        monitor: Some(Arc::new(monitor)),
        ..Source::default()
    });
}

#[test]
fn a_held_live_note_tunes_and_maps_its_register() {
    let src = live(&sine(48000, 220.0, SR as f64));
    let state = VoiceprintState::new();
    // 220 Hz is A3, in tune.
    let shown = texts(&mut simulator(pitch, &src, &state, DOCK));
    assert!(shown.iter().any(|t| t == "A3"), "{shown:?}");
    let shown = texts(&mut simulator(register, &src, &state, DOCK));
    assert!(
        shown
            .iter()
            .any(|t| t.starts_with("chest ") && t.ends_with('%')),
        "{shown:?}"
    );
}

// ---------------------------------------- analysis_frame_timing_test

/// Silence, then a steady 220 Hz tone from exactly 500 ms; 1200 ms long.
fn burst() -> Vec<f32> {
    let n = (1200 * SR / 1000) as usize;
    let onset = (500 * SR / 1000) as usize;
    (0..n)
        .map(|i| {
            if i < onset {
                0.0
            } else {
                (0.3 * (2.0 * PI * 220.0 * (i - onset) as f64 / SR as f64).sin()) as f32
            }
        })
        .collect()
}

/// The window-centre time of frame `f` for the production framing, spelled
/// out rather than read back through the mapping under test.
fn centre_seconds(f: i64) -> f64 {
    (f * HOP + FFT / 2) as f64 / SR as f64
}

/// Where the take stack sits in a dock tab: past the tab's 16 px padding
/// and the 46 px axis, to the 20 px right padding.
const STACK_LEFT: f32 = 16.0 + AXIS_WIDTH;

fn stack_width(window: Size) -> f32 {
    window.width - STACK_LEFT - 20.0
}

#[test]
fn a_tap_on_a_column_seeks_to_its_frame_and_the_readouts_read_it() {
    let samples = burst();
    let params = AnalysisParams::default();
    let m = Arc::new(compute_voice_metrics(&samples, SR, params));
    let spec = Arc::new(compute_spectrogram(&samples, SR, params));
    let src = Source {
        track: Some(track(1200)),
        metrics: Some(Arc::clone(&m)),
        spectrogram: Some(Arc::clone(&spec)),
        duration_ms: 1200,
        ..Source::default()
    };
    let window = Size::new(1440.0, 300.0);
    let state = synced(&src);
    // The image's column 45 on the stack's axis.
    let w = stack_width(window);
    let dst = spectrogram_image_rect(spec.as_ref(), 1200, Size::new(w, 200.0)).unwrap();
    let col_w = dst.width / spec.frames as f32;
    let x = dst.x + (45.0 + 0.5) * col_w;

    let mut ui = simulator(spectrogram, &src, &state, window);
    ui.point_at(Point::new(STACK_LEFT + x, 150.0));
    ui.simulate(iced_test::simulator::click());
    let seeks: Vec<Duration> = ui
        .into_messages()
        .filter_map(|m| match m {
            Message::Seek(at) => Some(at),
            _ => None,
        })
        .collect();
    assert_eq!(seeks.len(), 1, "the press seeks once; the release does not");
    let at = seeks[0].as_millis() as f64 / 1000.0;
    assert!((at - spec.time_of_frame(45)).abs() < 0.001, "{at}");
    assert_eq!(spec.frame_at_time(at), 45);
    assert_eq!(m.frame_at_time(at), 45);
    assert!((spec.time_of_frame(45) - centre_seconds(45)).abs() < 1e-12);
}

#[test]
fn a_drag_on_the_spectrogram_seeks_along_with_the_pointer() {
    let src = analyzed(&sine(48000, 220.0, SR as f64));
    let window = Size::new(1440.0, 300.0);
    let state = synced(&src);
    let w = stack_width(window);
    let mut ui = simulator(spectrogram, &src, &state, window);
    let at = |x: f32| Point::new(STACK_LEFT + x, 150.0);
    let moved = |p: Point| iced::Event::Mouse(mouse::Event::CursorMoved { position: p });
    ui.point_at(at(w / 4.0));
    ui.simulate([iced::Event::Mouse(mouse::Event::ButtonPressed(
        mouse::Button::Left,
    ))]);
    // Within the hit slop: still a tap.
    ui.point_at(at(w / 4.0 + 0.5));
    ui.simulate([moved(at(w / 4.0 + 0.5))]);
    ui.point_at(at(w / 2.0));
    ui.simulate([moved(at(w / 2.0))]);
    // Past the panel's right edge: clamped to the subject's end.
    ui.point_at(at(w + 300.0));
    ui.simulate([moved(at(w + 300.0))]);
    ui.simulate([iced::Event::Mouse(mouse::Event::ButtonReleased(
        mouse::Button::Left,
    ))]);
    // A move after the release seeks nothing.
    ui.point_at(at(10.0));
    ui.simulate([moved(at(10.0))]);
    let seeks: Vec<i64> = ui
        .into_messages()
        .filter_map(|m| match m {
            Message::Seek(at) => Some(at.as_millis() as i64),
            _ => None,
        })
        .collect();
    assert_eq!(seeks, vec![250, 500, 1000]);
}

/// Hand-built metrics on the production framing whose readouts change at a
/// frame boundary: frames 0..19 sing A3 (F1 300 Hz), frames 20..39 sing C4
/// (F1 750 Hz). A playhead two hops early or late reads the wrong side.
fn step_metrics() -> Arc<VoiceMetrics> {
    let n = 40;
    let curve =
        |lo: f32, hi: f32| -> Vec<f32> { (0..n).map(|f| if f < 20 { lo } else { hi }).collect() };
    Arc::new(VoiceMetrics {
        frames: n,
        hop: HOP,
        fft_size: FFT,
        sample_rate: SR,
        ring: curve(-10.0, -10.0),
        tilt: curve(-5.0, -5.0),
        cpp: curve(10.0, 10.0),
        loudness: curve(-20.0, -20.0),
        f0: curve(220.0, 261.6256),
        f1: curve(300.0, 750.0),
        f2: curve(2300.0, 1150.0),
        f3: vec![f32::NAN; n],
        h1h2: vec![f32::NAN; n],
        active: vec![1; n],
        ring_avg: -10.0,
        tilt_avg: -5.0,
        cpp_avg: 10.0,
        peak_dbfs: f64::NAN,
        clipped_fraction: 0.0,
        snr_db: f64::NAN,
    })
}

#[test]
fn the_timbre_bar_and_the_vowel_chart_read_the_same_frame() {
    let step = step_metrics();
    // The last A3 frame's centre (224 ms): a start-anchored mapping reads
    // two hops ahead, into the C4 frames.
    let at_last_a3 = Duration::from_millis(round_to_i64(centre_seconds(19) * 1000.0) as u64);
    let src = Source {
        track: Some(track(500)),
        metrics: Some(Arc::clone(&step)),
        duration_ms: 500,
        position: at_last_a3,
        ..Source::default()
    };
    let state = VoiceprintState::new();
    let shown = texts(&mut simulator(timbre, &src, &state, DOCK));
    assert!(shown.iter().any(|t| t == "220 Hz"), "{shown:?}");
    assert!(shown.iter().any(|t| t == "A3 +0¢"), "{shown:?}");
    assert_eq!(vowel_formants(at_last_a3, 500, 0, &step), (300.0, 2300.0));
    simulator(vowel, &src, &state, DOCK)
        .snapshot(&app_theme())
        .expect("the vowel tab draws");
}

// ------------------------------------------------- playhead_repaint_test

/// A headless renderer to draw a rebuilt interface with.
fn renderer() -> iced::Renderer {
    iced_test::futures::futures::executor::block_on(<iced::Renderer as Headless>::new(
        Font::with_name("Fira Sans"),
        Pixels(16.0),
        None,
    ))
    .expect("a headless renderer")
}

fn still_layer_draws() -> usize {
    STILL_LAYER_DRAWS.with(std::cell::Cell::get)
}

/// Rebuilds a tab across frames as the app does — a new view each frame,
/// diffed into the last frame's widget tree — and counts what each frame's
/// still layers recorded.
struct Frames {
    renderer: iced::Renderer,
    cache: user_interface::Cache,
}

impl Frames {
    fn new() -> Self {
        Self {
            renderer: renderer(),
            cache: user_interface::Cache::default(),
        }
    }

    /// Draws a frame of `tab` over `src`; how many still layers recorded.
    fn draw(&mut self, tab: Tab, src: &Source) -> usize {
        let before = still_layer_draws();
        let state = VoiceprintState::new();
        let mut ui = UserInterface::build(
            tab(src, &state),
            DOCK,
            std::mem::take(&mut self.cache),
            &mut self.renderer,
        );
        ui.draw(
            &mut self.renderer,
            &app_theme(),
            &iced::advanced::renderer::Style {
                text_color: Color::WHITE,
            },
            mouse::Cursor::Unavailable,
        );
        self.cache = ui.into_cache();
        still_layer_draws() - before
    }
}

#[test]
fn a_playhead_tick_leaves_the_timbre_curves_and_the_register_map_alone() {
    // The metrics bar's readouts change on each tick; the curves (every frame
    // of the take) and the register map must not be recorded again.
    for tab in [timbre as Tab, register as Tab] {
        let mut src = analyzed(&sine(48000, 220.0, SR as f64));
        let mut frames = Frames::new();
        src.position = Duration::from_millis(100);
        assert_eq!(frames.draw(tab, &src), 1, "the first frame records it");
        for ms in (116..=260).step_by(16) {
            src.position = Duration::from_millis(ms);
            assert_eq!(frames.draw(tab, &src), 0, "a tick at {ms} ms");
        }
    }
}

#[test]
fn a_screen_notification_leaves_the_still_layers_alone() {
    // Every notification of the screen (a fader tick on any lane) rebuilds
    // the dock's tab; a layer keyed by what it shows is not re-recorded.
    for tab in [timbre as Tab, register as Tab] {
        let mut src = analyzed(&sine(48000, 220.0, SR as f64));
        let mut frames = Frames::new();
        assert_eq!(frames.draw(tab, &src), 1);
        src.revision += 1;
        assert_eq!(frames.draw(tab, &src), 0);
        // A new analysis (a new Arc) is drawn again.
        let again = analyzed(&sine(48000, 220.0, SR as f64));
        src.metrics = again.metrics;
        src.register = again.register;
        assert_eq!(frames.draw(tab, &src), 1, "a new analysis redraws");
    }
}

// ----------------------------------------------------------- the pieces

#[test]
fn a_seek_maps_the_panel_back_onto_the_absolute_timeline() {
    // Subject-local [0, dur] → absolute start + local.
    assert_eq!(
        seek_target(50.0, 200.0, 1000, 4000),
        Duration::from_millis(4250)
    );
    assert_eq!(
        seek_target(-20.0, 200.0, 1000, 4000),
        Duration::from_millis(4000)
    );
    assert_eq!(
        seek_target(900.0, 200.0, 1000, 4000),
        Duration::from_millis(5000)
    );
    // A zero-length subject seeks to its start.
    assert_eq!(
        seek_target(50.0, 200.0, 0, 4000),
        Duration::from_millis(4000)
    );
}

#[test]
fn the_playhead_maps_into_the_subject_only_inside_its_span() {
    let at = |ms| local_seconds(Duration::from_millis(ms), 1000, 4000);
    assert_eq!(at(4000), Some(0.0));
    assert_eq!(at(4250), Some(0.25));
    assert_eq!(at(5000), Some(1.0));
    assert_eq!(at(3999), None);
    assert_eq!(at(5001), None);
    // Whole milliseconds, as Dart's inMilliseconds.
    assert_eq!(
        local_seconds(Duration::from_micros(4_250_900), 1000, 4000),
        Some(0.25)
    );
}

#[test]
fn the_pitch_tab_hands_its_lane_the_subject_span() {
    let lane = track(3000);
    // A whole-lane subject passes the lane through.
    assert!(Arc::ptr_eq(&pitch_subject(&lane, 0, 3000), &lane));
    // One clip of a multi-clip lane: a single-clip view of the subject axis.
    let subject = pitch_subject(&lane, 1200, 800);
    assert_eq!(subject.start_ms(), 1200);
    assert_eq!(subject.duration_ms(), 800);
    assert_eq!(subject.clips.len(), 1);
    assert_eq!(subject.clips[0].id, "t1-analysis-subject");
    assert_eq!(subject.clips[0].audio_file_path, "/x.wav");
    assert_eq!(subject.id, lane.id);
}

#[test]
fn the_khz_labels_sit_on_their_frequencies() {
    let labels = freq_axis_labels(5000.0, 500.0);
    assert_eq!(labels.len(), 6);
    assert_eq!(labels[0], (0, 0.0, Alignment::Left));
    assert_eq!(labels[2], (2, 200.0, Alignment::Center));
    assert_eq!(labels[5], (5, 500.0, Alignment::Right));
    // Not on a kHz boundary: the last label is the last whole kHz.
    let labels = freq_axis_labels(4500.0, 450.0);
    assert_eq!(labels.last().copied(), Some((4, 400.0, Alignment::Right)));
    assert!(freq_axis_labels(f64::INFINITY, 450.0).is_empty());
}

#[test]
fn a_timbre_curve_breaks_at_gaps_and_a_flat_one_draws_nothing() {
    let mut m = VoiceMetrics::clone(&step_metrics());
    m.ring = (0..40)
        .map(|f| if f == 10 { f32::NAN } else { f as f32 })
        .collect();
    let size = Size::new(500.0, 100.0);
    let runs = timbre_curve(&m, &m.ring, 500, size);
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].len(), 10);
    assert_eq!(runs[1].len(), 29);
    // The extremes sit 7% in from the edges.
    let ys: Vec<f32> = runs.iter().flatten().map(|p| p.y).collect();
    assert!((ys[0] - 93.0).abs() < 1e-3);
    assert!((ys.last().unwrap() - 7.0).abs() < 1e-3);
    // Frames at their window centres on the time axis.
    assert!((runs[0][0].x as f64 - m.time_of_frame(0) * 1000.0).abs() < 1e-3);
    // The step's flat RING... here CPP: one value, no range.
    assert!(timbre_curve(&m, &m.cpp, 500, size).is_empty());
    assert!(timbre_curve(&m, &m.f3, 500, size).is_empty());
    assert!(timbre_curve(&m, &m.ring, 0, size).is_empty());
}

#[test]
fn the_register_marker_stays_on_its_bar() {
    assert_eq!(register_marker_left(f64::NAN, 280.0), None);
    assert_eq!(register_marker_left(0.0, 280.0), Some(0.0));
    assert_eq!(register_marker_left(0.5, 280.0), Some(138.5));
    assert_eq!(register_marker_left(1.0, 280.0), Some(277.0));
    assert_eq!(register_marker_left(7.0, 280.0), Some(277.0));
}

#[test]
fn the_vowel_chart_keeps_its_aspect() {
    assert_eq!(aspect_fit(Size::new(1300.0, 200.0), 1.3), (260.0, 200.0));
    assert_eq!(aspect_fit(Size::new(130.0, 200.0), 1.3), (130.0, 100.0));
}

#[test]
fn the_source_toggle_selects_the_take_or_the_live_mic() {
    for live in [false, true] {
        let mut ui = Simulator::new(source_toggle(live));
        ui.click("Live").expect("the Live segment");
        ui.click("Take").expect("the Take segment");
        let selected: Vec<bool> = ui
            .into_messages()
            .filter_map(|m| match m {
                Message::SelectLive(live) => Some(live),
                _ => None,
            })
            .collect();
        assert_eq!(selected, vec![true, false]);
    }
}

#[test]
fn the_take_image_follows_the_shown_spectrogram_and_goes_with_it() {
    let a = analyzed(&sine(4800, 220.0, SR as f64));
    let mut state = VoiceprintState::new();
    let request = state.sync_request(&a).expect("a first take renders");
    // The same take again: nothing to render.
    assert!(state.sync_request(&a).is_none());
    state.rendered(request.run());
    assert!(state.image.image().is_some());
    assert!(state.shows(a.spectrogram.as_ref().unwrap()));

    // Another take: the old image goes at once, the new one renders.
    let b = analyzed(&sine(4800, 330.0, SR as f64));
    let request = state.sync_request(&b).expect("a new take renders");
    assert!(state.image.image().is_none());
    assert!(!state.shows(a.spectrogram.as_ref().unwrap()));
    state.rendered(request.run());
    assert!(state.image.image().is_some());

    // Live, loading or nothing to show: the image is let go.
    for gone in [
        live(&[0.0; 512]),
        Source {
            loading: true,
            ..analyzed(&sine(4800, 330.0, SR as f64))
        },
        Source::default(),
    ] {
        let mut state = synced(&b);
        assert!(state.image.image().is_some());
        assert!(state.sync_request(&gone).is_none());
        assert!(state.image.image().is_none());
        assert!(state.shown.is_none());
    }
}

#[test]
fn another_takes_image_never_stands_in() {
    // A state still holding take A's image, asked to show take B before the
    // host synced: B's stack draws no image rather than A's.
    let a = analyzed(&sine(4800, 220.0, SR as f64));
    let b = analyzed(&sine(4800, 330.0, SR as f64));
    let state = synced(&a);
    assert!(!state.shows(b.spectrogram.as_ref().unwrap()));
    simulator(spectrogram, &b, &state, DOCK)
        .snapshot(&app_theme())
        .expect("draws");
}

#[test]
fn a_nan_bin_in_the_power_spectrum_reads_as_the_top_of_the_scale() {
    // Audio carrying a non-finite sample leaves NaN bins in its spectrogram.
    // Dart's clamp sends them to the top of the panel; a NaN coordinate
    // reached the path builder instead, and the Power tab panicked drawing.
    let column: Arc<[f32]> = vec![0.0, f32::NAN, 10.0, 5.0].into();
    let shape = PowerSpectrumPainter::new(column, 50.0, 5000.0)
        .shape(300.0, 100.0)
        .expect("a column");
    assert!(shape
        .area
        .iter()
        .chain(&shape.envelope)
        .all(|&(x, y)| x.is_finite() && y.is_finite()));
    assert_eq!(shape.area[2].1, 0.0);
}
