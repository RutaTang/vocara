//! The Range and Dynamics tabs' tests: their half of
//! `voiceprint_tabs_test.dart` — both views against the analysis the REAL
//! DSP produces from degenerate audio (no voiced frame, one frame, a DC
//! offset, non-finite samples), with nothing selected, while computing, over
//! a zero-length span and against a live monitor fed what a misbehaving
//! driver could deliver; and which half of the analysis each waits on. The
//! other eight tabs are `voiceprint_view`'s.
//!
//! A view "renders" when the headless renderer draws it: layout, every
//! canvas and every text, without a panic (Flutter's `takeException`).

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Duration;

use iced::{Settings, Size};
use iced_test::selector::Candidate;
use iced_test::Simulator;
use vocara_model::models::audio_track::AudioTrackKind;
use vocara_model::models::spectrogram::Spectrogram;
use vocara_model::models::voice_metrics::{
    analyze_voice_profile, Ltas, RegisterMap, VoiceProfileOptions,
};
use vocara_model::util::dart_date_time::DateTime;
use vocara_services::audio_dsp::{compute_spectrogram, compute_voice_metrics, AnalysisParams};

use super::*;
use crate::ui::core::theme::app_theme;
use crate::ui::song_practice_detail::widgets::live_spectrogram::tests::{fed, sine};

const SR: i64 = 48000;

/// The test's [`AnalysisSource`] (`_Source`).
#[derive(Default)]
struct Source {
    track: Option<Arc<AudioTrack>>,
    metrics: Option<Arc<VoiceMetrics>>,
    spectrogram: Option<Arc<Spectrogram>>,
    profile: Option<Arc<VoiceProfile>>,
    loading: bool,
    /// The metrics half's flag; `loading` when unset.
    metrics_loading: Option<bool>,
    duration_ms: i64,
    /// Set for a live source, which also supplies the monitor.
    monitor: Option<Arc<MonitorService>>,
    position: Duration,
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
        None
    }
    fn selected_register(&self) -> Option<Arc<RegisterMap>> {
        None
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
        0
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
    let ms = round_to_i64(samples.len() as f64 / SR as f64 * 1000.0);
    Source {
        track: Some(track(ms)),
        profile: Some(Arc::new(analyze_voice_profile(
            &metrics,
            VoiceProfileOptions::default(),
        ))),
        spectrogram: Some(Arc::new(compute_spectrogram(samples, SR, params))),
        metrics: Some(metrics),
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

type View = for<'a> fn(&'a dyn AnalysisSource) -> Element<'a, ()>;

fn range<'a>(src: &'a dyn AnalysisSource) -> Element<'a, ()> {
    range_view(src)
}

fn dynamics<'a>(src: &'a dyn AnalysisSource) -> Element<'a, ()> {
    dynamics_view(src)
}

const VIEWS: [(&str, View); 2] = [("Range", range), ("Dynamics", dynamics)];

/// The dock: a wide, short strip.
const DOCK: Size = Size {
    width: 1440.0,
    height: 260.0,
};

fn simulator<'a>(view: View, src: &'a dyn AnalysisSource) -> Simulator<'a, ()> {
    Simulator::with_size(Settings::default(), DOCK, view(src))
}

/// Both views, one at a time: a view that panics must not be masked by the
/// other, and the failure names which one.
fn pump_views(src: &Source) {
    for (name, view) in VIEWS {
        let drawn = catch_unwind(AssertUnwindSafe(|| {
            simulator(view, src)
                .snapshot(&app_theme())
                .expect("snapshot");
        }));
        assert!(drawn.is_ok(), "the {name} view panicked");
    }
}

/// Every text on screen.
fn texts(ui: &mut Simulator<'_, ()>) -> Vec<String> {
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

#[test]
fn both_views_render_a_normal_take() {
    let src = analyzed(&sine(48000, 220.0, SR as f64));
    pump_views(&src);
    let mut ui = simulator(range, &src);
    let shown = texts(&mut ui);
    for label in ["Range", "Tessitura", "A3 – A3"] {
        assert!(shown.iter().any(|t| t == label), "{label} in {shown:?}");
    }
    let mut ui = simulator(dynamics, &src);
    let shown = texts(&mut ui);
    for label in [
        "Dynamic range",
        "Softest",
        "Loudest",
        "Loudness over time",
        "Span",
    ] {
        assert!(shown.iter().any(|t| t == label), "{label} in {shown:?}");
    }
}

#[test]
fn both_views_render_a_silent_take() {
    // The muted-input case: metrics exist, but nothing is voiced or audible.
    let src = analyzed(&[0.0; 48000]);
    assert!(src.metrics.as_ref().unwrap().frames > 0);
    pump_views(&src);
    assert!(simulator(range, &src).find(RANGE_EMPTY_HINT).is_ok());
}

#[test]
fn both_views_render_a_take_stopped_after_one_buffer() {
    pump_views(&analyzed(&sine(256, 220.0, SR as f64)));
}

#[test]
fn both_views_render_a_single_sample_take() {
    pump_views(&analyzed(&sine(1, 220.0, SR as f64)));
}

#[test]
fn both_views_render_a_dc_take() {
    pump_views(&analyzed(&[1.0; 48000]));
}

#[test]
fn both_views_render_audio_carrying_non_finite_samples() {
    let mut data = sine(48000, 220.0, SR as f64);
    data[100] = f32::NAN;
    data[200] = f32::INFINITY;
    data[300] = f32::NEG_INFINITY;
    pump_views(&analyzed(&data));
}

#[test]
fn both_views_render_with_nothing_selected() {
    let src = Source::default();
    pump_views(&src);
    for (name, view) in VIEWS {
        assert!(
            simulator(view, &src).find(SELECT_TAKE_HINT).is_ok(),
            "{name}"
        );
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
    for (name, view) in VIEWS {
        assert!(
            simulator(view, &src).find(SELECT_TAKE_HINT).is_ok(),
            "{name}"
        );
    }
}

#[test]
fn both_views_render_while_the_analysis_is_still_computing() {
    let src = Source {
        track: Some(track(1000)),
        loading: true,
        duration_ms: 1000,
        ..Source::default()
    };
    pump_views(&src);
    for (name, view) in VIEWS {
        assert!(simulator(view, &src).find(ANALYZING_HINT).is_ok(), "{name}");
    }
}

#[test]
fn each_view_waits_only_on_the_metrics() {
    // The halves compute apart: the profile views read the metrics only, so
    // a spectrogram still computing must not hide them behind a spinner.
    let whole = analyzed(&sine(48000, 220.0, SR as f64));
    let spins = |view: View, src: &Source| {
        let mut ui = simulator(view, src);
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
        loading: true,
        metrics_loading: Some(false),
        duration_ms: whole.duration_ms,
        ..Source::default()
    };
    for (name, view) in VIEWS {
        assert!(
            spins(view, &metrics_computing),
            "{name}, spectrogram held, metrics computing"
        );
        assert!(
            !spins(view, &spectrogram_computing),
            "{name}, metrics held, spectrogram computing"
        );
    }
}

#[test]
fn a_subject_with_a_zero_length_span_does_not_divide_by_it() {
    let base = analyzed(&sine(4800, 220.0, SR as f64));
    pump_views(&Source {
        track: Some(track(0)),
        duration_ms: 0,
        position: Duration::from_millis(250),
        ..base
    });
}

#[test]
fn the_playhead_far_outside_the_take_renders() {
    pump_views(&Source {
        position: Duration::from_secs(86_400),
        ..analyzed(&sine(48000, 220.0, SR as f64))
    });
}

// ------------------------------------------------- the LIVE monitor path

#[test]
fn the_range_view_says_live_has_no_range() {
    let src = live(&sine(24000, 220.0, SR as f64));
    assert!(simulator(range, &src).find(RANGE_LIVE_HINT).is_ok());
}

#[test]
fn both_views_render_a_fed_monitor() {
    let src = live(&sine(24000, 220.0, SR as f64));
    pump_views(&src);
    let mut ui = simulator(dynamics, &src);
    assert!(ui.find("Loudness (live)").is_ok());
    assert!(ui.find(LIVE_DYNAMICS_HINT).is_err());
}

#[test]
fn both_views_render_a_monitor_fed_silence() {
    let src = live(&[0.0; 24000]);
    pump_views(&src);
    assert!(simulator(dynamics, &src).find(LIVE_DYNAMICS_HINT).is_ok());
}

#[test]
fn both_views_render_a_monitor_fed_non_finite_samples() {
    let mut data = sine(24000, 220.0, SR as f64);
    data[100] = f32::NAN;
    data[200] = f32::INFINITY;
    data[300] = f32::NEG_INFINITY;
    pump_views(&live(&data));
}

#[test]
fn a_nan_inside_a_sustained_note_renders() {
    let mut data = sine(48000, 220.0, SR as f64);
    data[30000] = f32::NAN;
    pump_views(&live(&data));
}

#[test]
fn both_views_render_a_monitor_started_but_never_fed() {
    let src = live(&[]);
    pump_views(&src);
    // Waiting as the other live tabs do: titled, marked Live, the word in
    // its well (a bare hint, untitled, read as another state).
    let shown = texts(&mut simulator(dynamics, &src));
    for label in [LIVE_DYNAMICS_HINT, "Loudness (live)", "Live"] {
        assert!(shown.iter().any(|t| t == label), "{label} in {shown:?}");
    }
}

// ----------------------------------------------------------- the pieces

fn profile(low: Option<i64>, high: Option<i64>) -> VoiceProfile {
    VoiceProfile {
        low_midi: low,
        high_midi: high,
        tess_lo_midi: None,
        tess_hi_midi: None,
        passaggio_midi: None,
        passaggio_jump_db: 0.0,
        soft_db: f64::NAN,
        loud_db: f64::NAN,
        median_db: f64::NAN,
    }
}

#[test]
fn a_range_reads_its_width_in_semitones_singular_at_one() {
    assert_eq!(semitones_label(19), "19 semitones");
    assert_eq!(semitones_label(1), "1 semitone");
    assert_eq!(semitones_label(0), "0 semitones");
}

#[test]
fn the_range_bar_spans_whole_octaves_around_the_range() {
    // A3 (57) .. E5 (76): two semitones each side reach 55 and 78, so the
    // axis runs C3 (48) .. C6 (84), 36 semitones over 360 px.
    let mut p = profile(Some(57), Some(76));
    p.tess_lo_midi = Some(60);
    p.tess_hi_midi = Some(70);
    p.passaggio_midi = Some(64);
    let bar = range_bar(&p, 360.0, 100.0).expect("a range");
    let whole = |(a, b): (f64, f64)| (a.round(), b.round());
    assert_eq!(bar.bar_y, 50.0);
    assert_eq!(whole(bar.range), (90.0, 280.0));
    assert_eq!(bar.tessitura.map(whole), Some((120.0, 220.0)));
    assert_eq!(bar.passaggio.map(f64::round), Some(160.0));
    assert_eq!(
        bar.grid,
        vec![
            (0.0, Some(48)),
            (120.0, Some(60)),
            (240.0, Some(72)),
            (360.0, Some(84))
        ]
    );
    // No range, no bar.
    assert_eq!(range_bar(&profile(None, Some(60)), 230.0, 100.0), None);
}

#[test]
fn a_narrow_range_has_an_axis_and_names_each_c_once() {
    // G4 (67) .. C5 (72): the axis was 65..74, one gridline, its C5 named
    // under the bar's own C5. Now C4 .. C6, and the C5 the bar's end names
    // goes unnamed on the axis.
    let bar = range_bar(&profile(Some(67), Some(72)), 240.0, 100.0).expect("a range");
    assert_eq!(
        bar.grid,
        vec![(0.0, Some(60)), (120.0, None), (240.0, Some(84))]
    );
    assert_eq!(bar.range, (70.0, 120.0));
}

#[test]
fn the_range_bar_never_leaves_the_panel() {
    // One note: an octave each side of it.
    let mut p = profile(Some(60), Some(60));
    p.tess_lo_midi = Some(40);
    p.tess_hi_midi = Some(90);
    let bar = range_bar(&p, 100.0, 40.0).expect("a range");
    assert_eq!(bar.range, (50.0, 50.0));
    assert_eq!(bar.tessitura, Some((0.0, 100.0)));
    // Below MIDI 0 the octaves still fall on Cs.
    let bar = range_bar(&profile(Some(-5), Some(3)), 120.0, 40.0).expect("a range");
    assert_eq!(
        bar.grid,
        vec![(0.0, Some(-12)), (60.0, Some(0)), (120.0, Some(12))]
    );
}

#[test]
fn the_span_bar_places_dbfs_on_a_sixty_db_track() {
    assert_eq!(dyn_bar_x(-60.0, 300.0), 0.0);
    assert_eq!(dyn_bar_x(-30.0, 300.0), 150.0);
    assert_eq!(dyn_bar_x(0.0, 300.0), 300.0);
    assert_eq!(dyn_bar_x(-90.0, 300.0), 0.0);
    assert_eq!(dyn_bar_x(6.0, 300.0), 300.0);
}

#[test]
fn the_live_contour_breaks_at_non_finite_readings() {
    assert!(live_loudness_runs(&[], 100.0, 60.0).is_empty());
    assert!(live_loudness_runs(&[-20.0], 100.0, 60.0).is_empty());
    let runs = live_loudness_runs(
        &[-60.0, 0.0, f64::NAN, -30.0, f64::NEG_INFINITY, -90.0, 10.0],
        60.0,
        60.0,
    );
    assert_eq!(
        runs,
        vec![
            vec![(0.0, 60.0), (10.0, 0.0)],
            vec![(30.0, 30.0)],
            vec![(50.0, 60.0), (60.0, 0.0)],
        ]
    );
}

#[test]
fn a_break_of_six_db_or_more_reads_as_abrupt() {
    assert_eq!(passaggio_shift(6.0), "abrupt shift");
    assert_eq!(passaggio_shift(5.9), "gradual shift");
    assert_eq!(passaggio_shift(0.0), "gradual shift");
}

/// The dock's tab at its least height (160, less its handle and tabs).
const SHORT_DOCK: Size = Size {
    width: 1100.0,
    height: 121.0,
};

/// Whether `label` shows in `ui`'s viewport.
fn visible(ui: &mut Simulator<'_, ()>, label: &str) -> bool {
    ui.find(label)
        .ok()
        .and_then(|t| t.visible_bounds())
        .is_some_and(|v| v.height > 1.0)
}

/// A wheel turn down over the middle of `ui`.
fn scroll_down(ui: &mut Simulator<'_, ()>) {
    ui.point_at(iced::Point::new(500.0, 60.0));
    let _ = ui.simulate([iced::Event::Mouse(iced::mouse::Event::WheelScrolled {
        delta: iced::mouse::ScrollDelta::Pixels { x: 0.0, y: -400.0 },
    })]);
}

#[test]
fn on_the_shortest_dock_range_and_dynamics_scroll_rather_than_crush() {
    // At the dock's least height, Range clipped its chart and Dynamics
    // crushed its loudness curve to a line; neither scrolled, as Summary
    // does.
    let src = analyzed(&sine(48000, 220.0, SR as f64));
    for (view, header) in [
        (range as View, "Sung range on a pitch axis"),
        (dynamics as View, "Loudness over time"),
    ] {
        let mut ui = Simulator::with_size(Settings::default(), SHORT_DOCK, view(&src));
        assert!(visible(&mut ui, header));
        scroll_down(&mut ui);
        assert!(!visible(&mut ui, header), "{header}: it scrolls");
    }
    // Dynamics keeps its whole layout: the span's scale lies below the
    // viewport, scrolled into it.
    let mut ui = Simulator::with_size(Settings::default(), SHORT_DOCK, dynamics(&src));
    let end = ui.find("0 dBFS").expect("the span's end").bounds();
    assert!(end.y > SHORT_DOCK.height, "{end:?}");
    scroll_down(&mut ui);
    assert!(visible(&mut ui, "0 dBFS"));
}

#[test]
fn the_range_view_marks_the_passaggio() {
    let mut p = profile(Some(57), Some(76));
    p.tess_lo_midi = Some(60);
    p.tess_hi_midi = Some(70);
    p.passaggio_midi = Some(64);
    p.passaggio_jump_db = 7.5;
    let src = Source {
        profile: Some(Arc::new(p)),
        ..analyzed(&sine(4800, 220.0, SR as f64))
    };
    let mut ui = simulator(range, &src);
    let shown = texts(&mut ui);
    for label in [
        "A3 – E5",
        "19 semitones",
        "C4 – A♯4",
        "comfortable zone",
        "Passaggio",
        "E4",
        "abrupt shift",
        "Sung range on a pitch axis",
    ] {
        assert!(shown.iter().any(|t| t == label), "{label} in {shown:?}");
    }
    ui.snapshot(&app_theme()).expect("draws");
}
