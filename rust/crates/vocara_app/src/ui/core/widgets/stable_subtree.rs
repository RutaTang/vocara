//! Port of `ui/core/widgets/stable_subtree.dart` — as a documented no-op.
//!
//! `StableSubtree` kept showing the child instance it had adopted while a
//! comparator said a new one showed the same thing, so Flutter skipped that
//! subtree's rebuild, relayout and repaint: the song screen rebuilt from one
//! view-model notification, and its live edits (a fader, pan-knob or FX tick,
//! a clip drag) notified at pointer rate while changing one lane or strip.
//!
//! That is a Flutter element-tree optimization with no iced counterpart worth
//! porting: an iced view is rebuilt from state on every update by design, its
//! layout is recomputed only when the view changed, and the expensive
//! drawing (waveforms, spectrograms) is cached per canvas by its inputs (see
//! `waveform_painter` and `spectrogram_painter`), which is what kept repaints
//! local in Flutter. So [`stable_subtree`] returns its child unchanged —
//! exactly what Dart's `debugNeverKeep` oracle showed, the "freshly built"
//! screen the kept one had to match.

use iced::Element;

/// `StableSubtree(child: child, sameAs: ...)`: `child`, always fresh.
pub fn stable_subtree<'a, M>(child: impl Into<Element<'a, M>>) -> Element<'a, M> {
    child.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::widget::text;

    // Dart's `debugNeverKeep builds every child afresh`: what a kept subtree
    // shows must be what a fresh build shows. (The other two Dart tests
    // checked the keeping itself — a stale label shown while the comparator
    // held — which has no counterpart: nothing is kept.)
    #[test]
    fn shows_exactly_what_a_fresh_build_shows() {
        for label in ["a 1", "b 1"] {
            let mut ui = iced_test::simulator::<(), _, _>(stable_subtree(text(label)));
            assert!(ui.find(label).is_ok());
        }
    }
}
