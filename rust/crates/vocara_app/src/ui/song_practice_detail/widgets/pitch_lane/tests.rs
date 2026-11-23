//! The pitch lane's tests: `pitch_memo_test.dart`, the pitch parts of
//! `voice_metrics_test.dart`, `lane_painter_window_test.dart`,
//! `analysis_frame_timing_test.dart` and `analysis_lanes_test.dart`.
//!
//! Where a Dart test analysed real audio (`computeVoiceMetrics`, not ported
//! yet), the metrics are built by hand with the F0 the analysis gives such
//! audio, on the production framing. Raster comparisons render through
//! `iced_test`'s headless renderer.

use std::f64::consts::PI;
use std::io::BufReader;
use std::path::Path as FsPath;

use iced::widget::{canvas as canvas_widget, row, Space};
use iced::Settings;
use vocara_model::models::audio_track::AudioTrackKind;
use vocara_model::util::dart_date_time::DateTime;
use vocara_model::util::dart_num::pow_f64;

use super::*;
use crate::ui::core::theme::app_theme;

const SR: i64 = 48000;
const HOP: i64 = 512;
const FFT: i64 = 2048;

/// Hand-built metrics on a framing: `f0(i)` per frame (NaN = unvoiced), the
/// other curves flat.
fn metrics_with(frames: usize, hop: i64, f0: impl Fn(usize) -> f64) -> VoiceMetrics {
    let f0: Vec<f32> = (0..frames).map(|i| f0(i) as f32).collect();
    let nan = vec![f32::NAN; frames];
    VoiceMetrics {
        frames,
        hop,
        fft_size: FFT,
        sample_rate: SR,
        ring: vec![0.0; frames],
        tilt: vec![0.0; frames],
        cpp: vec![0.0; frames],
        loudness: vec![-20.0; frames],
        active: f0.iter().map(|hz| u8::from(hz.is_finite())).collect(),
        f0,
        f1: nan.clone(),
        f2: nan.clone(),
        f3: nan.clone(),
        h1h2: nan,
        ring_avg: 0.0,
        tilt_avg: 0.0,
        cpp_avg: 0.0,
        peak_dbfs: f64::NAN,
        clipped_fraction: 0.0,
        snr_db: f64::NAN,
    }
}

fn track(start_ms: i64, duration_ms: i64) -> AudioTrack {
    AudioTrack::new(
        "t1",
        AudioTrackKind::Vocal,
        "Take 1",
        DateTime::utc_ymd(2026, 1, 1),
    )
    .with_audio("/x.wav", start_ms, duration_ms, Vec::new())
}

// ------------------------------------------------------------ formatting

#[test]
fn notes_and_cents_read_as_the_dart_labels() {
    assert_eq!(semi_name(57), "A3");
    assert_eq!(semi_name(60), "C4");
    assert_eq!(semi_name(61), "C♯4");
    // Dart's `%` is never negative and its `~/` truncates.
    assert_eq!(semi_name(-1), "B-1");
    assert_eq!(semi_name(-13), "B-2");
    assert_eq!(cents_str(12.4), "+12¢");
    assert_eq!(cents_str(-7.5), "−8¢");
    assert_eq!(cents_str(-0.4), "+0¢");
    assert_eq!(cents_str(0.0), "+0¢");
}

#[test]
fn note_colours_ramp_from_in_tune_to_record_at_fifty_cents() {
    assert_eq!(cents_color(0.0), Studio::IN_TUNE);
    assert_eq!(cents_color(50.0), Studio::RECORD);
    assert_eq!(cents_color(500.0), Studio::RECORD);
    let mid = cents_color(25.0);
    assert!((mid.r - (Studio::IN_TUNE.r + Studio::RECORD.r) / 2.0).abs() < 1e-6);
}

// -------------------------------------------------- voice_metrics_test

/// A steady sung pitch: `hz` with a ±`wobble_cents` 5.5 Hz wobble, voiced
/// from 0.1 s to 0.9 s of a one-second take (as the analysis reads a
/// synthetic vowel).
fn steady(hz: f64, wobble_cents: f64) -> VoiceMetrics {
    let fps = SR as f64 / HOP as f64;
    metrics_with(90, HOP, |i| {
        if !(9..=84).contains(&i) {
            return f64::NAN;
        }
        let cents = wobble_cents * (2.0 * PI * 5.5 * i as f64 / fps).sin();
        hz * pow_f64(2.0, cents / 1200.0)
    })
}

fn longest(notes: &[PitchNote]) -> PitchNote {
    *notes
        .iter()
        .reduce(|a, b| if a.frames() >= b.frames() { a } else { b })
        .expect("notes")
}

#[test]
fn segment_pitch_notes_groups_a_steady_vowel_into_one_note_near_its_pitch() {
    let m = steady(196.0, 6.0); // G3
    let notes = segment_pitch_notes(&m, MIN_NOTE_FRAMES, MAX_NOTE_GAP);
    assert!(!notes.is_empty());
    let main = longest(&notes);
    assert_eq!(main.semitone, 55, "G3 = MIDI 55");
    assert!(main.median_cents.abs() < 20.0);
    assert!(pitch_in_tune_percent(&notes) > 80.0);
}

#[test]
fn a_fifty_cent_off_note_near_a_semitone_boundary_survives_as_one_off_note() {
    // 201.7 Hz ≈ 50 cents above G3 — without hysteresis the wobble would
    // flip-flop round() and fragment it into dropped sub-min runs, vanishing
    // entirely.
    let m = steady(201.7, 8.0);
    let notes = segment_pitch_notes(&m, MIN_NOTE_FRAMES, MAX_NOTE_GAP);
    assert!(!notes.is_empty());
    let main = longest(&notes);
    assert!(
        main.median_cents.abs() > 35.0,
        "should stay flagged off-pitch, not disappear"
    );
    assert_eq!(main.frames(), 76, "one note over every voiced frame");
}

#[test]
fn pitch_semitone_range_brackets_the_f0_with_at_least_an_octave_span() {
    let m = steady(196.0, 6.0); // ≈ G3 ≈ MIDI 55
    let r = pitch_semitone_range(&m);
    assert!(r.lo < 55);
    assert!(r.hi > 55);
    assert!(r.hi - r.lo >= 12);
    // Nothing voiced: C3..C5.
    let silent = metrics_with(40, HOP, |_| f64::NAN);
    assert_eq!(pitch_semitone_range(&silent), DEFAULT_SEMITONE_RANGE);
    // A wide range keeps its ± 2.
    let wide = metrics_with(40, HOP, |i| if i < 20 { 110.0 } else { 880.0 });
    assert_eq!(
        pitch_semitone_range(&wide),
        SemitoneRange { lo: 43, hi: 83 }
    );
}

#[test]
fn a_dropout_up_to_the_gap_is_bridged_and_a_longer_one_splits_the_note() {
    let with_gap = |gap: usize| {
        metrics_with(40, HOP, move |i| {
            if (10..10 + gap).contains(&i) {
                f64::NAN
            } else {
                220.0
            }
        })
    };
    let bridged = segment_pitch_notes(&with_gap(3), MIN_NOTE_FRAMES, MAX_NOTE_GAP);
    assert_eq!(bridged.len(), 1);
    assert_eq!((bridged[0].start_frame, bridged[0].end_frame), (0, 39));
    let split = segment_pitch_notes(&with_gap(4), MIN_NOTE_FRAMES, MAX_NOTE_GAP);
    assert_eq!(split.len(), 2);
    assert_eq!((split[0].start_frame, split[0].end_frame), (0, 9));
    // A run shorter than the minimum is dropped.
    let short = metrics_with(20, HOP, |i| if i < 6 { 220.0 } else { f64::NAN });
    assert!(segment_pitch_notes(&short, MIN_NOTE_FRAMES, MAX_NOTE_GAP).is_empty());
}

#[test]
fn an_infinite_or_negative_f0_reads_as_unvoiced() {
    let m = metrics_with(30, HOP, |i| match i {
        3 => f64::INFINITY,
        4 => f64::NEG_INFINITY,
        5 => -100.0,
        _ => 220.0,
    });
    let notes = segment_pitch_notes(&m, MIN_NOTE_FRAMES, MAX_NOTE_GAP);
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].semitone, 57);
    assert_eq!(pitch_semitone_range(&m), SemitoneRange { lo: 51, hi: 63 });
}

// ------------------------------------------------------- pitch_memo_test

/// 1.6 s notes with a ±25¢ 5.5 Hz vibrato, every fourth sung `detune_cents`
/// sharp.
fn memo_metrics(frames: usize, detune_cents: f64) -> Arc<VoiceMetrics> {
    let fps = SR as f64 / HOP as f64;
    Arc::new(metrics_with(frames, HOP, |i| {
        if i % 160 >= 150 {
            return f64::NAN;
        }
        let note = i / 160;
        let cents = (if note % 4 == 0 { detune_cents } else { 0.0 })
            + 25.0 * (2.0 * PI * 5.5 * i as f64 / fps).sin();
        220.0 * pow_f64(2.0, (note % 7) as f64 / 12.0) * pow_f64(2.0, cents / 1200.0)
    }))
}

#[test]
fn the_memoized_notes_are_the_fresh_segmentation_computed_once() {
    let m = memo_metrics(4000, 40.0);
    let notes = pitch_notes_of(&m);
    assert!(!notes.is_empty());
    assert_eq!(
        &notes[..],
        &segment_pitch_notes(&m, MIN_NOTE_FRAMES, MAX_NOTE_GAP)[..]
    );
    assert!(Arc::ptr_eq(&pitch_notes_of(&m), &notes));
}

#[test]
fn the_memoized_range_is_the_fresh_range() {
    let m = memo_metrics(4000, 40.0);
    assert_eq!(pitch_range_of(&m), pitch_semitone_range(&m));
    assert_eq!(pitch_range_of(&m), pitch_range_of(&m));
}

#[test]
fn the_memoized_flagged_notes_are_the_up_to_3_most_off_pitch_ones() {
    let m = memo_metrics(4000, 40.0);
    let notes = pitch_notes_of(&m);
    let worst = worst_notes_of(&notes);
    // The same rule, computed fresh: by |cents| descending, beyond 30¢, top 3.
    let mut by_cents: Vec<usize> = (0..notes.len()).collect();
    by_cents.sort_by(|&a, &b| {
        notes[b]
            .median_cents
            .abs()
            .total_cmp(&notes[a].median_cents.abs())
    });
    let want: BTreeSet<usize> = by_cents
        .into_iter()
        .filter(|&i| notes[i].median_cents.abs() > 30.0)
        .take(3)
        .collect();
    assert!(!want.is_empty(), "the detuned notes should be flagged");
    assert_eq!(*worst, want);
    assert!(Arc::ptr_eq(&worst_notes_of(&notes), &worst));
}

fn same(x: f64, y: f64) -> bool {
    x == y || (x.is_nan() && y.is_nan())
}

#[test]
fn the_memoized_vibrato_of_a_note_is_the_fresh_analysis_computed_once() {
    let m = memo_metrics(4000, 40.0);
    let notes = pitch_notes_of(&m);
    let mut present = 0;
    for n in notes.iter() {
        let v = note_vibrato_of(&m, n);
        let fresh = analyze_vibrato(
            &m,
            n.start_frame as i64,
            n.end_frame as i64,
            VibratoOptions::default(),
        );
        assert_eq!(v.present, fresh.present);
        assert!(same(v.rate_hz, fresh.rate_hz));
        assert!(same(v.extent_cents, fresh.extent_cents));
        assert!(same(v.regularity, fresh.regularity));
        assert!(same(v.jitter, fresh.jitter));
        assert!(same(v.onset_ms, fresh.onset_ms));
        assert_eq!(
            v.deviation_cents
                .iter()
                .map(|c| c.to_bits())
                .collect::<Vec<_>>(),
            fresh
                .deviation_cents
                .iter()
                .map(|c| c.to_bits())
                .collect::<Vec<_>>()
        );
        assert!(Arc::ptr_eq(&note_vibrato_of(&m, n), &v));
        if v.present {
            present += 1;
        }
    }
    assert!(present > 0, "the notes carry a vibrato");
}

#[test]
fn memos_never_leak_between_takes() {
    let a = memo_metrics(4000, 0.0);
    let b = memo_metrics(3000, 45.0);
    let notes_a = pitch_notes_of(&a);
    let notes_b = pitch_notes_of(&b);
    assert!(!Arc::ptr_eq(&notes_a, &notes_b));
    assert_eq!(
        &notes_a[..],
        &segment_pitch_notes(&a, MIN_NOTE_FRAMES, MAX_NOTE_GAP)[..]
    );
    assert_eq!(
        &notes_b[..],
        &segment_pitch_notes(&b, MIN_NOTE_FRAMES, MAX_NOTE_GAP)[..]
    );
    assert!(worst_notes_of(&notes_a).is_empty());
    assert!(!worst_notes_of(&notes_b).is_empty());
    assert_eq!(pitch_range_of(&b), pitch_semitone_range(&b));
}

#[test]
fn a_memo_entry_goes_with_its_take() {
    let gone = memo_metrics(500, 40.0);
    let _ = pitch_notes_of(&gone);
    let weak = Arc::downgrade(&gone);
    drop(gone);
    // The next lookup sweeps the dead entry (the weak key here keeps its
    // address from being reused meanwhile).
    let _ = pitch_notes_of(&memo_metrics(200, 0.0));
    let table = NOTES_MEMO.lock().unwrap_or_else(|e| e.into_inner());
    assert!(!table
        .iter()
        .any(|(k, _)| std::ptr::addr_eq(k.as_ptr(), weak.as_ptr())));
}

// ------------------------------------------------- lane_painter_window_test

/// `frames` of synthetic analysis: a note every ~0.5 s with vibrato, mostly
/// voiced.
fn window_metrics(frames: usize) -> Arc<VoiceMetrics> {
    Arc::new(metrics_with(frames, HOP, |i| {
        if i % 47 < 33 {
            220.0 * pow_f64(2.0, ((i / 47) % 12) as f64 / 12.0) * (1.0 + 0.01 * (i as f64).sin())
        } else {
            f64::NAN
        }
    }))
}

/// The lane as `_WholeTakePitch` painted it before windowing: every note and
/// every contour frame of the take, as a frozen reference.
fn whole_take_plan(p: &PitchLanePainter, size: Size) -> PitchLanePlan {
    let Some(a) = p.axes(size) else {
        return PitchLanePlan::default();
    };
    let block_h = a.height / a.span * 0.62;
    let mut notes = Vec::new();
    for (i, n) in p.notes.iter().enumerate() {
        let x0 = a.x_of_frame(n.start_frame);
        let x1 = a.x_of_frame(n.end_frame).max(x0 + 2.0);
        let yc = a.y_of(n.semitone as f64);
        let col = cents_color(n.median_cents.abs());
        let is_worst = p.worst.contains(&i);
        notes.push(PitchOp::Block {
            index: i,
            left: x0,
            top: yc - block_h / 2.0,
            right: x1,
            bottom: yc + block_h / 2.0,
            fill: alpha(col, 0.26),
            stroke: if is_worst {
                Studio::RECORD
            } else {
                alpha(col, 0.7)
            },
            stroke_width: if is_worst { 1.6 } else { 1.0 },
        });
        if is_worst {
            let mut ly = yc - block_h / 2.0 - 11.0;
            if ly < 1.0 {
                ly = yc + block_h / 2.0 + 2.0;
            }
            notes.push(PitchOp::Label {
                text: cents_str(n.median_cents),
                x: x0 + 2.0,
                y: ly + 5.0,
                color: Studio::RECORD,
            });
        }
    }
    let mut contour = Vec::new();
    for n in p.notes.iter() {
        let mut prev = None;
        for fr in n.start_frame..=n.end_frame {
            let Some(hz) = voiced_hz(p.metrics.f0[fr]) else {
                prev = None;
                continue;
            };
            let q = (a.x_of_frame(fr), a.y_of(hz_to_midi(hz)));
            if let Some(from) = prev {
                contour.push(PitchOp::Contour { from, to: q });
            }
            prev = Some(q);
        }
    }
    PitchLanePlan {
        grid: p.grid_ops(size),
        notes,
        contour,
    }
}

/// Draws a fixed list of ops, as one of the painter's layers does.
struct OpsCanvas(Vec<PitchOp>);

impl canvas::Program<()> for OpsCanvas {
    type State = ();

    fn draw(
        &self,
        _: &(),
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        draw_ops(&mut frame, bounds.size(), &self.0);
        vec![frame.into_geometry()]
    }
}

fn plan_view(plan: PitchLanePlan) -> Element<'static, ()> {
    let layer = |ops| {
        Element::from(
            canvas_widget(OpsCanvas(ops))
                .width(Length::Fill)
                .height(Length::Fill),
        )
    };
    stack![layer(plan.grid), layer(plan.notes), layer(plan.contour)].into()
}

/// A rendered frame: its physical size and RGBA bytes.
struct Raster {
    width: usize,
    rgba: Vec<u8>,
}

/// Renders `element` in a `size` window through `iced_test`'s headless
/// renderer and reads the pixels back.
fn raster(element: Element<'_, ()>, size: Size) -> Raster {
    let dir = tempfile::tempdir().expect("temp dir");
    let mut ui = iced_test::Simulator::with_size(Settings::default(), size, element);
    let snapshot = ui.snapshot(&app_theme()).expect("snapshot");
    // A first match writes the image (named after the renderer).
    assert!(snapshot
        .matches_image(dir.path().join("frame.png"))
        .expect("write"));
    let file = std::fs::read_dir(dir.path())
        .expect("list")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "png"))
        .expect("png written");
    decode(&file)
}

fn decode(path: &FsPath) -> Raster {
    let decoder = png::Decoder::new(BufReader::new(std::fs::File::open(path).expect("open")));
    let mut reader = decoder.read_info().expect("png header");
    let mut rgba = vec![0; reader.output_buffer_size().expect("size")];
    let info = reader.next_frame(&mut rgba).expect("png frame");
    rgba.truncate(info.buffer_size());
    Raster {
        width: info.width as usize,
        rgba,
    }
}

const LANE: Size = Size {
    width: 300.0,
    height: 96.0,
};

/// Windows across the take: the whole of it, deep zoom (a 1-second window),
/// deeper still (a quarter second, whose 8 px margin is narrower than the
/// half window each frame is drawn past its start, so the frame window must
/// bracket frames by their CENTRES), either end, a window cutting a note, a
/// take placed later on the timeline, and a window past the take's end:
/// `(dur_ms, view_start_ms, start_ms)`.
const WINDOWS: [(i64, i64, i64); 8] = [
    (70000, 0, 0),
    (1000, 30017, 0),
    (250, 30025, 0),
    (5000, 0, 0),
    (4000, 57000, 0),
    (2300, 9411, 0),
    (6000, 41000, 12000),
    (5000, 64000, 0),
];

/// One minute at 48 kHz / 512, its notes, and every fifth note flagged (the
/// flagged notes carry a cents label right of their start). A 5-second
/// window in the middle of the minute by default.
fn window_painter(dur_ms: i64, view_start_ms: i64, start_ms: i64) -> PitchLanePainter {
    let m = window_metrics(5625);
    let notes: Arc<[PitchNote]> = segment_pitch_notes(&m, MIN_NOTE_FRAMES, MAX_NOTE_GAP).into();
    let range = pitch_semitone_range(&m);
    let worst = (0..notes.len()).step_by(5).collect();
    PitchLanePainter {
        metrics: m,
        notes,
        worst: Arc::new(worst),
        timeline_duration_ms: dur_ms,
        start_ms,
        view_start_ms,
        lo_semi: range.lo,
        hi_semi: range.hi,
    }
}

#[test]
fn the_windowed_pitch_lane_keeps_exactly_the_whole_take_ops_that_reach_the_box() {
    for (dur, view, start) in WINDOWS {
        let p = window_painter(dur, view, start);
        let got = p.plan(LANE);
        let want = whole_take_plan(&p, LANE);
        // Every op drawn is one the whole-take walk drew, in the same order.
        for (got, want) in [(&got.notes, &want.notes), (&got.contour, &want.contour)] {
            let mut rest = want.iter();
            for op in got {
                assert!(
                    rest.any(|w| w == op),
                    "{op:?} not in the whole-take walk ({dur}, {view}, {start})"
                );
            }
        }
        assert!(got.contour.len() <= want.contour.len());
    }
}

#[test]
fn the_windowed_pitch_lane_draws_exactly_the_whole_take_pixels() {
    for (dur, view, start) in WINDOWS {
        let p = window_painter(dur, view, start);
        let want = raster(plan_view(whole_take_plan(&p, LANE)), LANE);
        let got = raster(p.view(), LANE);
        let differing = got
            .rgba
            .iter()
            .zip(&want.rgba)
            .filter(|(a, b)| a != b)
            .count();
        assert_eq!(got.rgba.len(), want.rgba.len());
        assert_eq!(differing, 0, "({dur}, {view}, {start})");
    }
}

#[test]
fn a_zoomed_pitch_lane_paints_nothing_outside_its_box() {
    let inset = 100.0;
    let p = window_painter(5000, 25000, 0);
    let view: Element<'static, ()> = row![
        Space::new().width(inset),
        container(p.view()).width(LANE.width).height(LANE.height),
        Space::new().width(inset),
    ]
    .into();
    let r = raster(view, Size::new(LANE.width + 2.0 * inset, LANE.height));
    let background = Studio::BACKGROUND.into_rgba8();
    // The snapshot is at twice the logical size.
    let scale = r.width as f32 / (LANE.width + 2.0 * inset);
    let (lo, hi) = (
        (inset * scale) as usize,
        ((inset + LANE.width) * scale) as usize,
    );
    let (mut inside, mut outside) = (0, 0);
    for (i, px) in r.rgba.as_chunks::<4>().0.iter().enumerate() {
        if *px == background {
            continue;
        }
        let x = i % r.width;
        if x < lo || x >= hi {
            outside += 1;
        } else {
            inside += 1;
        }
    }
    assert!(inside > 0, "the window shows the take");
    assert_eq!(outside, 0, "the rest of the take stays off screen");
}

#[test]
fn lane_visible_frames_brackets_the_viewport_by_frame_centres() {
    let m = window_metrics(5625);
    // No frames, or no axis.
    let none = metrics_with(0, HOP, |_| 0.0);
    assert!(lane_visible_frames(&none, 0, 0, 1000, 300.0, LANE_MARGIN_PX).is_empty());
    assert_eq!(
        lane_visible_frames(m.as_ref(), 0, 0, 0, 300.0, LANE_MARGIN_PX),
        FrameWindow {
            first: 0,
            last: 5624
        }
    );
    assert_eq!(
        lane_visible_frames(m.as_ref(), 0, 0, 1000, 0.0, LANE_MARGIN_PX),
        FrameWindow {
            first: 0,
            last: 5624
        }
    );
    // A 1 s window at 30 s: the frames centred in it, plus the margin and
    // one frame each side.
    let w = lane_visible_frames(m.as_ref(), 0, 30000, 1000, 300.0, LANE_MARGIN_PX);
    let margin_s = LANE_MARGIN_PX * 1000.0 / 300.0 / 1000.0;
    assert_eq!(
        w.first,
        m.frame_position(30.0 - margin_s).floor() as i64 - 1
    );
    assert_eq!(w.last, m.frame_position(31.0 + margin_s).ceil() as i64 + 1);
    assert!(m.time_of_frame(w.first + 1) < 30.0);
    assert!(m.time_of_frame(w.last - 1) > 31.0);
    // Past the end: empty.
    assert!(lane_visible_frames(m.as_ref(), 0, 999_999, 1000, 300.0, LANE_MARGIN_PX).is_empty());
}

// ----------------------------------------------- analysis_frame_timing_test

/// The production framing: frame f analyses the 2048-sample window starting
/// at f·512, so it sits at the window's CENTRE.
fn centre_seconds(f: i64) -> f64 {
    (f * HOP + FFT / 2) as f64 / SR as f64
}

const HOP_MS: f64 = HOP as f64 / SR as f64 * 1000.0;

#[test]
fn the_pitch_lane_starts_the_note_block_and_contour_at_500_ms() {
    // A 1.2 s take, silent then a steady 220 Hz tone from exactly 500 ms: the
    // analysis voices the frames whose window centre is past the onset.
    let frames = ((1200 * SR / 1000 - FFT) / HOP + 1) as usize;
    let m = Arc::new(metrics_with(frames, HOP, |f| {
        if centre_seconds(f as i64) >= 0.5 {
            220.0
        } else {
            f64::NAN
        }
    }));
    let notes: Arc<[PitchNote]> = segment_pitch_notes(&m, MIN_NOTE_FRAMES, MAX_NOTE_GAP).into();
    assert_eq!(notes.len(), 1);
    // 1 px per ms: x reads directly as a time.
    let p = PitchLanePainter {
        metrics: Arc::clone(&m),
        notes: Arc::clone(&notes),
        worst: Arc::new(BTreeSet::new()),
        timeline_duration_ms: 1200,
        start_ms: 0,
        view_start_ms: 0,
        lo_semi: 50,
        hi_semi: 62,
    };
    let plan = p.plan(Size::new(1200.0, 96.0));
    let Some(PitchOp::Block { left, .. }) = plan.notes.first() else {
        panic!("a block")
    };
    assert!((left - 500.0).abs() <= HOP_MS, "{left}");
    assert!((left - m.time_of_frame(notes[0].start_frame as i64) * 1000.0).abs() < 1e-6);
    // The contour's first segment begins on the same frame, and its last ends
    // on the last voiced frame's centre — not half a window early.
    let PitchOp::Contour { from, .. } = plan.contour.first().expect("contour") else {
        panic!("a segment")
    };
    assert!((from.0 - 500.0).abs() <= HOP_MS);
    let PitchOp::Contour { to, .. } = plan.contour.last().expect("contour") else {
        panic!("a segment")
    };
    assert!((to.0 - m.time_of_frame(notes[0].end_frame as i64) * 1000.0).abs() < 1e-6);
}

/// Frames 0..19 sing A3, frames 20..39 sing C4: a playhead two hops early or
/// late reads the wrong side.
fn step_metrics() -> Arc<VoiceMetrics> {
    Arc::new(metrics_with(
        40,
        HOP,
        |f| if f < 20 { 220.0 } else { 261.6256 },
    ))
}

#[test]
fn the_pitch_lane_names_the_note_the_frame_under_it_sings() {
    let step = step_metrics();
    let notes = pitch_notes_of(&step);
    // The last A3 frame's centre (224 ms): a start-anchored mapping reads two
    // hops ahead, into the C4 frames.
    let at_last_a3 = (centre_seconds(19) * 1000.0).round() as i64;
    assert_eq!(playhead_note_text(&step, &notes, 0, at_last_a3), "A3 +0¢");
    // Inside the LAST frame's cell, past its centre, is still over the take.
    let in_last_cell = (step.cell_end(39) * 1000.0).floor() as i64;
    assert_eq!(playhead_note_text(&step, &notes, 0, in_last_cell), "C4 +0¢");
    // Off the take either side: no note (frame_at_time would clamp).
    assert_eq!(playhead_note_text(&step, &notes, 0, 1000), "—");
    assert_eq!(playhead_note_text(&step, &notes, 300, 100), "—");
    // Placed later on the timeline, the take reads from its start.
    assert_eq!(
        playhead_note_text(&step, &notes, 5000, 5000 + at_last_a3),
        "A3 +0¢"
    );

    // And the lane shows it.
    let lane = |position_ms| {
        Element::<()>::from(pitch_lane(
            &track(0, 500),
            500,
            position_ms,
            180.0,
            false,
            Some(Arc::clone(&step)),
        ))
    };
    let mut ui = iced_test::simulator(lane(at_last_a3));
    assert!(ui.find("A3 +0¢").is_ok());
    assert!(ui.find("PITCH").is_ok());
    let mut ui = iced_test::simulator(lane(in_last_cell));
    assert!(ui.find("C4 +0¢").is_ok());
}

// ------------------------------------------------------ analysis_lanes_test

/// Renders the lane (drawing every layer) and returns what it shows.
fn render_lane(lane: PitchLane) {
    let mut ui = iced_test::Simulator::with_size(
        Settings::default(),
        Size::new(1440.0, 96.0),
        Element::<()>::from(lane),
    );
    ui.snapshot(&app_theme()).expect("the lane renders");
}

/// A one-second take at 220 Hz with breaths, as the analysis reads a sine.
fn normal_take() -> Arc<VoiceMetrics> {
    Arc::new(metrics_with(90, HOP, |i| {
        if i % 30 < 26 {
            220.0
        } else {
            f64::NAN
        }
    }))
}

#[test]
fn the_lane_renders_every_kind_of_analysis() {
    let takes = [
        ("normal", normal_take()),
        // Silence: nothing voiced.
        ("silent", Arc::new(metrics_with(90, HOP, |_| f64::NAN))),
        // Non-finite values in the F0 curve.
        (
            "non-finite",
            Arc::new(metrics_with(90, HOP, |i| match i % 10 {
                1 => f64::NAN,
                2 => f64::INFINITY,
                3 => f64::NEG_INFINITY,
                _ => 220.0,
            })),
        ),
        // A take stopped after one buffer: one frame, or none.
        ("one frame", Arc::new(metrics_with(1, HOP, |_| 220.0))),
        ("no frames", Arc::new(metrics_with(0, HOP, |_| 220.0))),
    ];
    for (_, m) in takes {
        render_lane(pitch_lane(
            &track(0, 1000),
            20000,
            250,
            180.0,
            false,
            Some(m),
        ));
    }
}

#[test]
fn no_analysis_yet_shows_the_hints() {
    let lane = pitch_lane(&track(0, 1000), 20000, 250, 180.0, true, None);
    let mut ui = iced_test::simulator(Element::<()>::from(lane.clone()));
    assert!(ui.find("Analyzing pitch…").is_ok());
    assert!(ui.find("PITCH").is_ok());
    render_lane(lane);
    // Loading hides even metrics already in hand.
    let mut ui = iced_test::simulator(Element::<()>::from(pitch_lane(
        &track(0, 1000),
        20000,
        250,
        180.0,
        true,
        Some(normal_take()),
    )));
    assert!(ui.find("Analyzing pitch…").is_ok());
    let mut ui = iced_test::simulator(Element::<()>::from(pitch_lane(
        &track(0, 1000),
        20000,
        250,
        180.0,
        false,
        None,
    )));
    assert!(ui
        .find("Pitch is available for recorded WAV takes.")
        .is_ok());
    assert!(ui.find("—").is_err(), "no readout without metrics");
}

#[test]
fn degenerate_spans_render() {
    let m = normal_take();
    let lane = |start_ms, duration_ms, timeline_ms, view_start_ms, position_ms| {
        pitch_lane(
            &track(start_ms, duration_ms),
            timeline_ms,
            position_ms,
            180.0,
            false,
            Some(Arc::clone(&m)),
        )
        .view_start_ms(view_start_ms)
    };
    // A zero-length timeline is not divided by.
    render_lane(lane(0, 1000, 0, 0, 250));
    // A zero-length TAKE.
    render_lane(lane(0, 0, 20000, 0, 250));
    // A viewport scrolled past the end.
    render_lane(lane(0, 1000, 20000, 999_999, 250));
    // A take starting beyond the timeline.
    render_lane(lane(999_999, 1000, 20000, 0, 250));
    // The playhead far outside the take (a day in).
    render_lane(lane(0, 1000, 20000, 0, 86_400_000));
    // Take-local (the Pitch dock tab), and a taller lane.
    render_lane(
        lane(4000, 1000, 1000, 0, 4250)
            .take_local(true)
            .height(300.0),
    );
}

#[test]
fn the_header_scores_the_take() {
    let m = normal_take();
    let notes = pitch_notes_of(&m);
    assert_eq!(pitch_in_tune_percent(&notes), 100.0);
    assert!(pitch_in_tune_percent(&[]).is_nan());
    let off = PitchNote {
        start_frame: 0,
        end_frame: 9,
        semitone: 57,
        median_cents: 40.0,
    };
    let on = PitchNote {
        start_frame: 10,
        end_frame: 39,
        semitone: 57,
        median_cents: -25.0,
    };
    assert_eq!(pitch_in_tune_percent(&[off, on]), 75.0);
    assert_eq!(worst_notes(&[off, on]), BTreeSet::from([0]));
}
