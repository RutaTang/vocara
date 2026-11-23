//! The song screen's timeline geometry, from
//! `ui/song_practice_detail/widgets/song_practice_detail_screen.dart`: the
//! scrollbar thumb under the lanes (`timelineScrollThumb`) and the master
//! and bus lanes' whole-timeline envelope (`timelineEnvelopePainter`). Both
//! are pure, so the screen draws them and their tests
//! (`timeline_scrollbar_test.dart`, `timeline_envelope_test.dart`) run
//! without a window.

use std::sync::Arc;

use iced::Color;
use vocara_model::util::dart_num::clamp_f64;

use crate::ui::core::widgets::waveform_painter::{visible_peak_slice, WaveformPainter};

/// The thumb's floor: it stays grabbable at deep zoom.
pub const SCROLL_THUMB_MIN_WIDTH: f64 = 28.0;

/// The timeline scrollbar's thumb in its groove.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrollThumb {
    /// Its left edge in the groove.
    pub left: f64,
    /// Its width.
    pub width: f64,
    /// How many ms one px of thumb drag pans (0 when there is nothing to
    /// scroll).
    pub ms_per_px: f64,
}

/// The timeline scrollbar's thumb in a groove [groove_w] px wide, for the
/// view [start_ms] + [span_ms] over a timeline [extent_ms] long.
///
/// The thumb has a [`SCROLL_THUMB_MIN_WIDTH`] floor so it stays grabbable at
/// deep zoom, so it is wider than its share of the timeline and its left
/// edge can only travel `groove_w - width`: the start maps over THAT range
/// ([0, extent − span] onto [0, travel]), and a drag maps back the same way.
/// Mapping the start onto the whole groove instead ran the floored thumb off
/// the groove's end near the last page, clipped to a ~2 px sliver at deep
/// zoom.
pub fn timeline_scroll_thumb(
    start_ms: i64,
    span_ms: i64,
    extent_ms: i64,
    groove_w: f64,
) -> ScrollThumb {
    let floor = if groove_w < SCROLL_THUMB_MIN_WIDTH {
        groove_w
    } else {
        SCROLL_THUMB_MIN_WIDTH
    };
    let width = clamp_f64(
        span_ms as f64 / extent_ms as f64 * groove_w,
        floor,
        groove_w,
    );
    let travel = groove_w - width;
    // A song shorter than the minimum view span has span > extent.
    let scroll_ms = extent_ms - span_ms;
    if travel <= 0.0 || scroll_ms <= 0 {
        return ScrollThumb {
            left: 0.0,
            width,
            ms_per_px: 0.0,
        };
    }
    ScrollThumb {
        left: clamp_f64(start_ms as f64 / scroll_ms as f64 * travel, 0.0, travel),
        width,
        ms_per_px: scroll_ms as f64 / travel,
    }
}

/// The painter for a whole-timeline envelope — the master or a bus lane's,
/// [peaks] bucketed over [extent_ms] from 0 — in the view [view_start_ms] +
/// [view_span_ms]: only the visible slice, anchored through the shared
/// [`visible_peak_slice`] so it lines up with the zoomed lanes, ruler and
/// playhead above.
///
/// The view can reach past [extent_ms] (a take recording on past the
/// timeline's end stretches the view until it is saved; a song shorter than
/// the minimum view span): the slice then fills only the lane's share up to
/// the timeline's end. Spread over the whole lane, every peak drew late.
pub fn timeline_envelope_painter(
    peaks: Arc<[f64]>,
    extent_ms: i64,
    view_start_ms: i64,
    view_span_ms: i64,
    color: Color,
) -> WaveformPainter {
    if extent_ms <= 0 || peaks.is_empty() || (view_start_ms <= 0 && view_span_ms == extent_ms) {
        return WaveformPainter::new(peaks, color);
    }
    let end = if view_start_ms + view_span_ms > extent_ms {
        extent_ms
    } else {
        view_start_ms + view_span_ms
    };
    if end <= view_start_ms {
        return WaveformPainter::new(Arc::from(Vec::new()), color);
    }
    let cut = visible_peak_slice(peaks.len(), 0, extent_ms, view_start_ms, end);
    WaveformPainter::sliced(peaks, cut, color)
        .width_fraction((end - view_start_ms) as f64 / view_span_ms as f64)
}

#[cfg(test)]
mod tests {
    use iced::Size;

    use super::*;

    // ------------------------------------------------ timeline_scrollbar_test
    //
    // A 10-minute song in a 600 px groove viewed through a 1-second window:
    // the view's true share is 1 px, so the thumb sits on its 28 px floor.
    const EXT: i64 = 600_000;
    const SPAN: i64 = 1000;
    const GROOVE: f64 = 600.0;

    #[test]
    fn at_the_last_page_the_thumb_ends_at_the_groove_end_not_past_it() {
        let t = timeline_scroll_thumb(EXT - SPAN, SPAN, EXT, GROOVE);
        assert_eq!(t.width, 28.0);
        assert!((t.left + t.width - GROOVE).abs() <= 1e-9, "{t:?}");
    }

    #[test]
    fn at_the_first_page_the_thumb_starts_at_the_groove_start() {
        let t = timeline_scroll_thumb(0, SPAN, EXT, GROOVE);
        assert_eq!(t.left, 0.0);
    }

    #[test]
    fn dragging_the_thumb_across_its_whole_travel_pans_the_whole_song() {
        let t = timeline_scroll_thumb(0, SPAN, EXT, GROOVE);
        assert!((t.ms_per_px * (GROOVE - t.width) - (EXT - SPAN) as f64).abs() <= 1e-6);
    }

    #[test]
    fn at_fit_all_the_thumb_fills_the_groove_and_does_not_drag() {
        let t = timeline_scroll_thumb(0, EXT, EXT, GROOVE);
        assert_eq!(t.left, 0.0);
        assert_eq!(t.width, GROOVE);
        assert_eq!(t.ms_per_px, 0.0);
    }

    #[test]
    fn a_song_shorter_than_the_minimum_view_span_does_not_divide_by_the_negative_scroll_range() {
        // The view never narrows below 1 s, so a 400 ms song has span > extent.
        let t = timeline_scroll_thumb(0, 1000, 400, GROOVE);
        assert_eq!(t.left, 0.0);
        assert_eq!(t.width, GROOVE);
        assert_eq!(t.ms_per_px, 0.0);
    }

    // ------------------------------------------------- timeline_envelope_test
    //
    // The master and bus lanes draw one envelope bucketed over the whole
    // timeline, sliced to the view. While a take runs on past the timeline's
    // end the view reaches past it too (the extent follows the record head
    // until the take is saved), and an envelope still spread over the whole
    // lane put every peak of the song at the wrong time — stretched right,
    // against the lanes, ruler and playhead above it.

    /// A 20 s timeline in 1000 buckets (20 ms each).
    const TIMELINE: i64 = 20000;
    const LANE: Size = Size::new(600.0, 40.0);

    /// 1000 silent peaks, loud only at [at].
    fn spiked(at: usize) -> Arc<[f64]> {
        (0..1000).map(|i| if i == at { 1.0 } else { 0.0 }).collect()
    }

    /// Whether (x, y) is inside the closed polygon [outline] (even-odd).
    fn inside(outline: &[(f64, f64)], x: f64, y: f64) -> bool {
        let mut hit = false;
        let mut j = outline.len() - 1;
        for i in 0..outline.len() {
            let (xi, yi) = outline[i];
            let (xj, yj) = outline[j];
            if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
                hit = !hit;
            }
            j = i;
        }
        hit
    }

    /// The x-center of the topmost painted row of [painter] in [size], as
    /// pixel centres rasterize it — with a single full-amplitude spike among
    /// silent peaks, the spike's drawn position.
    fn spike_x(painter: &WaveformPainter, size: Size) -> f64 {
        let shape = painter.shape(size).expect("nothing painted");
        let right = if shape.clip {
            shape.draw_width
        } else {
            f64::from(size.width)
        };
        for y in 0..size.height as usize {
            let cy = y as f64 + 0.5;
            let painted: Vec<usize> = (0..size.width as usize)
                .filter(|&x| {
                    let cx = x as f64 + 0.5;
                    cx < right && inside(&shape.outline, cx, cy)
                })
                .collect();
            if let (Some(lo), Some(hi)) = (painted.first(), painted.last()) {
                return (lo + hi) as f64 / 2.0;
            }
        }
        panic!("nothing painted");
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!((actual - expected).abs() <= 3.0, "{actual} vs {expected}");
    }

    #[test]
    fn the_fit_all_view_spreads_the_envelope_over_the_lane() {
        let p = timeline_envelope_painter(spiked(500), TIMELINE, 0, TIMELINE, Color::WHITE);
        assert_close(spike_x(&p, LANE), 300.0);
    }

    #[test]
    fn a_view_reaching_past_the_timeline_end_keeps_peaks_on_time() {
        // Recording on past the end: the fit-all view now spans 30 s.
        let p = timeline_envelope_painter(spiked(500), TIMELINE, 0, 30000, Color::WHITE);
        assert_close(spike_x(&p, LANE), 10000.0 / 30000.0 * 600.0);
    }

    #[test]
    fn a_zoomed_view_paged_past_the_end_keeps_peaks_on_time() {
        let p = timeline_envelope_painter(spiked(850), TIMELINE, 15000, 10000, Color::WHITE);
        // Bucket 850 starts at 17 000 ms: 2 s into the 10 s window.
        assert_close(spike_x(&p, LANE), 2000.0 / 10000.0 * 600.0);
    }

    #[test]
    fn a_view_wholly_past_the_timeline_draws_nothing() {
        let p = timeline_envelope_painter(spiked(500), TIMELINE, 25000, 10000, Color::WHITE);
        assert!(p.shape(LANE).is_none());
    }
}
