//! The Material icons media_kit's desktop video controls drew (Flutter's
//! `Icons.*` from the `MaterialIcons` font, and `AnimatedIcons.play_pause`),
//! as paths. The app's own icons are Lucide (`icons`); these are drawn as
//! the Flutter widget drew them, so they look like Material's.
//!
//! - [`MaterialIcon`]: `volume_up`, `volume_down`, `volume_off`,
//!   `fullscreen` and `fullscreen_exit`, each the outline of the filled
//!   ("baseline") glyph from Google's Material Icons
//!   (google/material-design-icons, `src/<group>/<name>/materialicons/
//!   24px.svg`, Apache License 2.0, listed on the license page), on its
//!   24-unit grid: the source of the glyphs in Flutter's `MaterialIcons`
//!   font, which matches them to within its 1/512-em grid. An `Icon` filled
//!   its `size` square with the glyph's em, so the grid scales to the size.
//! - [`play_pause`]: `AnimatedIcons.play_pause` (Flutter 3.38's
//!   `animated_icons/data/play_pause.g.dart`, BSD-3-Clause, listed on the
//!   license page): three paths on a 48-unit grid whose every point moves
//!   through 16 keyframes, interpolated linearly at the animation's progress
//!   (`_interpolate`), each path filled at its own interpolated opacity
//!   (`_AnimatedIconPainter`). At 0 it is the play triangle, at 1 the pause
//!   bars.

use iced::widget::canvas::Path;
use iced::Point;

mod play_pause_data;

use play_pause_data::{PathFrames, PLAY_PAUSE};

/// A Material icon the video controls draw (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MaterialIcon {
    VolumeUp,
    VolumeDown,
    VolumeOff,
    Fullscreen,
    FullscreenExit,
}

impl MaterialIcon {
    /// The icon's outline filling a `size`-px square at `origin`.
    pub fn path(self, origin: Point, size: f32) -> Path {
        let scale = size / 24.0;
        let p = |x: f32, y: f32| Point::new(origin.x + x * scale, origin.y + y * scale);
        Path::new(|b| match self {
            // M3 9v6h4l5 5V4L7 9H3zm13.5 3c0-1.77-1.02-3.29-2.5-4.03v8.05
            // c1.48-.73 2.5-2.25 2.5-4.02zM14 3.23v2.06c2.89.86 5 3.54 5 6.71
            // s-2.11 5.85-5 6.71v2.06c4.01-.91 7-4.49 7-8.77s-2.99-7.86-7-8.77z
            MaterialIcon::VolumeUp => {
                b.move_to(p(3.0, 9.0));
                b.line_to(p(3.0, 15.0));
                b.line_to(p(7.0, 15.0));
                b.line_to(p(12.0, 20.0));
                b.line_to(p(12.0, 4.0));
                b.line_to(p(7.0, 9.0));
                b.line_to(p(3.0, 9.0));
                b.close();
                b.move_to(p(16.5, 12.0));
                b.bezier_curve_to(p(16.5, 10.23), p(15.48, 8.71), p(14.0, 7.97));
                b.line_to(p(14.0, 16.02));
                b.bezier_curve_to(p(15.48, 15.29), p(16.5, 13.77), p(16.5, 12.0));
                b.close();
                b.move_to(p(14.0, 3.23));
                b.line_to(p(14.0, 5.29));
                b.bezier_curve_to(p(16.89, 6.15), p(19.0, 8.83), p(19.0, 12.0));
                b.bezier_curve_to(p(19.0, 15.17), p(16.89, 17.85), p(14.0, 18.71));
                b.line_to(p(14.0, 20.77));
                b.bezier_curve_to(p(18.01, 19.86), p(21.0, 16.28), p(21.0, 12.0));
                b.bezier_curve_to(p(21.0, 7.72), p(18.01, 4.14), p(14.0, 3.23));
                b.close();
            }
            // M18.5 12c0-1.77-1.02-3.29-2.5-4.03v8.05c1.48-.73 2.5-2.25
            // 2.5-4.02zM5 9v6h4l5 5V4L9 9H5z
            MaterialIcon::VolumeDown => {
                b.move_to(p(18.5, 12.0));
                b.bezier_curve_to(p(18.5, 10.23), p(17.48, 8.71), p(16.0, 7.97));
                b.line_to(p(16.0, 16.02));
                b.bezier_curve_to(p(17.48, 15.29), p(18.5, 13.77), p(18.5, 12.0));
                b.close();
                b.move_to(p(5.0, 9.0));
                b.line_to(p(5.0, 15.0));
                b.line_to(p(9.0, 15.0));
                b.line_to(p(14.0, 20.0));
                b.line_to(p(14.0, 4.0));
                b.line_to(p(9.0, 9.0));
                b.line_to(p(5.0, 9.0));
                b.close();
            }
            // M16.5 12c0-1.77-1.02-3.29-2.5-4.03v2.21l2.45 2.45
            // c.03-.2.05-.41.05-.63zm2.5 0c0 .94-.2 1.82-.54 2.64l1.51 1.51
            // C20.63 14.91 21 13.5 21 12c0-4.28-2.99-7.86-7-8.77v2.06
            // c2.89.86 5 3.54 5 6.71zM4.27 3L3 4.27 7.73 9H3v6h4l5 5v-6.73
            // l4.25 4.25c-.67.52-1.42.93-2.25 1.18v2.06c1.38-.31 2.63-.95
            // 3.69-1.81L19.73 21 21 19.73l-9-9L4.27 3zM12 4L9.91 6.09 12 8.18
            // V4z
            MaterialIcon::VolumeOff => {
                b.move_to(p(16.5, 12.0));
                b.bezier_curve_to(p(16.5, 10.23), p(15.48, 8.71), p(14.0, 7.97));
                b.line_to(p(14.0, 10.18));
                b.line_to(p(16.45, 12.63));
                b.bezier_curve_to(p(16.48, 12.43), p(16.5, 12.22), p(16.5, 12.0));
                b.close();
                b.move_to(p(19.0, 12.0));
                b.bezier_curve_to(p(19.0, 12.94), p(18.8, 13.82), p(18.46, 14.64));
                b.line_to(p(19.97, 16.15));
                b.bezier_curve_to(p(20.63, 14.91), p(21.0, 13.5), p(21.0, 12.0));
                b.bezier_curve_to(p(21.0, 7.72), p(18.01, 4.14), p(14.0, 3.23));
                b.line_to(p(14.0, 5.29));
                b.bezier_curve_to(p(16.89, 6.15), p(19.0, 8.83), p(19.0, 12.0));
                b.close();
                b.move_to(p(4.27, 3.0));
                b.line_to(p(3.0, 4.27));
                b.line_to(p(7.73, 9.0));
                b.line_to(p(3.0, 9.0));
                b.line_to(p(3.0, 15.0));
                b.line_to(p(7.0, 15.0));
                b.line_to(p(12.0, 20.0));
                b.line_to(p(12.0, 13.27));
                b.line_to(p(16.25, 17.52));
                b.bezier_curve_to(p(15.58, 18.04), p(14.83, 18.45), p(14.0, 18.7));
                b.line_to(p(14.0, 20.76));
                b.bezier_curve_to(p(15.38, 20.45), p(16.63, 19.81), p(17.69, 18.95));
                b.line_to(p(19.73, 21.0));
                b.line_to(p(21.0, 19.73));
                b.line_to(p(12.0, 10.73));
                b.line_to(p(4.27, 3.0));
                b.close();
                b.move_to(p(12.0, 4.0));
                b.line_to(p(9.91, 6.09));
                b.line_to(p(12.0, 8.18));
                b.line_to(p(12.0, 4.0));
                b.close();
            }
            // M7 14H5v5h5v-2H7v-3zm-2-4h2V7h3V5H5v5zm12 7h-3v2h5v-5h-2v3z
            // M14 5v2h3v3h2V5h-5z
            MaterialIcon::Fullscreen => {
                const CORNERS: [[(f32, f32); 6]; 4] = [
                    [
                        (7.0, 14.0),
                        (5.0, 14.0),
                        (5.0, 19.0),
                        (10.0, 19.0),
                        (10.0, 17.0),
                        (7.0, 17.0),
                    ],
                    [
                        (5.0, 10.0),
                        (7.0, 10.0),
                        (7.0, 7.0),
                        (10.0, 7.0),
                        (10.0, 5.0),
                        (5.0, 5.0),
                    ],
                    [
                        (17.0, 17.0),
                        (14.0, 17.0),
                        (14.0, 19.0),
                        (19.0, 19.0),
                        (19.0, 14.0),
                        (17.0, 14.0),
                    ],
                    [
                        (14.0, 5.0),
                        (14.0, 7.0),
                        (17.0, 7.0),
                        (17.0, 10.0),
                        (19.0, 10.0),
                        (19.0, 5.0),
                    ],
                ];
                for corner in &CORNERS {
                    polygon(b, &p, corner);
                }
            }
            // M5 16h3v3h2v-5H5v2zm3-8H5v2h5V5H8v3zm6 11h2v-3h3v-2h-5v5z
            // m2-11V5h-2v5h5V8h-3z
            MaterialIcon::FullscreenExit => {
                const CORNERS: [[(f32, f32); 6]; 4] = [
                    [
                        (5.0, 16.0),
                        (8.0, 16.0),
                        (8.0, 19.0),
                        (10.0, 19.0),
                        (10.0, 14.0),
                        (5.0, 14.0),
                    ],
                    [
                        (8.0, 8.0),
                        (5.0, 8.0),
                        (5.0, 10.0),
                        (10.0, 10.0),
                        (10.0, 5.0),
                        (8.0, 5.0),
                    ],
                    [
                        (14.0, 19.0),
                        (16.0, 19.0),
                        (16.0, 16.0),
                        (19.0, 16.0),
                        (19.0, 14.0),
                        (14.0, 14.0),
                    ],
                    [
                        (16.0, 8.0),
                        (16.0, 5.0),
                        (14.0, 5.0),
                        (14.0, 10.0),
                        (19.0, 10.0),
                        (19.0, 8.0),
                    ],
                ];
                for corner in &CORNERS {
                    polygon(b, &p, corner);
                }
            }
        })
    }
}

/// A closed polygon through `points` (an SVG run of `h`/`v` edges, made
/// absolute).
fn polygon(
    b: &mut iced::widget::canvas::path::Builder,
    p: &impl Fn(f32, f32) -> Point,
    points: &[(f32, f32)],
) {
    let Some(&(x, y)) = points.first() else {
        return;
    };
    b.move_to(p(x, y));
    for &(x, y) in &points[1..] {
        b.line_to(p(x, y));
    }
    b.close();
}

/// The side of `AnimatedIcons.play_pause`'s grid.
const ANIMATED_GRID: f64 = 48.0;

/// `_interpolate`: the value `progress` of the way through keyframes
/// spaced evenly in time, linear between the two around it.
fn interpolate<const N: usize>(frames: &[[f64; 2]; N], progress: f64) -> [f64; 2] {
    let target = progress * (N - 1) as f64;
    let low = target.floor() as usize;
    let high = (target.ceil() as usize).min(N - 1);
    let t = target - low as f64;
    let (a, b) = (frames[low.min(N - 1)], frames[high]);
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
}

/// `_interpolate` for a path's opacities.
fn interpolate_opacity(opacities: &[f64; 16], progress: f64) -> f64 {
    let target = progress * 15.0;
    let low = target.floor() as usize;
    let high = (target.ceil() as usize).min(15);
    let t = target - low as f64;
    let a = opacities[low.min(15)];
    a + (opacities[high] - a) * t
}

/// `AnimatedIcons.play_pause` at `progress` (clamped to 0–1: 0 is play, 1
/// pause) filling a `size`-px square at `origin`: each of its paths, with
/// the opacity to fill it at.
pub fn play_pause(progress: f32, origin: Point, size: f32) -> Vec<(Path, f32)> {
    let progress = f64::from(progress.clamp(0.0, 1.0));
    let scale = f64::from(size) / ANIMATED_GRID;
    let at = |frames: &[[f64; 2]; 16]| {
        let [x, y] = interpolate(frames, progress);
        Point::new(origin.x + (x * scale) as f32, origin.y + (y * scale) as f32)
    };
    PLAY_PAUSE
        .iter()
        .map(|frames: &PathFrames| {
            let path = Path::new(|b| {
                b.move_to(at(&frames.move_to));
                for [c1, c2, to] in &frames.cubics {
                    b.bezier_curve_to(at(c1), at(c2), at(to));
                }
                b.close();
            });
            (
                path,
                interpolate_opacity(&frames.opacities, progress) as f32,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyframes_are_interpolated_linearly_between_their_neighbours() {
        let mut frames = [[0.0; 2]; 16];
        for (i, f) in frames.iter_mut().enumerate() {
            *f = [i as f64, 2.0 * i as f64];
        }
        assert_eq!(interpolate(&frames, 0.0), [0.0, 0.0]);
        assert_eq!(interpolate(&frames, 1.0), [15.0, 30.0]);
        // Halfway between keyframes 7 and 8.
        assert_eq!(interpolate(&frames, 7.5 / 15.0), [7.5, 15.0]);
    }

    #[test]
    fn play_pause_runs_from_the_triangle_to_the_bars() {
        // The first path's start moves from the triangle's top-left corner
        // to the right bar's.
        assert_eq!(PLAY_PAUSE[0].move_to[0], [16.046875, 10.039062500000002]);
        assert_eq!(PLAY_PAUSE[0].move_to[15], [36.0, 10.0]);
        // Two paths stay opaque; the third fades out a quarter of the way.
        let opacities = |progress: f32| -> Vec<f32> {
            play_pause(progress, Point::ORIGIN, 48.0)
                .into_iter()
                .map(|(_, o)| o)
                .collect()
        };
        assert_eq!(opacities(0.0), vec![1.0, 1.0, 1.0]);
        let at_keyframe_4 = opacities(4.0 / 15.0);
        assert!(
            (at_keyframe_4[2] - 0.733_333).abs() < 1e-4,
            "{at_keyframe_4:?}"
        );
        assert_eq!(opacities(1.0), vec![1.0, 1.0, 0.0]);
        // Halfway between keyframes 3 (opaque) and 4.
        let between = opacities(3.5 / 15.0);
        assert!((between[2] - (1.0 + 0.733_333) / 2.0).abs() < 1e-4);
    }
}
