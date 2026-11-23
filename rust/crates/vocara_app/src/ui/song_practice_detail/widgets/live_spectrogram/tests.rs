//! The live spectrogram's tests: what it shows for each state of the
//! monitor, and that it remakes its image only when the monitor moved.
//! (`live_columns_raster_test.dart`, the raster's own test, is ported with
//! the raster in `spectrogram_painter`.) Rendered through `iced_test`'s
//! headless renderer; the raster helpers are shared with the voiceprint
//! tabs' tests.

use std::f64::consts::PI;
use std::io::BufReader;

use iced::{Settings, Size};
use iced_test::Simulator;

use super::*;
use crate::ui::core::theme::app_theme;

/// A rendered frame: its physical size and RGBA bytes.
pub(crate) struct Raster {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

impl Raster {
    /// The pixels as `[r, g, b, a]`.
    pub fn pixels(&self) -> impl Iterator<Item = [u8; 4]> + '_ {
        self.rgba.as_chunks::<4>().0.iter().copied()
    }

    /// Pixel (`x`, `y`) in physical pixels.
    pub fn at(&self, x: usize, y: usize) -> [u8; 4] {
        let i = (y * self.width + x) * 4;
        [
            self.rgba[i],
            self.rgba[i + 1],
            self.rgba[i + 2],
            self.rgba[i + 3],
        ]
    }
}

/// Draws `ui` and reads its pixels back (a first image match writes the
/// frame as a PNG, named after the renderer).
pub(crate) fn raster<M>(ui: &mut Simulator<'_, M>) -> Raster {
    let dir = tempfile::tempdir().expect("temp dir");
    let snapshot = ui.snapshot(&app_theme()).expect("snapshot");
    assert!(snapshot
        .matches_image(dir.path().join("frame.png"))
        .expect("write"));
    let file = std::fs::read_dir(dir.path())
        .expect("list")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "png"))
        .expect("png written");
    let decoder = png::Decoder::new(BufReader::new(std::fs::File::open(file).expect("open")));
    let mut reader = decoder.read_info().expect("png header");
    let mut rgba = vec![0; reader.output_buffer_size().expect("size")];
    let info = reader.next_frame(&mut rgba).expect("png frame");
    rgba.truncate(info.buffer_size());
    Raster {
        width: info.width as usize,
        height: info.height as usize,
        rgba,
    }
}

/// A 0.3-amplitude sine (`_sine` of the Dart tests).
pub(crate) fn sine(n: usize, f: f64, sr: f64) -> Vec<f32> {
    (0..n)
        .map(|i| (0.3 * (2.0 * PI * f * i as f64 / sr).sin()) as f32)
        .collect()
}

/// A started monitor fed `samples` in 512-sample chunks, the way the engine
/// delivers them (`_live`).
pub(crate) fn fed(samples: &[f32]) -> MonitorService {
    let monitor = MonitorService::default();
    monitor.start();
    for chunk in samples.chunks(512) {
        monitor.feed_samples(chunk);
    }
    monitor
}

const BOX: Size = Size {
    width: 600.0,
    height: 200.0,
};

fn simulator(monitor: &MonitorService) -> Simulator<'_, ()> {
    Simulator::with_size(Settings::default(), BOX, live_spectrogram::<()>(monitor))
}

/// Whether the frame shows a voiced ridge: the heat map's coral, amber or
/// white, far brighter than the well, the grid or the hint.
fn shows_a_ridge(r: &Raster) -> bool {
    r.pixels()
        .any(|[red, green, _, _]| red > 180 && green > 100)
}

fn rebuilds() -> usize {
    IMAGE_REBUILDS.with(std::cell::Cell::get)
}

#[test]
fn a_monitor_without_columns_shows_the_hint_over_the_well() {
    let idle = MonitorService::default();
    let started = MonitorService::default();
    started.start();
    for monitor in [&idle, &started] {
        let mut ui = simulator(monitor);
        assert!(ui.find(LIVE_SPECTROGRAM_HINT).is_ok());
        let r = raster(&mut ui);
        assert!(!shows_a_ridge(&r));
        // The well's inset colour away from the grid lines and the hint.
        let [red, green, blue, _] = r.at(4, r.height - 4);
        assert_eq!([red, green, blue], [0x0E, 0x0E, 0x12]);
    }
}

#[test]
fn a_fed_monitor_shows_its_window_and_no_hint() {
    let monitor = fed(&sine(24000, 220.0, 48000.0));
    let mut ui = simulator(&monitor);
    assert!(ui.find(LIVE_SPECTROGRAM_HINT).is_err());
    let r = raster(&mut ui);
    assert!(shows_a_ridge(&r), "the 220 Hz ridge is drawn");
    // The window is stretched over the box: its newest column reaches the
    // right edge, on the ridge's row (220 Hz of 5 kHz).
    let ridge_row = ((1.0 - 220.0 / 5000.0) * r.height as f64) as usize;
    let bright = |x: usize| {
        (ridge_row.saturating_sub(6)..(ridge_row + 6).min(r.height)).any(|y| r.at(x, y)[0] > 180)
    };
    assert!(bright(r.width - 2));
}

#[test]
fn the_image_is_remade_only_when_the_monitor_moves() {
    let monitor = fed(&sine(24000, 220.0, 48000.0));
    let mut ui = simulator(&monitor);
    let start = rebuilds();
    ui.snapshot(&app_theme()).expect("draws");
    assert_eq!(rebuilds(), start + 1, "the first frame makes the image");
    ui.snapshot(&app_theme()).expect("draws");
    ui.snapshot(&app_theme()).expect("draws");
    assert_eq!(rebuilds(), start + 1, "frames of a still monitor reuse it");
    // A chunk arrives (Dart's listener fired): the next frame remakes it.
    monitor.feed_samples(&sine(2048, 220.0, 48000.0));
    ui.snapshot(&app_theme()).expect("draws");
    assert_eq!(rebuilds(), start + 2);
    ui.snapshot(&app_theme()).expect("draws");
    assert_eq!(rebuilds(), start + 2);
}

#[test]
fn a_monitor_restarted_with_no_columns_drops_its_image() {
    let monitor = fed(&sine(24000, 220.0, 48000.0));
    let mut ui = simulator(&monitor);
    assert!(shows_a_ridge(&raster(&mut ui)));
    // A new session clears the window: the image on screen must go with it,
    // not stay frozen over the empty well.
    monitor.stop();
    monitor.start();
    assert!(!shows_a_ridge(&raster(&mut ui)));
    // And the view built from it says why.
    assert!(simulator(&monitor).find(LIVE_SPECTROGRAM_HINT).is_ok());
}

#[test]
fn a_monitor_stopped_keeps_its_picture() {
    // The rolling state stays for the views until the next start.
    let monitor = fed(&sine(24000, 220.0, 48000.0));
    let mut ui = simulator(&monitor);
    assert!(shows_a_ridge(&raster(&mut ui)));
    monitor.stop();
    assert!(shows_a_ridge(&raster(&mut ui)));
    assert!(simulator(&monitor).find(LIVE_SPECTROGRAM_HINT).is_err());
}
