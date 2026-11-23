//! Ports of `spectrogram_image_test.dart`, `live_columns_raster_test.dart`,
//! and the spectrogram-placement parts of `analysis_frame_timing_test.dart`
//! and `lane_painter_window_test.dart`.

use super::*;
use std::sync::Mutex as StdMutex;

/// The image cache is process-wide; tests that use it take turns.
static CACHE_TESTS: StdMutex<()> = StdMutex::new(());

fn cache_lock() -> std::sync::MutexGuard<'static, ()> {
    CACHE_TESTS.lock().unwrap_or_else(|e| e.into_inner())
}

/// A small deterministic generator (the Dart tests seeded `math.Random`).
struct Lcg(u64);

impl Lcg {
    fn next_f64(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
    fn next_int(&mut self, n: u64) -> u64 {
        (self.next_f64() * n as f64) as u64
    }
}

fn spec(frames: usize, bins: usize, db: impl Fn(usize, usize) -> f64) -> PlainSpectrogram {
    let mut values = vec![0f32; frames * bins];
    let mut max_db = f64::NEG_INFINITY;
    for f in 0..frames {
        for b in 0..bins {
            let v = db(f, b) as f32;
            values[f * bins + b] = v;
            if f64::from(v) > max_db {
                max_db = f64::from(v);
            }
        }
    }
    PlainSpectrogram {
        frames,
        bins,
        sample_rate: 48000,
        fft_size: 2048,
        hop: 512,
        db: values,
        max_db,
    }
}

/// The RGB of pixel (`x`, `y`) in an image `width` wide.
fn rgb(px: &[u8], width: usize, x: usize, y: usize) -> [u8; 3] {
    let i = (y * width + x) * 4;
    [px[i], px[i + 1], px[i + 2]]
}

// With a 60 dB peak the 72 dB window runs from -12 (bottom colour) to 60.
const BOTTOM: [u8; 3] = [14, 14, 18];
const TOP: [u8; 3] = [255, 255, 255];

#[test]
fn one_column_per_frame_paints_the_heat_map_low_frequency_at_the_bottom() {
    let values = [60.0, 24.0, -12.0, f64::NAN, -40.0, 60.0];
    let s = spec(2, 3, |f, b| values[f * 3 + b]);
    let px = spectrogram_pixels(&s, 72.0, MAX_SPECTROGRAM_IMAGE_COLUMNS);
    assert_eq!((px.width, px.height), (2, 3));
    // Frame 0: bin 0 (bottom row) at the peak, bin 1 mid-scale, bin 2 floor.
    assert_eq!(rgb(&px.pixels, 2, 0, 2), TOP);
    assert_eq!(rgb(&px.pixels, 2, 0, 1), [158, 81, 109]);
    assert_eq!(rgb(&px.pixels, 2, 0, 0), BOTTOM);
    // Frame 1: NaN and below-the-window read as the bottom colour.
    assert_eq!(rgb(&px.pixels, 2, 1, 2), BOTTOM);
    assert_eq!(rgb(&px.pixels, 2, 1, 1), BOTTOM);
    assert_eq!(rgb(&px.pixels, 2, 1, 0), TOP);
}

#[test]
fn a_take_within_the_cap_keeps_one_column_per_frame() {
    let mut r = Lcg(3);
    let values: Vec<f64> = (0..400 * 5).map(|_| r.next_f64() * 70.0 - 10.0).collect();
    let s = spec(400, 5, |f, b| values[f * 5 + b]);
    let capped = spectrogram_pixels(&s, 72.0, 400);
    let uncapped = spectrogram_pixels(&s, 72.0, 1 << 30);
    assert_eq!(capped.width, 400);
    assert_eq!(capped.pixels, uncapped.pixels);
}

#[test]
fn past_the_cap_columns_max_pool_equal_shares_of_the_take() {
    // One hot bin in frame 200 of 307, pooled to 100 columns: frames
    // [199, 202) form column 65.
    let s = spec(307, 4, |f, b| {
        if f == 200 && b == 2 {
            60.0
        } else if f == 10 && b == 0 {
            f64::NAN
        } else {
            -12.0
        }
    });
    let px = spectrogram_pixels(&s, 72.0, 100);
    assert_eq!((px.width, px.height), (100, 4));
    for c in 0..100 {
        for row in 0..4 {
            let hot = c == 65 && row == 4 - 1 - 2;
            assert_eq!(
                rgb(&px.pixels, 100, c, row),
                if hot { TOP } else { BOTTOM },
                "column {c} row {row}"
            );
        }
    }
}

#[test]
fn a_take_longer_than_the_texture_limit_is_capped_to_it() {
    let s = spec(MAX_SPECTROGRAM_IMAGE_COLUMNS + 1000, 1, |f, _| {
        (f % 60) as f64
    });
    let px = spectrogram_pixels(&s, 72.0, MAX_SPECTROGRAM_IMAGE_COLUMNS);
    assert_eq!(px.width, MAX_SPECTROGRAM_IMAGE_COLUMNS);
    assert_eq!(px.height, 1);
}

#[test]
fn an_empty_spectrogram_renders_a_transparent_placeholder() {
    let s = spec(0, 8, |_, _| 0.0);
    match render_spectrogram_image(&s) {
        Handle::Rgba {
            width,
            height,
            pixels,
            ..
        } => {
            assert_eq!((width, height), (1, 1));
            assert_eq!(&pixels[..], &[0, 0, 0, 0]);
        }
        other => panic!("unexpected handle {other:?}"),
    }
}

fn size_of(h: &Handle) -> (u32, u32) {
    match h {
        Handle::Rgba { width, height, .. } => (*width, *height),
        _ => (0, 0),
    }
}

#[test]
fn a_take_is_rendered_once_and_its_handles_outlive_eviction() {
    let _guard = cache_lock();
    release_spectrogram_images();
    let mut r = Lcg(5);
    let mut take = || {
        let values: Vec<f64> = (0..50 * 8).map(|_| r.next_f64() * 60.0).collect();
        Arc::new(spec(50, 8, |f, b| values[f * 8 + b]))
    };
    let first = take();
    let a = spectrogram_image_for(&first);
    let b = spectrogram_image_for(&first);
    assert_eq!(a.id(), b.id(), "one render, shared");
    assert_eq!(size_of(&a), (50, 8));

    let others: Vec<_> = (0..5).map(|_| take()).collect();
    for s in &others {
        let h = spectrogram_image_for(s);
        assert_ne!(h.id(), a.id());
    }
    // `first` has been evicted (the cache is smaller than six takes): the
    // handles already out still hold the image...
    assert_eq!(size_of(&a), (50, 8));
    // ...and asking again renders it afresh.
    let c = spectrogram_image_for(&first);
    assert_ne!(c.id(), a.id());
}

#[test]
fn overlapping_requests_and_evictions_hand_out_whole_images() {
    let _guard = cache_lock();
    release_spectrogram_images();
    let takes: Vec<_> = (0..7)
        .map(|i| Arc::new(spec(300, 16, move |f, b| ((f * b + i) % 60) as f64)))
        .collect();
    for _round in 0..3 {
        let handles: Vec<_> = std::thread::scope(|scope| {
            let mut joins = Vec::new();
            for (i, s) in takes.iter().enumerate() {
                let s = Arc::clone(s);
                joins.push(scope.spawn(move || spectrogram_image_for(&s)));
                // Re-ask the first take while it is in flight and being evicted.
                if i % 2 == 1 {
                    let first = Arc::clone(&takes[0]);
                    joins.push(scope.spawn(move || spectrogram_image_for(&first)));
                }
            }
            joins.into_iter().map(|j| j.join().unwrap()).collect()
        });
        for h in handles {
            assert_eq!(size_of(&h), (300, 16));
        }
    }
}

#[test]
fn a_closed_screen_leaves_the_cache_holding_no_image_and_shown_images_intact() {
    let _guard = cache_lock();
    let s = Arc::new(spec(40, 8, |f, b| ((f + b) % 60) as f64));
    let shown = spectrogram_image_for(&s);
    release_spectrogram_images();
    assert_eq!(size_of(&shown), (40, 8), "the shown image stays whole");
    let again = spectrogram_image_for(&s);
    assert_ne!(again.id(), shown.id(), "rendered afresh");
}

#[test]
fn the_cache_does_not_keep_a_spectrogram_alive() {
    // A strong key kept every recently shown spectrogram — tens to hundreds
    // of MB for a long take — resident after the analysis cache let it go
    // and after the practice closed, until four newer takes were rendered.
    let _guard = cache_lock();
    let s = Arc::new(spec(2000, 64, |f, b| ((f + b) % 60) as f64));
    let weak = Arc::downgrade(&s);
    drop(spectrogram_image_for(&s)); // the widget is gone
    drop(s);
    assert!(weak.upgrade().is_none());
}

// Regression guard: switching the dock to another take kept painting the
// PREVIOUS take's image — stretched over the new take's frames, under its F0
// and formant dots — until the new render landed, which is hundreds of ms
// for a long take that has fallen out of the image cache.
#[test]
fn a_new_take_shows_nothing_until_its_own_image_is_ready() {
    let _guard = cache_lock();
    let mut image = SpectrogramImage::new();
    let first = Arc::new(spec(60, 8, |f, b| ((f + b) % 60) as f64));
    let request = image.show(&first).expect("first take renders");
    assert!(image.image().is_none());
    image.rendered(request.run());
    assert!(image.image().is_some(), "the first take renders");
    assert!(image.show(&first).is_none(), "the same take renders once");

    let second = Arc::new(spec(90, 8, |f, b| ((f * b) % 60) as f64));
    let request = image.show(&second).expect("second take renders");
    assert!(
        image.image().is_none(),
        "the first take must not stand in while the second renders"
    );
    image.rendered(request.run());
    assert_eq!(
        size_of(image.image().unwrap()),
        (90, 8),
        "then the second take shows"
    );
}

#[test]
fn a_render_finishing_after_a_newer_one_is_dropped() {
    let _guard = cache_lock();
    let mut image = SpectrogramImage::new();
    let a = Arc::new(spec(10, 4, |_, _| 1.0));
    let b = Arc::new(spec(20, 4, |_, _| 2.0));
    let late = image.show(&a).unwrap();
    let current = image.show(&b).unwrap();
    image.rendered(current.run());
    image.rendered(late.run());
    assert_eq!(size_of(image.image().unwrap()), (20, 4));
}

// ------------------------------------------------- live_columns_raster_test

const BINS: usize = 24;
const MAX_COLUMNS: usize = 60;

fn column(r: &mut Lcg, loud: f64) -> LiveColumn {
    (0..BINS)
        .map(|_| (r.next_f64() * 40.0 - 10.0 + loud) as f32)
        .collect::<Vec<f32>>()
        .into()
}

#[test]
fn every_update_is_exactly_the_full_window_colouring() {
    let mut r = Lcg(11);
    let mut raster = LiveColumnsRaster::default();
    let mut cols: Vec<LiveColumn> = Vec::new();
    let mut active: Vec<u8> = Vec::new();
    let mut step = 0;
    let mut push =
        |n: usize, loud: f64, cols: &mut Vec<LiveColumn>, active: &mut Vec<u8>, r: &mut Lcg| {
            for _ in 0..n {
                cols.push(column(r, loud));
                active.push(if r.next_int(3) == 0 { 0 } else { 1 });
                if cols.len() > MAX_COLUMNS {
                    cols.remove(0);
                    active.remove(0);
                }
            }
            let got = raster.update(cols, BINS, Some(active));
            let want = live_columns_pixels(cols, BINS, 72.0, Some(active));
            assert!(got == want, "step {step} ({} columns)", cols.len());
            step += 1;
        };
    // Filling the window (it grows), then scrolling.
    for _ in 0..40 {
        let n = 1 + r.next_int(4) as usize;
        push(n, 0.0, &mut cols, &mut active, &mut r);
    }
    // A loud column raises the ceiling; scrolling on until it leaves lowers
    // it again.
    push(1, 40.0, &mut cols, &mut active, &mut r);
    for _ in 0..30 {
        let n = 1 + r.next_int(3) as usize;
        push(n, 0.0, &mut cols, &mut active, &mut r);
    }
    // Several updates' worth of columns at once, and none at all.
    push(25, 0.0, &mut cols, &mut active, &mut r);
    push(0, 0.0, &mut cols, &mut active, &mut r);
    // The monitor restarted: a window with nothing in common with the last.
    cols.clear();
    active.clear();
    push(5, 0.0, &mut cols, &mut active, &mut r);
    push(3, 0.0, &mut cols, &mut active, &mut r);
}

#[test]
fn a_column_already_on_screen_is_not_coloured_again() {
    // Dart pinned the reuse by mutating an on-screen column in place (the
    // monitor never does) and seeing the stale colours survive; Rust's
    // columns are immutable, so the raster reports what it coloured.
    let mut r = Lcg(12);
    let mut raster = LiveColumnsRaster::default();
    let mut cols: Vec<LiveColumn> = (0..10).map(|_| column(&mut r, 0.0)).collect();
    cols.push(column(&mut r, 30.0)); // holds the ceiling
    raster.update(&cols, BINS, None);
    assert_eq!(raster.coloured_columns(), 11);
    cols.push(column(&mut r, 0.0));
    let got = raster.update(&cols, BINS, None);
    assert_eq!(raster.coloured_columns(), 1, "only the new column");
    assert!(got == live_columns_pixels(&cols, BINS, 72.0, None));
}

#[test]
fn the_live_colouring_is_the_take_colouring_of_the_same_bins() {
    // Voiced columns use the take renderer's scale and colour map: the same
    // bins as a spectrogram (whose peak is the window's) paint identically.
    let mut r = Lcg(13);
    let cols: Vec<LiveColumn> = (0..30)
        .map(|i| column(&mut r, if i == 7 { 30.0 } else { 0.0 }))
        .collect();
    let mut db = Vec::with_capacity(cols.len() * BINS);
    let mut peak = f64::NEG_INFINITY;
    for c in &cols {
        for &v in c.iter() {
            db.push(v);
            peak = peak.max(f64::from(v));
        }
    }
    let s = PlainSpectrogram {
        frames: cols.len(),
        bins: BINS,
        sample_rate: 48000,
        fft_size: 2048,
        hop: 512,
        db,
        max_db: peak,
    };
    assert!(
        live_columns_pixels(&cols, BINS, 72.0, None)
            == spectrogram_pixels(&s, 72.0, MAX_SPECTROGRAM_IMAGE_COLUMNS).pixels
    );
}

// -------------------------------------------- analysis_frame_timing_test

const SR: u32 = 48000;
const HOP: u32 = 512;
const FFT: u32 = 2048;
const HOP_MS: f64 = HOP as f64 / SR as f64 * 1000.0;

#[test]
fn image_columns_sit_on_their_frames_window_centres() {
    // The SpectrogramTab case: a 1200 ms take, its frames' columns placed on
    // the take's axis.
    let take_ms = 1200;
    let frames = ((take_ms * SR as usize / 1000) - FFT as usize) / HOP as usize + 1;
    let s = spec(frames, 4, |_, _| 0.0);
    let w = 1300.0;
    let dst = spectrogram_image_rect(&s, take_ms as i64, Size::new(w, 300.0)).unwrap();
    let col_w = f64::from(dst.width) / frames as f64;
    let ms_of_x = |x: f64| x / f64::from(w) * take_ms as f64;
    for c in [0, 45, frames - 1] {
        let mid = ms_of_x(f64::from(dst.x) + (c as f64 + 0.5) * col_w);
        assert!(
            (mid - s.time_of_frame(c) * 1000.0).abs() < 0.01,
            "column {c}"
        );
    }
}

#[test]
fn a_max_pooled_column_sits_over_the_frames_it_pools() {
    // Past the texture cap, image column c holds frames
    // [c·frames/w, (c+1)·frames/w) — and must be drawn where THOSE frames'
    // centres fall on the axis, like an unpooled column.
    const FRAMES: usize = 20000;
    const HOT: usize = 60;
    let s = spec(
        FRAMES,
        2,
        |f, b| if f == HOT && b == 0 { 60.0 } else { -12.0 },
    );
    let take_ms = (((FRAMES - 1) * HOP as usize + FFT as usize) * 1000 / SR as usize) as i64;
    let size = Size::new(700.0, 40.0);
    let dst = spectrogram_image_rect(&s, take_ms, size).unwrap();
    let w = spectrogram_pixels(&s, 72.0, MAX_SPECTROGRAM_IMAGE_COLUMNS).width;
    assert_eq!(w, MAX_SPECTROGRAM_IMAGE_COLUMNS);
    let c = (0..w)
        .filter(|&c| c * FRAMES / w <= HOT && HOT < (c + 1) * FRAMES / w)
        .collect::<Vec<_>>();
    assert_eq!(c.len(), 1);
    let c = c[0];
    let ms_of = |x: f64| x / f64::from(size.width) * take_ms as f64;
    let col_w = f64::from(dst.width) / w as f64;
    let centre_ms = s.time_of_frame(HOT) * 1000.0;
    // The pooled column spans its frames' cells, give or take the half frame
    // its integer frame bounds round off: the hot frame's window centre falls
    // inside it.
    assert!(centre_ms >= ms_of(f64::from(dst.x) + c as f64 * col_w) - HOP_MS / 2.0);
    assert!(centre_ms <= ms_of(f64::from(dst.x) + (c + 1) as f64 * col_w));
}

fn tracks(frames: usize) -> PlainFormantTracks {
    let curve = |base: f32| -> Vec<f32> {
        (0..frames)
            .map(|f| {
                if f % 5 == 0 {
                    f32::NAN
                } else {
                    base + (f % 7) as f32 * 10.0
                }
            })
            .collect()
    };
    PlainFormantTracks {
        frames,
        sample_rate: SR,
        fft_size: FFT,
        hop: HOP,
        f0: curve(220.0),
        f1: curve(500.0),
        f2: curve(1500.0),
        f3: curve(2600.0),
    }
}

#[test]
fn every_f0_dot_sits_at_its_frames_window_centre() {
    let m = tracks(120);
    let take_ms = ((m.frames - 1) * HOP as usize + FFT as usize) * 1000 / SR as usize;
    let size = Size::new(700.0, 220.0);
    let dots = formant_dots(&m, 5000.0, take_ms as i64, size);
    // F0 dots are the larger ones (1.4 px, formants 1.1 px), drawn last.
    let f0: Vec<_> = dots.iter().filter(|d| (d.1 - 1.4).abs() < 1e-6).collect();
    let voiced: Vec<usize> = (0..m.frames)
        .filter(|&f| m.f0[f].is_finite() && m.f0[f] > 0.0)
        .collect();
    assert_eq!(f0.len(), voiced.len());
    for (dot, &f) in f0.iter().zip(&voiced) {
        let ms = f64::from(dot.0.x) / f64::from(size.width) * take_ms as f64;
        assert!((ms - m.time_of_frame(f) * 1000.0).abs() < 0.01);
    }
    assert!(dots.len() > 4 * voiced.len() - 4, "all four tracks draw");
}

#[test]
fn dots_skip_unvoiced_and_out_of_range_frames() {
    let mut m = tracks(10);
    m.f1 = vec![0.0, -5.0, f32::NAN, 6000.0, 500.0, 0.0, 0.0, 0.0, 0.0, 0.0];
    let dots = formant_dots(&m, 5000.0, 1000, Size::new(100.0, 100.0));
    let f1: Vec<_> = dots
        .iter()
        .filter(|d| d.2 == alpha(FORMANT_COLORS[0], 0.9))
        .collect();
    assert_eq!(f1.len(), 1);
    assert!((f1[0].0.y - 90.0).abs() < 1e-4);
    assert!(formant_dots(&m, 5000.0, 0, Size::new(100.0, 100.0)).is_empty());
}

#[test]
fn live_formant_dots_spread_each_history_over_the_width() {
    let h = [100.0, f64::NAN, 300.0, 0.0];
    let dots = live_formant_dots(&h, &[], &[], &[], 400.0, Size::new(40.0, 100.0));
    assert_eq!(dots.len(), 2);
    assert!((dots[0].0.x - 5.0).abs() < 1e-5 && (dots[0].0.y - 75.0).abs() < 1e-5);
    assert!((dots[1].0.x - 25.0).abs() < 1e-5);
}

#[test]
fn the_power_spectrum_auto_scales_and_marks_its_formants() {
    let column: Arc<[f32]> = vec![0.0f32; 100].into();
    let mut p = PowerSpectrumPainter::new(column, 50.0, 5000.0);
    p.formants = vec![500.0, f64::NAN, 2500.0, 3000.0];
    p.min_ceil_db = Some(48.0);
    let shape = p.shape(500.0, 100.0).unwrap();
    // A silent column with a 48 dB floor ceiling reads as a low flat line.
    assert!(shape.area[1..shape.area.len() - 1]
        .iter()
        .all(|&(_, y)| (y - 100.0 * 48.0 / 78.0).abs() < 1e-9));
    assert_eq!(shape.markers, vec![(50.0, 0), (250.0, 2)]);
    assert_eq!(shape.band, (260.0, 380.0));
    assert!(PowerSpectrumPainter::new(Vec::new().into(), 50.0, 5000.0)
        .shape(1.0, 1.0)
        .is_none());
}

#[test]
fn the_vowel_chart_puts_high_f2_left_and_high_f1_low() {
    let size = Size::new(210.0, 80.0);
    let i = vowel_position(300.0, 2300.0, size);
    let a = vowel_position(750.0, 1150.0, size);
    assert!(i.x < a.x && i.y < a.y);
    assert_eq!(vowel_position(0.0, 9999.0, size), Point::new(0.0, 0.0));
}
