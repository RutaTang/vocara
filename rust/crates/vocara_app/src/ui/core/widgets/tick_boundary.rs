//! Port of `ui/core/widgets/tick_boundary.dart` — as a documented no-op.
//!
//! `TickBoundary` kept a subtree that rebuilt on a fast tick (a live take's
//! waveform, an input meter) from re-laying-out and repainting what
//! surrounded it: a `RepaintBoundary` over a `LayoutBuilder`, because under
//! the timeline's LayoutBuilders a 20 Hz recording tick otherwise repainted
//! the whole screen.
//!
//! iced has no relayout chain to cut: a canvas redraws itself from its
//! program's state, and a `canvas::Cache` (used by the painters in
//! `waveform_painter` / `spectrogram_painter`) is cleared only when its own
//! inputs change, so a tick re-tessellates the one canvas it feeds and
//! nothing around it. [`tick_boundary`] therefore returns its child
//! unchanged; mount the ticking canvas where its size is fixed, as the Dart
//! note asked, and the effect is the same.

use iced::Element;

/// `TickBoundary(child: child)`: `child`.
pub fn tick_boundary<'a, M>(child: impl Into<Element<'a, M>>) -> Element<'a, M> {
    child.into()
}
